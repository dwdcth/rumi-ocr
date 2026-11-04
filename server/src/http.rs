//! axum 0.8 路由与生命周期。
//!
//! 路由（Umi 兼容子集，M3 §5）：
//! - `GET /`、`GET /api`：健康与状态 JSON
//! - `POST /api/ocr`：body `{"base64": ...}` 或 `{"path": ...}` →
//!   成功 `{"code":100,"data":[{"text","box","score"}],"count":N,"ignored":M}`，
//!   失败 `{"code":102,"data":"<错误>"}`（HTTP 200）
//!
//! 扩展参数（P2 #11/#12/#14，Rumi 侧新增，Umi 客户端忽略未知字段不受影响）：
//! - `layout`：排版解析，`"by_line"`（默认）| `"merge_para"` | `"natural_para"`；
//!   非法值报 `{code:102}`
//! - `rule`：`layout` 的别名（Umi 原生参数名）；两者同传且不一致报 `{code:102}`，
//!   一致或只传其一则正常生效
//! - `ignore`：正则字符串数组，任一命中（整行 match）的识别行被剔除
//!
//! 成功响应在存在**编译失败的忽略正则**时附 `"bad_regex": ["原正则", ...]`
//! （Wave1-B 遗留收尾；空数组省略字段以保持对旧客户端的响应形状兼容）。
//! PDF 文件路径（`.pdf`，不分大小写）走 `rumi_ocr::load_pdf` 分页识别后跨页合并。
//!
//! `count` = 过滤/合并后的输出条数（= data 长度）；`ignored` = 被忽略规则剔除的行数。
//!
//! 鉴权（可选，默认关）：配置 token 后，请求须带 `?token=<t>` 或
//! `Authorization: Bearer <t>`，否则 401 + `{code:102}`。
//!
//! 翻译扩展路由（Rumi 新增面，非 Umi 协议；核心逻辑在 rumi-translate）：
//! - `POST /api/translate`：body `{"text"|"texts","from","to","engine","polish",...}` →
//!   成功 `{"texts":[...],"engine":"...","elapsed_ms":N}`；
//!   失败按性质给真实状态码 400（参数/引擎名/离线模型缺失）/502（在线网络），
//!   body `{"error":"<原因>"}`（不复用 Umi 的 code:102——那是对 Umi 客户端的兼容形状，
//!   翻译端点无兼容包袱，规格要求状态码区分错误性质）
//! - `GET /api/translate/models`：内置模型清单 + 安装状态 + 导入包
//! - `POST /api/translate/models/install`：body `{"key":"en_zh"}` 同步装完才返回
//!   （大文件分钟级，客户端请自设超时）

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::config::default_config_path;
use crate::dispatch::{OcrInput, OcrOptions, Recognizer};
use rumi_ocr::postprocess::Layout;

/// 共享状态：识别器（内部自持配置热加载与插件缓存）。
#[derive(Clone)]
pub struct AppState {
    pub rec: Arc<Recognizer>,
}

pub fn build_router(rec: Arc<Recognizer>) -> Router {
    Router::new()
        .route("/", get(status))
        .route("/api", get(status))
        .route("/api/ocr", post(api_ocr))
        .route("/api/translate", post(api_translate))
        .route("/api/translate/models", get(api_translate_models))
        .route(
            "/api/translate/models/install",
            post(api_translate_models_install),
        )
        .with_state(AppState { rec })
}

/// `GET /`：健康与状态。
async fn status(State(st): State<AppState>) -> Json<Value> {
    let cfg = st.rec.config();
    Json(json!({
        "status": "ok",
        "app": "Rumi",
        "version": env!("CARGO_PKG_VERSION"),
        "engine": st.rec.engine_now().label(),
        "port": cfg.port(),
        "token_auth": cfg.token().is_some(),
        "uptime_s": st.rec.started.elapsed().as_secs(),
    }))
}

/// `POST /api/ocr` 请求体。
#[derive(serde::Deserialize, Default)]
struct OcrBody {
    base64: Option<String>,
    path: Option<String>,
    /// 排版解析："by_line"（默认）| "merge_para" | "natural_para"
    layout: Option<String>,
    /// `layout` 的别名（Umi 原生参数名）；与 layout 同传且不一致时报 102
    rule: Option<String>,
    /// 忽略规则：任一正则整行命中的行被剔除
    #[serde(default)]
    ignore: Vec<String>,
}

/// layout 字符串 → 排版策略；未知值返回 None（HTTP 层报 102）。
fn parse_layout(s: &str) -> Option<Layout> {
    match s {
        "by_line" => Some(Layout::ByLine),
        "merge_para" => Some(Layout::MergePara),
        "natural_para" => Some(Layout::NaturalPara),
        _ => None,
    }
}

/// `POST /api/ocr`：解析 body → spawn_blocking 识别 → Umi 格式 JSON。
///
/// 任何失败（坏 JSON / 缺参数 / 文件不存在 / 引擎报错）都返回 HTTP 200 +
/// `{"code":102,"data":"<原因>"}`，对齐 Umi 的错误形状；
/// 仅鉴权失败返回 401。
async fn api_ocr(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 可选鉴权：query token 优先，其次 Authorization: Bearer
    if let Some(expect) = st.rec.config().token() {
        let given = q
            .get("token")
            .map(|s| s.as_str())
            .or_else(|| {
                headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.strip_prefix("Bearer "))
            });
        if given != Some(expect) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "code": 102, "data": "token 校验失败" })),
            )
                .into_response();
        }
    }

    let parsed: OcrBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return umi_error(format!("请求体不是合法 JSON：{e}")),
    };
    let input = if let Some(b64) = parsed.base64.filter(|s| !s.trim().is_empty()) {
        OcrInput::Base64(b64)
    } else if let Some(p) = parsed.path.filter(|s| !s.trim().is_empty()) {
        OcrInput::Path(p)
    } else {
        return umi_error("缺少参数：需要 base64 或 path 之一");
    };

    // 排版解析参数：`layout`（Rumi 命名）/ `rule`（Umi 原生别名）。
    // 省略/空串 = 按行（默认）；两者同传且不一致 → code 102（不做静默择一）。
    let layout_s = parsed.layout.as_deref().filter(|s| !s.is_empty());
    let rule_s = parsed.rule.as_deref().filter(|s| !s.is_empty());
    let (chosen, origin) = match (layout_s, rule_s) {
        (Some(l), Some(r)) if l != r => {
            return umi_error(format!(
                "layout 与 rule 参数冲突：{l:?} ≠ {r:?}（rule 是 layout 的别名，二者只能传其一或保持一致）"
            ));
        }
        (Some(l), _) => (l, "layout"),
        (None, Some(r)) => (r, "rule"),
        (None, None) => ("", "layout"),
    };
    let layout = if chosen.is_empty() {
        Layout::ByLine
    } else {
        match parse_layout(chosen) {
            Some(l) => l,
            None => {
                return umi_error(format!(
                    "非法 {origin}：{chosen}（可选 by_line / merge_para / natural_para）"
                ));
            }
        }
    };
    let opts = OcrOptions { layout, ignore: parsed.ignore };

    // 识别是阻塞的（ONNX 推理 / 子进程），丢进阻塞线程池
    let rec = st.rec.clone();
    match tokio::task::spawn_blocking(move || rec.recognize_opts(input, &opts)).await {
        Ok(Ok(outcome)) => {
            let count = outcome.lines.len();
            // Umi 兼容基座；扩展字段只在非默认时附加，旧客户端忽略未知字段不受影响
            let mut body = json!({
                "code": 100,
                "data": outcome.lines,
                "count": count,
                "ignored": outcome.ignored,
            });
            // 编译失败的忽略正则原样上报（Wave1-B 遗留收尾）；空时省略字段保持兼容
            if !outcome.bad_regex.is_empty() {
                body["bad_regex"] = json!(outcome.bad_regex);
            }
            Json(body).into_response()
        }
        Ok(Err(msg)) => umi_error(msg),
        Err(e) => umi_error(format!("识别任务失败：{e}")),
    }
}

/// Umi 兼容错误响应：HTTP 200 + `{"code":102,"data":"<原因>"}`。
fn umi_error(msg: impl Into<String>) -> Response {
    (StatusCode::OK, Json(json!({ "code": 102, "data": msg.into() }))).into_response()
}

// ---------------------------------------------------------------------------
// 翻译 API（规格 E）：POST /api/translate + GET models + POST models/install。
// 全部走 rumi-translate 同步 API，HTTP 层 spawn_blocking（CT2 推理 / 目录扫描 /
// 模型下载都是阻塞活）。错误用真实状态码 + {"error":...}，理由见模块头注释。
// ---------------------------------------------------------------------------

/// `POST /api/translate` 请求体。text 与 texts 二选一（同给 400）；
/// from/to 缺省 en→zh，engine 缺省 offline，polish 缺省 true（仅 to=zh 生效）。
/// 在线引擎参数按 engine 取用，缺省不前置校验（缺 key/url 交 provider 报错 → 502），
/// 与 UI「空串回退默认值」的语义一致（deeplx 空 url 回退 127.0.0.1:1188 等）。
#[derive(serde::Deserialize, Default)]
struct TranslateBody {
    text: Option<String>,
    texts: Option<Vec<String>>,
    from: Option<String>,
    to: Option<String>,
    engine: Option<String>,
    polish: Option<bool>,
    deepl_api_key: Option<String>,
    deepl_pro: Option<bool>,
    deeplx_url: Option<String>,
    openai_base_url: Option<String>,
    openai_api_key: Option<String>,
    openai_model: Option<String>,
}

/// engine 名 → [`rumi_translate::Engine`]；未知名返回 None（HTTP 层报 400）。
fn parse_engine(name: &str, p: &TranslateBody) -> Option<rumi_translate::Engine> {
    use rumi_translate::Engine;
    match name {
        "offline" => Some(Engine::Offline),
        "google" => Some(Engine::Google),
        "microsoft" => Some(Engine::Microsoft),
        "deepl" => Some(Engine::DeepL {
            api_key: p.deepl_api_key.clone().unwrap_or_default(),
            pro: p.deepl_pro.unwrap_or(false),
        }),
        "deeplx" => Some(Engine::DeepLX {
            url: p.deeplx_url.clone().unwrap_or_default(),
        }),
        "openai" => Some(Engine::OpenAI {
            base_url: p.openai_base_url.clone().unwrap_or_default(),
            api_key: p.openai_api_key.clone().unwrap_or_default(),
            model: p.openai_model.clone().unwrap_or_default(),
            // 术语表 v1 为 UI 设置面功能；HTTP 请求体不带 glossary（空=不注入）
            glossary: Vec::new(),
        }),
        _ => None,
    }
}

/// 翻译 API 错误响应：`<status>` + `{"error":"<原因>"}`。
/// 与 /api/ocr 的 Umi 形状（HTTP 200 + code:102）刻意不同——翻译端点是
/// Rumi 自有面，规格要求用状态码区分错误性质（400 参数/离线缺失、502 在线网络）。
fn translate_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

/// `POST /api/translate`：统一翻译入口（离线模型走 engine::shared() 懒加载）。
async fn api_translate(body: Bytes) -> Response {
    let parsed: TranslateBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return translate_error(StatusCode::BAD_REQUEST, format!("请求体不是合法 JSON：{e}")),
    };

    // text / texts 二选一：同给不做静默择一（与 layout/rule 冲突报 102 同思路）。
    // 按引用匹配（不 move 字段）：parsed 后面还要整体借给 parse_engine
    let texts = match (&parsed.text, &parsed.texts) {
        (Some(_), Some(_)) => {
            return translate_error(
                StatusCode::BAD_REQUEST,
                "text 与 texts 只能传其一（二选一）",
            );
        }
        (Some(t), None) => vec![t.clone()],
        (None, Some(v)) => v.clone(),
        (None, None) => {
            return translate_error(StatusCode::BAD_REQUEST, "缺少参数：需要 text 或 texts 之一");
        }
    };

    let engine_name = parsed.engine.clone().unwrap_or_else(|| "offline".to_string());
    let Some(engine) = parse_engine(&engine_name, &parsed) else {
        return translate_error(
            StatusCode::BAD_REQUEST,
            format!("未知引擎：{engine_name}（可选 offline / google / microsoft / deepl / deeplx / openai）"),
        );
    };

    // 空输入短路：不进阻塞池，elapsed 恒 0（规格 §3）
    if texts.is_empty() {
        return Json(json!({ "texts": [], "engine": engine_name, "elapsed_ms": 0 })).into_response();
    }

    let from = parsed.from.unwrap_or_else(|| "en".to_string());
    let to = parsed.to.unwrap_or_else(|| "zh".to_string());
    let polish = parsed.polish.unwrap_or(true);

    // 翻译是阻塞的（CT2 推理 / 在线 HTTP），丢进阻塞线程池（api_ocr 同款）
    let started = std::time::Instant::now();
    let offline = engine_name == "offline";
    match tokio::task::spawn_blocking(move || {
        rumi_translate::translate(&texts, &from, &to, &engine, polish)
    })
    .await
    {
        Ok(Ok(translated)) => Json(json!({
            "texts": translated,
            "engine": engine_name,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }))
        .into_response(),
        Ok(Err(msg)) => {
            // 离线失败（模型未安装/加载/推理）→ 400，文案透传 engine 错误 Display；
            // 在线失败（网络/鉴权/服务端错）→ 502（上游网关语义）
            if offline {
                translate_error(StatusCode::BAD_REQUEST, msg)
            } else {
                translate_error(StatusCode::BAD_GATEWAY, msg)
            }
        }
        Err(e) => translate_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("翻译任务失败：{e}"),
        ),
    }
}

/// `GET /api/translate/models`：内置模型清单（registry）+ 安装标志 + 导入包清单。
/// installed/imported 都来自 `engine::shared().installed()`（manifest 记账口径）：
/// key 命中 registry 的归 installed 标志，未命中的归 imported（用户导入的 Argos 包）。
/// InstalledInfo 未实现 Serialize（不改 translate crate），server 侧手工组 JSON。
async fn api_translate_models() -> Response {
    let installed = tokio::task::spawn_blocking(|| rumi_translate::engine::shared().installed())
        .await
        .unwrap_or_default();
    let models: Vec<Value> = rumi_translate::registry::MODELS
        .iter()
        .map(|m| {
            json!({
                "key": m.key,
                "from": m.from,
                "to": m.to,
                "size_mb": m.size_mb,
                "installed": installed.iter().any(|i| i.key == m.key),
            })
        })
        .collect();
    let imported: Vec<Value> = installed
        .iter()
        .filter(|i| !rumi_translate::registry::MODELS.iter().any(|m| m.key == i.key))
        .map(|i| json!({ "key": i.key, "from": i.from, "to": i.to, "bytes": i.bytes }))
        .collect();
    Json(json!({ "models": models, "imported": imported })).into_response()
}

/// `POST /api/translate/models/install` 请求体。
#[derive(serde::Deserialize)]
struct InstallBody {
    key: String,
}

/// `POST /api/translate/models/install`：按 key 同步安装内置模型（进度不上报）。
///
/// 大文件（~70MB 起，分钟级）装完才返回——**客户端请自设超时**；
/// 进度展示请走 UI（它有逐块回调），此端点面向脚本/无 UI 场景。
async fn api_translate_models_install(body: Bytes) -> Response {
    let parsed: InstallBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return translate_error(StatusCode::BAD_REQUEST, format!("请求体不是合法 JSON：{e}")),
    };
    // by_key 给 &'static ModelSpec；install 接收模型根目录、内部自行 join(key)
    let Some(spec) = rumi_translate::registry::by_key(&parsed.key) else {
        return translate_error(
            StatusCode::BAD_REQUEST,
            format!(
                "未知模型 key：{}（可选 {}）",
                parsed.key,
                rumi_translate::registry::MODELS
                    .iter()
                    .map(|m| m.key)
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        );
    };
    let mirrors = rumi_translate::download::Mirrors::default();
    let dir = rumi_translate::models_dir();
    match tokio::task::spawn_blocking(move || {
        rumi_translate::download::install(spec, &dir, &mirrors, &|_| {})
    })
    .await
    {
        Ok(Ok(_)) => Json(json!({ "ok": true, "key": parsed.key })).into_response(),
        Ok(Err(msg)) => translate_error(StatusCode::BAD_GATEWAY, msg),
        Err(e) => translate_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("安装任务失败：{e}"),
        ),
    }
}

/// 服务配置；[`Default`] = 127.0.0.1:1224 + 默认配置路径 + `python3`。
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: IpAddr,
    pub port: u16,
    pub config_path: PathBuf,
    pub plugins_dir: Option<PathBuf>,
    pub python: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 1224,
            config_path: default_config_path().unwrap_or_else(|| PathBuf::from("config.toml")),
            plugins_dir: None,
            python: PathBuf::from("python3"),
        }
    }
}

/// 启动 HTTP 服务：绑定成功即返回 `(实际监听地址, 句柄)`。
///
/// `port = 0` 时由系统分配随机端口（测试用），实际端口见返回的 [`SocketAddr`]。
/// 识别用的 runtime 即调用方当前 runtime（serve 必须 `.await` 在 tokio 上下文里）。
pub async fn serve(cfg: ServerConfig) -> std::io::Result<(SocketAddr, ServerHandle)> {
    let rec = Arc::new(Recognizer::new(cfg.config_path, cfg.plugins_dir, cfg.python));
    let app = build_router(rec);
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(cfg.host, cfg.port)).await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });
    Ok((addr, ServerHandle { shutdown: Some(tx), join }))
}

/// 服务句柄：shutdown 信号 + 任务 join。
pub struct ServerHandle {
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl ServerHandle {
    /// 发送优雅关闭信号（不等待退出）。
    pub fn signal_shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }

    /// 发送并等待服务任务结束。
    pub async fn shutdown(mut self) {
        self.signal_shutdown();
        let _ = self.join.await;
    }

    /// 发送关闭信号并取回底层任务句柄（想自行 await 时用）。
    pub fn into_join(mut self) -> JoinHandle<()> {
        self.signal_shutdown();
        self.join
    }
}
