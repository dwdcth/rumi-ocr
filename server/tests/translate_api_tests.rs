//! 翻译 API 集成测试（规格 E §6，不打真网）：随机端口起服务，裸 TcpStream 直调
//! （与 http_tests.rs 同款基建，复制其 http() / start() 助手）。
//!
//! 覆盖：text/texts 校验 / 未知引擎 / 离线缺模型报错（model_absent）/ from==to 原样 /
//! 空 texts 短路 / GET models 清单 / install 未知 key。
//!
//! 模型目录注入取舍（规格 §3 备选方案）：不注入 `RUMI_TRANSLATE_MODELS_DIR`——
//! edition 2024 `set_var` 为 unsafe 且 `#[tokio::test]` 默认并行有竞态，而离线引擎
//! 单例（engine::shared()）以首次调用时的环境变量初始化，跨用例不可控；改为依赖
//! 「干净环境无模型」前提（CI 成立），并用 `is_installed` 预检跳过已装模型的环境。
//! 真翻译链路由 rumi-translate 的 E2E（RUMI_E2E=1）覆盖，此处不重复。

use rumi_server::{ServerConfig, ServerHandle, serve};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;

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
    std::env::temp_dir().join(format!("rumi-trtest-{}-{}-{}", tag, std::process::id(), nanos))
}

/// 起一个 port=0 的服务实例（无配置文件 → 默认行为）。
async fn start(tag: &str) -> (SocketAddr, ServerHandle, PathBuf) {
    let dir = unique_tmp(tag);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = ServerConfig {
        port: 0,
        config_path: dir.join("config.toml"),
        ..Default::default()
    };
    let (addr, handle) = serve(cfg).await.expect("serve 失败");
    (addr, handle, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_text_texts_conflict_or_missing_rejected() {
    let (addr, handle, dir) = start("tr-body").await;

    // text 与 texts 同给 → 400
    let (code, body) = http(
        addr,
        "POST",
        "/api/translate",
        Some(r#"{"text":"a","texts":["b"]}"#),
    );
    assert_eq!(code, 400, "同给应 400：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("只能传其一"), "{body}");

    // 都缺 → 400
    let (code, body) = http(addr, "POST", "/api/translate", Some("{}"));
    assert_eq!(code, 400, "都缺应 400：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("text"), "{body}");

    // 坏 JSON 也是校验错 → 400
    let (code, body) = http(addr, "POST", "/api/translate", Some("not json"));
    assert_eq!(code, 400, "坏 JSON 应 400：{body}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_unknown_engine_returns_400() {
    let (addr, handle, dir) = start("tr-engine").await;
    let (code, body) = http(
        addr,
        "POST",
        "/api/translate",
        Some(r#"{"text":"hi","engine":"baidu"}"#),
    );
    assert_eq!(code, 400, "未知引擎应 400：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let msg = v["error"].as_str().unwrap();
    assert!(msg.contains("baidu"), "错误应点名引擎：{msg}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_offline_model_absent_returns_error() {
    // 干净环境（未装 en_zh）→ 400 且文案含「未安装」/「暂无」（engine 错误 Display 透传）。
    // 装过模型的环境会真翻译，断言不成立 → 预检跳过（见文件头注释的注入取舍）。
    if rumi_translate::engine::shared().is_installed("en", "zh") {
        eprintln!("SKIP：本机已装 en_zh 模型，model_absent 用例不适用");
        return;
    }
    let (addr, handle, dir) = start("tr-absent").await;
    let (code, body) = http(
        addr,
        "POST",
        "/api/translate",
        Some(r#"{"text":"The default port is 8080."}"#),
    );
    assert_eq!(code, 400, "缺模型应 400：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let msg = v["error"].as_str().unwrap();
    assert!(
        msg.contains("未安装") || msg.contains("暂无"),
        "文案应透传 engine Display：{msg}"
    );

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_same_lang_passthrough() {
    // from==to 不加载模型（facade 直通），空模型目录也能 200 原样返回
    let (addr, handle, dir) = start("tr-same").await;
    let (code, body) = http(
        addr,
        "POST",
        "/api/translate",
        Some(r#"{"texts":["hello","world"],"from":"en","to":"en"}"#),
    );
    assert_eq!(code, 200, "from==to 应原样 200：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["texts"],
        serde_json::json!(["hello", "world"]),
        "逐条原样：{body}"
    );
    assert_eq!(v["engine"], "offline", "缺省引擎名应回显 offline：{body}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_empty_texts_short_circuits() {
    let (addr, handle, dir) = start("tr-empty").await;
    let (code, body) = http(addr, "POST", "/api/translate", Some(r#"{"texts":[]}"#));
    assert_eq!(code, 200, "空 texts 应 200：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["texts"], serde_json::json!([]));
    assert_eq!(v["engine"], "offline");
    assert_eq!(v["elapsed_ms"], 0, "短路路径 elapsed 恒 0：{body}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_list_contains_registry_keys() {
    let (addr, handle, dir) = start("tr-models").await;
    let (code, body) = http(addr, "GET", "/api/translate/models", None);
    assert_eq!(code, 200, "清单应 200：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let arr = v["models"].as_array().expect("models 应为数组");
    for key in ["zh_en", "en_zh"] {
        let hit = arr
            .iter()
            .find(|m| m["key"] == key)
            .unwrap_or_else(|| panic!("models 应含 {key}：{body}"));
        assert!(hit["size_mb"].is_u64(), "应带 size_mb：{hit}");
        assert!(hit["installed"].is_boolean(), "应带 installed 标志：{hit}");
    }
    // imported 恒为数组（无导入包时为空）
    assert!(v["imported"].is_array(), "imported 应为数组：{body}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_unknown_key_returns_400() {
    let (addr, handle, dir) = start("tr-install").await;
    let (code, body) = http(
        addr,
        "POST",
        "/api/translate/models/install",
        Some(r#"{"key":"nope"}"#),
    );
    assert_eq!(code, 400, "未知 key 应 400：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let msg = v["error"].as_str().unwrap();
    assert!(msg.contains("nope"), "错误应点名 key：{msg}");
    // 不真下载（分钟级 + 打真网）：合法 key 的安装链路由 rumi-translate E2E 覆盖

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------- POST /api/translate/doc（文档翻译端点） ----------

/// from==to 直通的正向端到端：txt → .md，零模型零网络。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_doc_passthrough_txt_to_md() {
    let (addr, handle, dir) = start("tr-doc-ok").await;
    let input = dir.join("in.txt");
    std::fs::write(&input, "Hello world paragraph.").unwrap();
    let output = dir.join("out.bilingual.md");
    let body = serde_json::json!({
        "input": input.display().to_string(),
        "output": output.display().to_string(),
        "from": "en", "to": "en",
    });
    let (code, body) = http(addr, "POST", "/api/translate/doc", Some(&body.to_string()));
    assert_eq!(code, 200, "直通翻译应 200：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["units"], 1);
    assert!(output.is_file(), "输出 .md 应生成");
    assert!(std::fs::read_to_string(&output).unwrap().contains("Hello world paragraph."));

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

/// 坏路径：文件不存在 400；未知策略 400。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_doc_bad_paths() {
    let (addr, handle, dir) = start("tr-doc-bad").await;
    let (code, body) = http(
        addr,
        "POST",
        "/api/translate/doc",
        Some(r#"{"input":"/nonexistent/x.txt","output":"/tmp/x.md"}"#),
    );
    assert_eq!(code, 400, "缺文件应 400：{body}");

    let input = dir.join("in.txt");
    std::fs::write(&input, "text").unwrap();
    let body = serde_json::json!({
        "input": input.display().to_string(),
        "output": dir.join("o.md").display().to_string(),
        "strategy": "bogus",
    });
    let (code, body) = http(addr, "POST", "/api/translate/doc", Some(&body.to_string()));
    assert_eq!(code, 400, "未知策略应 400：{body}");

    handle.shutdown().await;
    std::fs::remove_dir_all(&dir).unwrap();
}
