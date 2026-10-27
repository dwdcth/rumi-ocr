//! 引擎分发：`engine = "native"` → rumi_ocr 进程内引擎；
//! `engine = "plugin:<名>"` → plugins-worker 的 Umi 插件协议进程。
//!
//! 全部阻塞 API；HTTP 层经 `tokio::task::spawn_blocking` 调用（识别含 ONNX 推理）。
//! 插件进程按名字缓存复用：配置热加载切换到别的插件时，旧进程随替换被回收
//! （BlockingPluginProcess 持有 kill_on_drop 子进程）。

use base64::Engine as _;
use plugins_worker::{BlockingPluginProcess, Error as PwError, plugin_result_to_lines, scan_plugins};
use rumi_ocr::pdf_route::PdfPage;
use rumi_ocr::{OcrEngine, OcrLine};
use rumi_ocr::postprocess::{self, Layout, PostRules};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use crate::config::{AppConfig, EngineChoice};

/// `/api/ocr` 的后处理选项（body 里的 `layout` / `ignore` 字段）。
#[derive(Debug, Clone, Default)]
pub struct OcrOptions {
    /// 排版解析策略（默认按行）
    pub layout: Layout,
    /// 忽略规则：任一正则整行命中的行被剔除
    pub ignore: Vec<String>,
}

/// 识别 + 后处理的结果：`data` 行、被忽略规则剔除的行数，
/// 以及编译失败被跳过的忽略正则原文（HTTP 层非空时在响应里上报 `bad_regex`）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OcrOutcome {
    pub lines: Vec<UmiLine>,
    pub ignored: usize,
    /// 编译失败、被跳过的忽略正则原文（空 = 全部规则可用）
    pub bad_regex: Vec<String>,
}

/// 识别请求（HTTP body 解析后的形式）。
#[derive(Debug, Clone)]
pub enum OcrInput {
    /// 图片内容 base64（可带 `data:image/...;base64,` 前缀）
    Base64(String),
    /// 图片/PDF 文件路径（native 侧经 rumi_ocr::load_frame 装载）
    Path(String),
}

/// Umi 兼容返回行：`{"text": ..., "box": [[x,y]x4], "score": ...}`。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UmiLine {
    pub text: String,
    #[serde(rename = "box")]
    pub box_: [[f32; 2]; 4],
    pub score: f32,
}

impl From<OcrLine> for UmiLine {
    fn from(l: OcrLine) -> Self {
        Self {
            text: l.text,
            box_: l.r#box,
            score: l.score,
        }
    }
}

/// native 引擎（全局单例，懒构建 + 模型缓存）。
fn native_engine() -> &'static OcrEngine {
    OcrEngine::global()
}

/// 引擎分发器：持有配置路径与插件进程缓存。
pub struct Recognizer {
    config_path: PathBuf,
    plugins_dir: PathBuf,
    python: PathBuf,
    /// 当前插件 worker（按名字缓存；None = 未启动）
    plugin: Mutex<Option<PluginSlot>>,
    /// 启动时刻（状态接口的 uptime）
    pub started: Instant,
}

struct PluginSlot {
    name: String,
    proc: BlockingPluginProcess,
}

impl Recognizer {
    /// `plugins_dir` 显式给定时优先；否则依次探测环境变量 `RUMI_PLUGINS_DIR`
    /// 与工作区布局 `<crate>/../plugins`（开发期）。
    pub fn new(config_path: PathBuf, plugins_dir: Option<PathBuf>, python: PathBuf) -> Self {
        let plugins_dir = plugins_dir
            .or_else(|| std::env::var_os("RUMI_PLUGINS_DIR").map(PathBuf::from))
            .unwrap_or_else(default_plugins_dir);
        Self {
            config_path,
            plugins_dir,
            python,
            plugin: Mutex::new(None),
            started: Instant::now(),
        }
    }

    /// 现读配置（热加载的核心：每次调用都是新值）。
    pub fn config(&self) -> AppConfig {
        AppConfig::load(&self.config_path)
    }

    /// 当前引擎选择。
    pub fn engine_now(&self) -> EngineChoice {
        self.config().engine()
    }

    /// 阻塞执行一次识别（默认后处理：按行、无忽略规则），
    /// 失败返回错误文本（HTTP 层转 `{code:102,data:msg}`）。
    pub fn recognize(&self, input: OcrInput) -> Result<Vec<UmiLine>, String> {
        self.recognize_opts(input, &OcrOptions::default()).map(|o| o.lines)
    }

    /// 阻塞执行一次识别 + 后处理（native 与 plugin 两条路统一过
    /// [`postprocess`]：忽略规则过滤 → 排版解析）。
    pub fn recognize_opts(&self, input: OcrInput, opts: &OcrOptions) -> Result<OcrOutcome, String> {
        let lines = match self.engine_now() {
            EngineChoice::Native => self.recognize_native(input)?,
            EngineChoice::Plugin(name) => self.recognize_plugin(&name, input)?,
        };
        Ok(apply_post(&lines, opts))
    }

    fn recognize_native(&self, input: OcrInput) -> Result<Vec<OcrLine>, String> {
        match input {
            OcrInput::Base64(b64) => {
                let bytes = decode_image_b64(&b64)?;
                let img =
                    rumi_ocr::load_frame_from_bytes(&bytes).map_err(|e| e.to_string())?;
                native_engine().recognize(&img).map_err(|e| e.to_string())
            }
            OcrInput::Path(p) => {
                let path = Path::new(&p);
                // PDF：智能路由（pdf_route）——文字层页直接提取行并入（跳过引擎），
                // 扫描页渲染成图后走现有识别；页序保持原 PDF 顺序。
                // 后处理（忽略规则 / 排版合并）仍由统一的 apply_post 处理。
                // 扩展名判断不区分大小写（.PDF 同样路由）。
                if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
                {
                    let pages = rumi_ocr::pdf_route::route_pdf(path).map_err(|e| e.to_string())?;
                    // 扫描页图像收集起来一次引擎锁批量识别（顺序 = 页序）
                    let raster: Vec<_> = pages
                        .iter()
                        .filter_map(|p| match p {
                            PdfPage::Raster(img) => Some(img.clone()),
                            PdfPage::Text(_) => None,
                        })
                        .collect();
                    let mut recognized: Vec<Vec<OcrLine>> = Vec::new();
                    if !raster.is_empty() {
                        recognized = native_engine()
                            .recognize_many(&raster)
                            .map_err(|e| e.to_string())?;
                    }
                    let mut it = recognized.into_iter();
                    let mut lines = Vec::new();
                    for page in &pages {
                        match page {
                            PdfPage::Text(text_lines) => lines.extend(text_lines.iter().cloned()),
                            PdfPage::Raster(_) => lines.extend(it.next().unwrap_or_default()),
                        }
                    }
                    Ok(lines)
                } else {
                    let img = rumi_ocr::load_frame(path).map_err(|e| e.to_string())?;
                    native_engine().recognize(&img).map_err(|e| e.to_string())
                }
            }
        }
    }

    /// 插件路径：worker 按名缓存复用；切换插件 / worker 死亡时重建（init+start 重走）。
    /// 返回统一中间格式行（缺 box/score 时给占位，由后处理聚合时兜底）。
    fn recognize_plugin(&self, name: &str, input: OcrInput) -> Result<Vec<OcrLine>, String> {
        let mut guard = self
            .plugin
            .lock()
            .map_err(|_| "插件管理锁被毒化".to_string())?;

        let stale = match guard.as_ref() {
            Some(slot) => slot.name != name,
            None => true,
        };
        if stale {
            let dir = self.find_plugin_dir(name).ok_or_else(|| {
                format!(
                    "找不到插件：{name}（扫描目录 {}）",
                    self.plugins_dir.display()
                )
            })?;
            let mut proc = BlockingPluginProcess::spawn(&self.python, &dir, "zh_CN")
                .map_err(|e| e.to_string())?;
            // 规范 §3：init(Api(globalArgd)) → start(argd)。
            // options = schema 默认值（Umi 语义：default 优先，无 default 的
            // optionsList 取首项——demo 插件 language 即此形态）
            // ⊕ 设置页保存的 [plugins.<名>.global/.local]（用户值优先）。
            let cfg = self.config();
            let info = proc.info().map_err(|e| e.to_string())?;
            let global = merged_options(info.get("global_options"), cfg.plugin_global(name));
            proc.init(global).map_err(|e| e.to_string())?;
            let local = merged_options(info.get("local_options"), cfg.plugin_local(name));
            let start = proc.start(local).map_err(|e| e.to_string())?;
            if start.get("ok") != Some(&serde_json::Value::Bool(true)) {
                let err = start
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("start 未回报 ok");
                return Err(format!("插件 {name} 启动失败：{err}"));
            }
            *guard = Some(PluginSlot {
                name: name.to_string(),
                proc,
            });
        }

        let slot = guard.as_mut().expect("上方已确保存在");
        let result = match &input {
            OcrInput::Base64(b64) => slot.proc.run_base64(b64),
            OcrInput::Path(p) => slot.proc.run_path(p),
        };
        // worker 死亡 → 清缓存，下次请求自动重建（规范 §3：重启后须重走 init+start）
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                if matches!(e, PwError::WorkerDied) {
                    *guard = None;
                }
                return Err(e.to_string());
            }
        };
        let lines =
            plugin_result_to_lines(&result).map_err(|e| e.to_string())?;
        Ok(lines
            .into_iter()
            .map(|l| OcrLine {
                text: l.text,
                // 插件未提供 box/score 时给 Umi 兼容的空框/满分占位
                r#box: l.box_.unwrap_or([[0.0, 0.0]; 4]),
                score: l.score.unwrap_or(1.0),
            })
            .collect())
    }

    fn find_plugin_dir(&self, name: &str) -> Option<PathBuf> {
        scan_plugins(&self.plugins_dir).into_iter().find(|p| {
            p.file_name()
                .map_or(false, |n| n.to_string_lossy() == name)
        })
    }
}

/// schema 默认值 ⊕ 用户保存值（用户优先）。
/// 叶子默认值与 Umi/设置页 coerce 同语义：有 `default` 用之；无 `default` 但有
/// `optionsList` 取第一项的值；容器元信息（title/type）不参与。
fn merged_options(schema: Option<&Value>, user: Option<&Value>) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(opts) = schema.and_then(Value::as_object) {
        for (k, node) in opts {
            if k == "title" || k == "type" {
                continue;
            }
            let default = node
                .get("default")
                .cloned()
                .or_else(|| {
                    node.get("optionsList")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|pair| pair.as_array())
                        .and_then(|pair| pair.first())
                        .cloned()
                })
                .unwrap_or(Value::Null);
            out.insert(k.clone(), default);
        }
    }
    if let Some(user) = user.and_then(Value::as_object) {
        for (k, v) in user {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

/// 开发期插件目录：工作区布局 `<crate>/../plugins`。
/// 打包分发时请设 `RUMI_PLUGINS_DIR`（与 plugins-worker 的 RUMI_PYSHIM_DIR 同思路）。
fn default_plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../plugins")
}

/// 统一后处理：忽略规则过滤 → 排版解析；输出仍保持 Umi 的 data 行形状。
///
/// 每条输出取后处理分组（`Processed::groups` 给的是原始行下标）：
/// 单行组沿用该行原 box/score；多行段用成员框的并集 + 最低分。
/// 编译失败的忽略正则原样打日志后跳过（不中断识别）。
fn apply_post(lines: &[OcrLine], opts: &OcrOptions) -> OcrOutcome {
    let rules = PostRules { ignore_regex: opts.ignore.clone() };
    let processed = postprocess::postprocess(lines, opts.layout, &rules);
    for bad in &processed.bad_regex {
        eprintln!("rumi-server：忽略规则编译失败，已跳过：{bad}");
    }
    let out = processed
        .groups
        .iter()
        .zip(&processed.texts)
        .map(|(g, text)| {
            let members: Vec<&OcrLine> = g.iter().map(|&i| &lines[i]).collect();
            UmiLine {
                text: text.clone(),
                box_: union_box(&members),
                score: members.iter().map(|l| l.score).fold(f32::INFINITY, f32::min),
            }
        })
        .collect();
    OcrOutcome { lines: out, ignored: processed.ignored, bad_regex: processed.bad_regex }
}

/// 成员行四点框的轴对齐并集（min/max 外接矩形）；空组兜底全零框。
fn union_box(members: &[&OcrLine]) -> [[f32; 2]; 4] {
    let mut min = [f32::MAX; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for m in members {
        for p in m.r#box {
            min[0] = min[0].min(p[0]);
            min[1] = min[1].min(p[1]);
            max[0] = max[0].max(p[0]);
            max[1] = max[1].max(p[1]);
        }
    }
    if min[0] > max[0] {
        return [[0.0; 2]; 4];
    }
    [[min[0], min[1]], [max[0], min[1]], [max[0], max[1]], [min[0], max[1]]]
}

/// base64（容忍 data-URL 前缀）→ 图片字节。
fn decode_image_b64(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    // "data:image/png;base64,xxxx" → 取逗号后段
    let payload = s.split_once(',').filter(|(head, _)| head.ends_with(";base64")).map(|(_, tail)| tail).unwrap_or(s);
    use base64::engine::general_purpose::STANDARD;
    STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("base64 解码失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rumi_ocr::postprocess::Layout;

    /// 合成行：矩形框，左上角 (x, y)，高 30。
    fn oline(text: &str, y: f32) -> OcrLine {
        OcrLine {
            text: text.into(),
            score: 0.9,
            r#box: [[10.0, y], [110.0, y], [110.0, y + 30.0], [10.0, y + 30.0]],
        }
    }

    #[test]
    fn umi_line_serializes_text_box_score() {
        let l = UmiLine {
            text: "你好".into(),
            box_: [[0.0, 0.0], [10.0, 0.0], [10.0, 5.0], [0.0, 5.0]],
            score: 0.98,
        };
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["text"], "你好");
        assert_eq!(v["box"][2], serde_json::json!([10.0, 5.0]));
        // f32 精度：近似比较
        assert!((v["score"].as_f64().unwrap() - 0.98).abs() < 1e-6);
    }

    #[test]
    fn merged_options_schema_defaults_plus_user_overrides() {
        let schema = serde_json::json!({
            "title": "阿巴阿巴OCR", "type": "group",
            "api_key": {"title": "密钥", "default": ""},
            "retries": {"title": "重试", "default": 3},
            "language": {"title": "语言", "optionsList": [["zh_CN","简体中文"],["en_US","English"]]},
        });
        let user = serde_json::json!({"api_key": "test-123", "language": "en_US"});

        let m = merged_options(Some(&schema), Some(&user));
        let obj = m.as_object().unwrap();
        // 用户值优先
        assert_eq!(obj["api_key"], "test-123");
        assert_eq!(obj["language"], "en_US");
        // 未覆盖项回退 schema 语义
        assert_eq!(obj["retries"], 3);
        // 容器元信息不混入
        assert!(!obj.contains_key("title") && !obj.contains_key("type"));

        // 无用户配置：optionsList 取首项
        let m = merged_options(Some(&schema), None);
        assert_eq!(m["language"], "zh_CN");
        assert_eq!(m["api_key"], "");
    }

    #[test]
    fn apply_post_filters_and_merges_with_union_box() {
        let lines = [oline("你好", 0.0), oline("SECRET", 20.0), oline("世界", 40.0)];
        let opts = OcrOptions {
            layout: Layout::MergePara,
            ignore: vec!["SECRET".into()],
        };
        let out = apply_post(&lines, &opts);
        assert_eq!(out.ignored, 1);
        assert_eq!(out.lines.len(), 1, "合并后只剩一段");
        let l = &out.lines[0];
        assert_eq!(l.text, "你好世界");
        // 并集框：跨过被剔除行的原始位置（y 0..70）
        assert_eq!(l.box_, [[10.0, 0.0], [110.0, 0.0], [110.0, 70.0], [10.0, 70.0]]);
        assert_eq!(l.score, 0.9);
    }

    #[test]
    fn apply_post_by_line_keeps_original_boxes() {
        let lines = [oline("第一行", 0.0), oline("第二行", 40.0)];
        let out = apply_post(&lines, &OcrOptions::default());
        assert_eq!(out.ignored, 0);
        assert_eq!(out.lines.len(), 2);
        assert_eq!(out.lines[0].box_, [[10.0, 0.0], [110.0, 0.0], [110.0, 30.0], [10.0, 30.0]]);
        assert_eq!(out.lines[1].text, "第二行");
    }

    #[test]
    fn base64_decode_tolerates_data_url() {
        let raw = b"png";
        let b64 = plugins_worker::base64_encode(raw);
        assert_eq!(decode_image_b64(&b64).unwrap(), raw);
        assert_eq!(
            decode_image_b64(&format!("data:image/png;base64,{b64}")).unwrap(),
            raw
        );
        assert!(decode_image_b64("!!!not base64!!!").is_err());
    }

    #[test]
    fn apply_post_reports_bad_regex_in_outcome() {
        // 编译失败的正则跳过并记入 bad_regex（Wave1-B 遗留：HTTP 层据此上报），
        // 好正则照常过滤
        let lines = [oline("你好", 0.0), oline("SECRET", 20.0)];
        let opts = OcrOptions {
            layout: Layout::ByLine,
            ignore: vec!["([unclosed".into(), "SECRET".into()],
        };
        let out = apply_post(&lines, &opts);
        assert_eq!(out.bad_regex, vec!["([unclosed"]);
        assert_eq!(out.ignored, 1);
        assert_eq!(out.lines.len(), 1);

        // 无 ignore 规则：bad_regex 为空（HTTP 层省略字段保持兼容）
        assert!(apply_post(&lines, &OcrOptions::default()).bad_regex.is_empty());
    }

    #[test]
    fn recognizer_defaults_for_missing_config() {
        // 无配置文件 → native；native 路径对缺失文件报 IO 错误
        let rec = Recognizer::new(
            PathBuf::from("/nonexistent/rumi/config.toml"),
            None,
            PathBuf::from("python3"),
        );
        assert_eq!(rec.engine_now(), EngineChoice::Native);
        let err = rec
            .recognize(OcrInput::Path("/tmp/definitely-missing.png".into()))
            .unwrap_err();
        assert!(err.contains("No such file"), "实际错误：{err}");

        // engine=plugin:<不存在> → 分发器报"找不到插件"
        let dir = std::env::temp_dir().join(format!("rumi-disp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        std::fs::write(&cfg, "engine = \"plugin:not_exist_plugin\"\n").unwrap();
        let rec = Recognizer::new(cfg, Some(dir.clone()), PathBuf::from("python3"));
        let err = rec
            .recognize(OcrInput::Path("/tmp/x.png".into()))
            .unwrap_err();
        assert!(err.contains("找不到插件"), "实际错误：{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recognizer_pdf_text_layer_skips_engine() {
        // 纯文字层 PDF 走 route_pdf：行直接来自文字层（score=1.0），
        // 全程不触碰 ONNX 引擎（无扫描页 → recognize_many 不会被调用）。
        let dir = std::env::temp_dir().join(format!("rumi-disp-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        std::fs::write(&cfg, "engine = \"native\"\n").unwrap();
        let rec = Recognizer::new(cfg, Some(dir.clone()), PathBuf::from("python3"));

        let pdf = dir.join("text-sample.pdf");
        rumi_ocr::pdf_route::sample::generate_text_pdf(&pdf).unwrap();
        let lines = rec
            .recognize(OcrInput::Path(pdf.to_string_lossy().into_owned()))
            .expect("文字层 PDF 应直接出行");
        assert!(
            lines.iter().any(|l| l.text.contains("深秋的校园")),
            "应含样例中文行，实际：{:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        // 文字层行置信度恒为 1.0（无识别环节）
        assert!(lines.iter().all(|l| (l.score - 1.0).abs() < 1e-6));

        // 缺失 .pdf 仍报"文件不存在"（HTTP 测试据此区分 PDF/图片分支）
        let err = rec
            .recognize(OcrInput::Path("/nonexistent/rumi/definitely-missing.pdf".into()))
            .unwrap_err();
        assert!(err.contains("文件不存在"), "实际错误：{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
