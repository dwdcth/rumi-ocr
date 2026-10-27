//! M2 验收集成测试（规范 §8）：用官方 demo_AbaOCR 插件实测。
//!
//! 依赖：系统 `python3` 在 PATH 上；工作区布局下自动定位 `pyshim/umi_worker.py`。

use plugins_worker::{BlockingPluginProcess, Error, PluginEntry, base64_encode, plugin_result_to_lines, scan_plugins};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("工作区根目录")
}

fn demo_plugin() -> PathBuf {
    project_root().join("plugins/demo_AbaOCR")
}

fn unique_tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    std::env::temp_dir().join(format!("rumi-pwtest-{}-{}-{}", tag, std::process::id(), nanos))
}

/// 验收 1：扫描 → info → init(global含api_key) → start(language=zh_CN) → runPath/runBytes/runBase64
/// 返回 code:100 且 text=="阿巴阿巴阿巴"。
#[test]
fn demo_plugin_full_flow_zh_cn() {
    // scan_plugins 应能找到 demo 插件
    let found = scan_plugins(&project_root().join("plugins"));
    assert!(
        found.iter().any(|p| p.file_name().map_or(false, |n| n == "demo_AbaOCR")),
        "scan_plugins 未找到 demo_AbaOCR：{found:?}"
    );

    let mut p = BlockingPluginProcess::spawn(Path::new("python3"), &demo_plugin(), "zh_CN")
        .expect("spawn worker");

    // info：schema（zh_CN 时 CSV 无 zh_CN 列 → 第0列 → 原文）
    let info = p.info().expect("info");
    assert_eq!(info["name"], "demo_AbaOCR");
    assert_eq!(info["group"], "ocr");
    assert_eq!(info["global_options"]["title"], "阿巴阿巴OCR");
    assert_eq!(info["global_options"]["api_key"]["default"], "");
    assert_eq!(
        info["local_options"]["language"]["optionsList"][0],
        json!(["zh_CN", "简体中文"])
    );
    let entry = PluginEntry::from_info(&info);
    assert_eq!(entry.name, "demo_AbaOCR");

    // init：global 含 api_key
    p.init(json!({"api_key": "test-key-123"})).expect("init");

    // start：language=zh_CN
    let start = p.start(json!({"language": "zh_CN"})).expect("start");
    assert_eq!(start["ok"], Value::Bool(true));
    assert_eq!(start["error"], "");

    // runPath：任意图片路径（demo 不真读图）
    let res = p
        .run_path("/tmp/rumi-demo-nonexistent.png")
        .expect("runPath");
    assert_eq!(res["code"], 100);
    assert_eq!(res["data"][0]["text"], "阿巴阿巴阿巴");

    // 统一中间格式
    let lines = plugin_result_to_lines(&res).expect("to lines");
    assert_eq!(lines[0].text, "阿巴阿巴阿巴");
    assert_eq!(
        lines[0].box_,
        Some([[0.0, 0.0], [200.0, 0.0], [200.0, 40.0], [0.0, 40.0]])
    );
    assert_eq!(lines[0].score, Some(1.0));

    // runBytes / runBase64
    let res_b = p.run_bytes(b"png-bytes").expect("runBytes");
    assert_eq!(res_b["data"][0]["text"], "阿巴阿巴阿巴");
    let res_b64 = p.run_base64(&base64_encode(b"png-bytes")).expect("runBase64");
    assert_eq!(res_b64["code"], 100);

    p.stop().expect("stop");
}

/// 验收 4：i18n.csv 翻译生效（en_US 时 title 变英文）。
#[test]
fn demo_plugin_i18n_en_us() {
    let mut p = BlockingPluginProcess::spawn(Path::new("python3"), &demo_plugin(), "en_US")
        .expect("spawn worker");
    let info = p.info().expect("info");
    assert_eq!(info["global_options"]["title"], "Aba OCR");
    assert_eq!(
        info["global_options"]["api_key"]["title"],
        "Api Key"
    );
    assert_eq!(
        info["local_options"]["title"],
        "Text recognition (Aba OCR)"
    );

    // en_US 下识别文本也应跟随语言
    p.init(json!({"api_key": ""})).expect("init");
    p.start(json!({"language": "en_US"})).expect("start");
    let res = p.run_path("/tmp/x.png").expect("runPath");
    assert_eq!(res["data"][0]["text"], "Aba Aba Aba");
}

/// 插件层失败（code:102）原样转发，不炸协议。
#[test]
fn plugin_level_failure_code_102() {
    let mut p = BlockingPluginProcess::spawn(Path::new("python3"), &demo_plugin(), "zh_CN")
        .expect("spawn worker");
    p.info().expect("info");
    p.init(json!({"api_key": ""})).expect("init");
    p.start(json!({"language": "zh_CN"})).expect("start");
    // 未知语言 → 插件返回 code:102
    let res = p.start(json!({"language": "xx_XX"})).ok(); // start 可重复调用，覆盖 lang
    let _ = res;
    let out = p.run_path("/tmp/x.png").expect("runPath");
    assert_eq!(out["code"], 102);
    match plugin_result_to_lines(&out) {
        Err(Error::Worker(msg)) => assert!(msg.contains("未知的语言")),
        other => panic!("预期 Error::Worker，实际 {other:?}"),
    }
}

/// 验收 3：worker 被杀死后宿主报 Error::WorkerDied。
#[test]
fn worker_death_reported_as_worker_died() {
    let mut p = BlockingPluginProcess::spawn(Path::new("python3"), &demo_plugin(), "zh_CN")
        .expect("spawn worker");
    p.info().expect("info");
    let pid = p.id().expect("worker pid");
    let st = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .expect("kill 进程");
    assert!(st.success(), "kill -9 失败");

    // kill -9 与管道 EOF 之间可能有一瞬竞态：写入仍能进管道缓冲，
    // 但 worker 已死，读取必得 EOF——容许重试直至感知。
    let mut died = false;
    for _ in 0..20 {
        match p.info() {
            Err(Error::WorkerDied) => {
                died = true;
                break;
            }
            Ok(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => panic!("预期 Error::WorkerDied，实际 {e:?}"),
        }
    }
    assert!(died, "worker 被 kill -9 后宿主未感知 EOF");
}
