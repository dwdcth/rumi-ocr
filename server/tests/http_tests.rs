//! M3 §5 HTTP 集成测试：随机端口起服务，用裸 TcpStream 打 127.0.0.1（不依赖网络）。
//!
//! 覆盖：健康状态 / Umi 错误形状 / native base64 全链路 / 配置热加载（含插件路径报错）/
//! token 鉴权 / shutdown 生命周期。
//! Wave1-B 追加（P2 #11/#12/#14）：`layout` 排版参数、`ignore` 忽略规则、
//! 响应新增 `count`/`ignored` 字段、非法 layout 报 code 102。
//! 遗留收尾：`bad_regex` 上报（空时省略）、`rule` 别名（冲突报 102）、
//! `.pdf` 路径路由到 load_pdf 分支。

use rumi_server::{ServerConfig, ServerHandle, serve};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// 极简 HTTP/1.1 客户端：Connection: close + 读到 EOF，解析状态行与 body。
fn http(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(addr).expect("connect 失败（服务未起？）");
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).expect("写请求失败");
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).expect("读响应失败");
    let text = String::from_utf8_lossy(&raw);
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

fn unique_tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    std::env::temp_dir().join(format!("rumi-srvtest-{}-{}-{}", tag, std::process::id(), nanos))
}

/// 起一个 port=0 的服务实例；config_text 给定时写入临时配置文件。
async fn start(tag: &str, config_text: Option<&str>) -> (SocketAddr, ServerHandle, PathBuf) {
    let dir = unique_tmp(tag);
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    if let Some(t) = config_text {
        std::fs::write(&config_path, t).unwrap();
    }
    let cfg = ServerConfig {
        port: 0,
        config_path,
        ..Default::default()
    };
    let (addr, handle) = serve(cfg).await.expect("serve 失败");
    (addr, handle, dir)
}

/// 64x64 纯白 PNG（native 引擎识别得空结果，code=100）。
fn blank_png_b64() -> String {
    let mut img = image::GrayImage::from_pixel(64, 64, image::Luma([255]));
    // 画一点内容，避免全白图的极端分支
    for x in 8..56 {
        img.put_pixel(x, 32, image::Luma([0]));
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .unwrap();
    plugins_worker::base64_encode(buf.get_ref())
}

/// native 引擎可用性：模型已缓存（~/.oar）或预热成功才跑真识别用例。
fn native_available() -> bool {
    match rumi_ocr::OcrEngine::global().warmup() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("SKIP：native 引擎预热失败（无 ~/.oar 缓存且可能离线）：{e}");
            false
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_ok_and_engine() {
    let (addr, handle, dir) = start("status", None).await;
    let (code, body) = http(addr, "GET", "/", None);
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["engine"], "native"); // 无配置文件 → native
    assert_eq!(v["token_auth"], false);

    // /api 同款
    let (code, body) = http(addr, "GET", "/api", None);
    assert_eq!(code, 200);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["status"], "ok");
    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_bad_requests_shape_code_102() {
    let (addr, handle, dir) = start("badreq", None).await;

    // 坏 JSON
    let (code, body) = http(addr, "POST", "/api/ocr", Some("this is not json"));
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], 102);
    assert!(v["data"].as_str().unwrap().contains("JSON"));

    // 缺参数
    let (_, body) = http(addr, "POST", "/api/ocr", Some("{}"));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], 102);
    assert!(v["data"].as_str().unwrap().contains("base64"));

    // path 指向不存在的文件（native 装载失败）
    let (_, body) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(r#"{"path": "/nonexistent/rumi/definitely-missing.png"}"#),
    );
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], 102);

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_native_base64_full_pipeline() {
    if !native_available() {
        return;
    }
    let (addr, handle, dir) = start("native", None).await;
    let body = format!(r#"{{"base64": "{}"}}"#, blank_png_b64());
    let (code, resp) = http(addr, "POST", "/api/ocr", Some(&body));
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "实际响应：{resp}");
    assert!(v["data"].is_array(), "data 应为数组：{resp}");
    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// Wave1-B：layout / ignore 后处理参数与 count/ignored 响应字段（P2 #11/#12/#14）
// ---------------------------------------------------------------------------

/// 找一个能渲染中文的字体（Noto Sans CJK 优先，回退任意 ttf/ttc/otf）。
fn test_font() -> ab_glyph::FontArc {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut all = Vec::new();
    for root in ["/usr/share/fonts", "/usr/local/share/fonts"] {
        collect(Path::new(root), &mut all);
    }
    let prefer = ["NotoSansCJK-Regular", "NotoSansCJK-Bold", "NotoSansCJK", "DejaVuSans.ttf"];
    let path = prefer
        .iter()
        .find_map(|key| all.iter().find(|p| p.to_string_lossy().contains(key)))
        .or_else(|| {
            all.iter().find(|p| {
                matches!(p.extension().and_then(|e| e.to_str()), Some("ttf" | "ttc" | "otf"))
            })
        })
        .expect("系统缺少可用字体（ttf/ttc/otf）")
        .clone();
    let bytes = std::fs::read(&path).expect("读取字体失败");
    if bytes.starts_with(b"ttcf") {
        let fv = ab_glyph::FontVec::try_from_vec_and_index(bytes, 0).expect("ttc 加载失败");
        ab_glyph::FontArc::from(fv)
    } else {
        ab_glyph::FontArc::try_from_vec(bytes).expect("字体加载失败")
    }
}

/// 白底黑字合成识别图 base64：每行一条大字（native 引擎可稳定检出）。
fn text_png_b64(lines: &[&str]) -> String {
    use imageproc::drawing::draw_text_mut;
    let font = test_font();
    let (w, line_h) = (1200u32, 110u32);
    let mut img = image::RgbaImage::from_pixel(
        w,
        line_h * lines.len() as u32 + 40,
        image::Rgba([255, 255, 255, 255]),
    );
    for (i, text) in lines.iter().enumerate() {
        draw_text_mut(
            &mut img,
            image::Rgba([0, 0, 0, 255]),
            20,
            20 + line_h as i32 * i as i32,
            72.0,
            &font,
            text,
        );
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .unwrap();
    plugins_worker::base64_encode(buf.get_ref())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_layout_param_changes_output_grouping() {
    if !native_available() {
        return;
    }
    let (addr, handle, dir) = start("layout", None).await;
    let b64 = text_png_b64(&["你好世界", "Rumi OCR 123"]);
    let img = format!(r#"{{"base64": "{b64}"}}"#);

    // 默认（无 layout）= by_line：count 与 data 长度一致，且至少识别出一行
    let (code, resp) = http(addr, "POST", "/api/ocr", Some(&img));
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "实际响应：{resp}");
    let by_line_count = v["count"].as_u64().expect("响应应有 count 字段");
    assert_eq!(by_line_count, v["data"].as_array().unwrap().len() as u64);
    assert!(by_line_count > 0, "引擎应至少识别出一行：{resp}");
    assert_eq!(v["ignored"], 0);
    // data 行形状不变（Umi 兼容）：text/box/score 齐全
    let first = &v["data"][0];
    assert!(first["text"].is_string() && first["box"].is_array() && first["score"].is_number());

    // merge_para：无论引擎切出几行，输出必为一段（含两行内容、无换行）
    let body = format!(r#"{{"base64": "{b64}", "layout": "merge_para"}}"#);
    let (code, resp) = http(addr, "POST", "/api/ocr", Some(&body));
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "{resp}");
    assert_eq!(v["count"], 1, "merge_para 应合并为一段：{resp}");
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    let text = v["data"][0]["text"].as_str().unwrap();
    assert!(text.contains("你好") && text.contains("Rumi"), "合并文本：{text:?}");
    assert!(!text.contains('\n'), "段内不应有换行：{text:?}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_ignore_param_filters_lines_and_reports_ignored() {
    if !native_available() {
        return;
    }
    let (addr, handle, dir) = start("ignore", None).await;
    let b64 = text_png_b64(&["你好世界", "Rumi OCR 123"]);
    let img = format!(r#"{{"base64": "{b64}"}}"#);

    // 基准：不过滤，至少识别出一行
    let (_, resp) = http(addr, "POST", "/api/ocr", Some(&img));
    let base: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let kept = base["count"].as_u64().expect("count 是数字");
    assert!(kept > 0, "基准识别为空：{resp}");

    // ignore=["."]（is_match 语义，命中任何非空行）：全部剔除
    let body = format!(r#"{{"base64": "{b64}", "ignore": ["."]}}"#);
    let (_, resp) = http(addr, "POST", "/api/ocr", Some(&body));
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "{resp}");
    assert_eq!(v["count"], 0, "全命中时应无输出：{resp}");
    assert_eq!(
        v["ignored"], kept,
        "ignored 应等于基准行数（同引擎同图）：{resp}"
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 0);

    // 不命中的规则：一切照旧
    let body = format!(r#"{{"base64": "{b64}", "ignore": ["绝不匹配的文本xyz"]}}"#);
    let (_, resp) = http(addr, "POST", "/api/ocr", Some(&body));
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["count"], kept, "不命中的规则不影响输出");
    assert_eq!(v["ignored"], 0);

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_invalid_layout_returns_code_102() {
    let (addr, handle, dir) = start("badlayout", None).await;
    let body = format!(r#"{{"base64": "{}", "layout": "bogus"}}"#, blank_png_b64());
    let (code, resp) = http(addr, "POST", "/api/ocr", Some(&body));
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 102, "非法 layout 应报 102：{resp}");
    let msg = v["data"].as_str().expect("data 应是错误文本");
    assert!(msg.contains("layout"), "错误信息应指出 layout：{msg}");

    // 空串 = 省略，回退默认 by_line（1x1/空白图识别 0 行但 code=100）
    let body = format!(r#"{{"base64": "{}", "layout": ""}}"#, blank_png_b64());
    let (_, resp) = http(addr, "POST", "/api/ocr", Some(&body));
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "空 layout 应按默认处理：{resp}");
    assert_eq!(v["count"], 0);

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// 遗留收尾：bad_regex 上报 / rule 别名 / PDF 路径路由
// ---------------------------------------------------------------------------

/// demo 插件不依赖 native 模型缓存：用它验证 bad_regex 上报与 rule 别名的
/// 成功链路（CI/离线环境稳定）。demo 返回固定单行"阿巴阿巴阿巴"。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_bad_regex_reported_in_response() {
    let (addr, handle, dir) = start(
        "badregex",
        Some("engine = \"plugin:demo_AbaOCR\"\n"),
    )
    .await;

    // 无 ignore 规则：响应不含 bad_regex 字段（空时省略，保持旧客户端形状）
    let (_, resp) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(r#"{"path": "/tmp/rumi-anything.png"}"#),
    );
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "{resp}");
    assert!(v.get("bad_regex").is_none(), "空 bad_regex 应省略字段：{resp}");

    // 含编译失败的正则：原样上报；好正则照常生效（demo 单行不含匹配文本）
    let (_, resp) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(
            r#"{"path": "/tmp/rumi-anything.png", "ignore": ["([unclosed", "绝不匹配的文本xyz"]}"#,
        ),
    );
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "{resp}");
    assert_eq!(
        v["bad_regex"],
        serde_json::json!(["([unclosed"]),
        "坏正则应原样出现在响应：{resp}"
    );

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_rule_alias_works_and_conflict_reports_102() {
    let (addr, handle, dir) = start(
        "rule",
        Some("engine = \"plugin:demo_AbaOCR\"\n"),
    )
    .await;

    // rule 是 layout 的别名：非法值生效并指明来源是 rule
    let (_, resp) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(r#"{"path": "/tmp/rumi-anything.png", "rule": "bogus"}"#),
    );
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 102);
    let msg = v["data"].as_str().expect("data 应是错误文本");
    assert!(msg.contains("rule") && msg.contains("bogus"), "错误应指明 rule：{msg}");

    // 同传且不一致 → code 102
    let (_, resp) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(r#"{"base64": "x", "layout": "by_line", "rule": "merge_para"}"#),
    );
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 102, "冲突应报 102：{resp}");
    assert!(v["data"].as_str().unwrap().contains("冲突"), "{resp}");

    // 同传且一致 → 正常识别（demo 插件恒定返回单行）
    let (_, resp) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(r#"{"path": "/tmp/rumi-anything.png", "layout": "merge_para", "rule": "merge_para"}"#),
    );
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 100, "一致的 layout/rule 应正常：{resp}");
    assert_eq!(v["count"], 1);

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_pdf_path_routes_to_pdf_branch() {
    // .pdf 扩展名应路由到 load_pdf 分支（不依赖识别模型：预检先于引擎）。
    // load_pdf 对缺失文件自报"文件不存在：…"；若仍走图片分支则报
    // image crate 的"图像解码失败/IO No such file"——以此区分两条装载路径。
    let (addr, handle, dir) = start("pdf", None).await;
    let (_, resp) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(r#"{"path": "/nonexistent/rumi/definitely-missing.pdf"}"#),
    );
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["code"], 102);
    let msg = v["data"].as_str().unwrap();
    assert!(
        msg.contains("文件不存在"),
        "缺失 .pdf 应走 PDF 分支（load_pdf 预检报错）：{msg}"
    );
    assert!(!msg.contains("图像解码"), "不应落入图片解码分支：{msg}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_hot_reload_via_config_file() {
    let (addr, handle, dir) = start("hot", None).await;
    let config_path = dir.join("config.toml");

    // 初始：native
    let (_, body) = http(addr, "GET", "/", None);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["engine"], "native");

    // 改配置 → 立即生效（热加载），且插件路径走分发器报"找不到插件"
    std::fs::write(&config_path, "engine = \"plugin:not_exist_plugin\"\n").unwrap();
    let (_, body) = http(addr, "GET", "/", None);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["engine"],
        "plugin:not_exist_plugin"
    );
    let (_, body) = http(addr, "POST", "/api/ocr", Some(r#"{"path": "/tmp/x.png"}"#));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], 102);
    assert!(
        v["data"].as_str().unwrap().contains("找不到插件"),
        "插件分发错误：{v}"
    );

    // 改回 native → 恢复
    std::fs::write(&config_path, "engine = \"native\"\n").unwrap();
    let (_, body) = http(addr, "GET", "/", None);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["engine"], "native");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_demo_engine_returns_umi_format() {
    // demo 插件无真引擎，返回固定文本；验证转发 + Umi 形状
    let (addr, handle, dir) = start(
        "plugin",
        Some("engine = \"plugin:demo_AbaOCR\"\n"),
    )
    .await;
    let (_, body) = http(
        addr,
        "POST",
        "/api/ocr",
        Some(r#"{"path": "/tmp/rumi-anything.png"}"#),
    );
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], 100, "demo 插件应成功：{v}");
    let line = &v["data"][0];
    assert_eq!(line["text"], "阿巴阿巴阿巴");
    assert!(line["box"].is_array());
    assert!(line["score"].is_number());
    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_auth_enforced() {
    let (addr, handle, dir) = start("token", Some("token = \"s3cret\"\n")).await;

    // 无 token → 401
    let (code, body) = http(addr, "POST", "/api/ocr", Some("{}"));
    assert_eq!(code, 401);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"], 102);

    // query token 通过鉴权（进入参数校验 → 102 缺参数）
    let (code, body) = http(addr, "POST", "/api/ocr?token=s3cret", Some("{}"));
    assert_eq!(code, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], 102);
    assert!(v["data"].as_str().unwrap().contains("base64"));

    // Bearer 头也行
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let body = "{}";
    let req = format!(
        "POST /api/ocr HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer s3cret\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    assert!(raw.starts_with("HTTP/1.1 200"), "Bearer 头应通过：{raw}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_accepting() {
    let (addr, handle, dir) = start("shut", None).await;
    let (code, _) = http(addr, "GET", "/", None);
    assert_eq!(code, 200);

    handle.shutdown().await; // join 完成才返回

    // 端口已释放：再连应被拒绝
    assert!(
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)).is_err(),
        "shutdown 后端口应不再接受连接"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
