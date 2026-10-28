//! wechat_ocr 插件真机冒烟（#[ignore]：需本机安装微信 4.x + protobuf）。
//!
//! 运行：`cargo test -p plugins-worker --test worker_wechat -- --ignored --nocapture`
//!
//! 验证链路：info(schema) → init → start（拉起 wxocr 子进程）→ runPath /
//! runBytes（tmpfs 临时文件路径）/ runBase64 → code:100 且识出文字 → stop。
//! 识别用图取 docs/img/main-light.png（应用主界面截图，含大量文字）。

use plugins_worker::{BlockingPluginProcess, base64_encode, plugin_result_to_lines};
use serde_json::json;
use std::path::{Path, PathBuf};

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("工作区根目录")
}

fn wechat_plugin() -> PathBuf {
    project_root().join("plugins/wechat_ocr")
}

fn test_image() -> Vec<u8> {
    std::fs::read(project_root().join("docs/img/main-light.png")).expect("测试图 docs/img/main-light.png")
}

#[test]
#[ignore = "需真机微信 4.x + python3 protobuf；手动运行：cargo test -p plugins-worker --test worker_wechat -- --ignored --nocapture"]
fn wechat_ocr_plugin_full_flow() {
    let img_path = project_root().join("docs/img/main-light.png");
    assert!(img_path.is_file(), "测试图不存在：{}", img_path.display());

    let mut p = BlockingPluginProcess::spawn(Path::new("python3"), &wechat_plugin(), "zh_CN")
        .expect("spawn worker");

    // info：schema 可在无微信/无 protobuf 环境下返回
    let info = p.info().expect("info");
    assert_eq!(info["name"], "wechat_ocr");
    assert_eq!(info["group"], "ocr");
    assert_eq!(info["global_options"]["wechat_dir"]["default"], "");
    assert_eq!(info["local_options"]["timeout"]["default"], 30);

    // init + start：启动 wxocr（自动探测 /opt/wechat-universal 等）；qr_mode=always 附加码结果
    p.init(json!({"wechat_dir": ""})).expect("init");
    let start = p.start(json!({"timeout": 60, "qr_mode": "always"})).expect("start");
    assert_eq!(start["ok"], true, "start 失败：{start}");

    // runPath：绝对路径
    let res = p
        .run_path(&img_path.to_string_lossy())
        .expect("runPath");
    assert_eq!(res["code"], 100, "runPath 非 100：{res}");
    let lines = plugin_result_to_lines(&res).expect("to lines");
    assert!(!lines.is_empty(), "应识出文字");
    let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
    assert!(joined.contains("Rumi"), "主界面标题应被识出，实际：{joined:?}");
    assert!(lines[0].box_.is_some(), "box 应有值");

    // runBytes / runBase64：零拷贝 Shared 通路（PIL 解码 RGBA；无 PIL 回退 tmpfs）
    let png = test_image();
    let res_b = p.run_bytes(&png).expect("runBytes");
    assert_eq!(res_b["code"], 100, "runBytes 非 100：{res_b}");
    let lines_b = plugin_result_to_lines(&res_b).expect("to lines b");
    assert!(!lines_b.is_empty());
    // 零拷贝通路不落盘：/tmp 应无 wechat_ocr_* 临时文件残留
    let tmp_leftover = std::fs::read_dir("/tmp")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("wechat_ocr_"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(tmp_leftover, 0, "runBytes 走了零拷贝通路，不应产生临时文件");

    let res_64 = p.run_base64(&base64_encode(&png)).expect("runBase64");
    assert_eq!(res_64["code"], 100, "runBase64 非 100：{res_64}");

    // 小程序码（VQR_CODE type=22）：样本 test_wxacode.png（无文字 → 码结果输出）
    let wxacode = std::fs::read(wechat_plugin().join("test_wxacode.png"))
        .expect("test_wxacode.png");
    let res_wx = p.run_bytes(&wxacode).expect("runBytes wxacode");
    assert_eq!(res_wx["code"], 100, "wxacode 非 100：{res_wx}");
    let lines_wx = plugin_result_to_lines(&res_wx).expect("to lines wx");
    assert!(!lines_wx.is_empty(), "小程序码样本应返回码结果行");
    let wx_joined: String = lines_wx.iter().map(|l| l.text.as_str()).collect();
    assert!(
        wx_joined.contains("[小程序码]"),
        "应有小程序码标注行，实际：{wx_joined:?}"
    );

    // stop 后应能再次 start（同一 OcrManager 实例复用，任务队列类属性不重建）
    p.stop().expect("stop");
    let start2 = p.start(json!({"timeout": 60, "qr_mode": "always"})).expect("start2");
    assert_eq!(start2["ok"], true, "二次 start 失败：{start2}");
    let res_again = p.run_path(&img_path.to_string_lossy()).expect("run after restart");
    assert_eq!(res_again["code"], 100);
    p.stop().expect("stop2");
}
