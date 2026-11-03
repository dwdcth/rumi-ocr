//! E2E：真实下载 / 真实推理 / 真实在线请求，全部 #[ignore]——留给监工复验。
//!
//! 运行：`RUMI_E2E=1 cargo test -p rumi-translate --test e2e -- --ignored`
//! 需要网络（argos-net.com 主源，huggingface.co / hf-mirror.com 备源；
//! google 用例需可达 translate.googleapis.com）。
//!
//! 按规格 §3 注记：离线用例直接 new OfflineEngine（不走 facade——
//! shared() 的 models_dir 在进程内固化，与临时目录对不上）；google 用例走 facade，无需模型。

use rumi_translate::download::{self, Mirrors};
use rumi_translate::engine::OfflineEngine;
use rumi_translate::{registry, Compute, Engine};

/// en_zh 离线：装模型 → 翻译 → 端口号必须原样保留。
#[test]
#[ignore = "真实下载 en_zh 模型（argos 直链约 71MB / HF 备源约 157MB）"]
fn e2e_en_zh_offline() {
    let dir = std::env::temp_dir().join("rumi-translate-e2e-en_zh");
    let spec = registry::by_key("en_zh").expect("注册表应含 en_zh");
    if !download::is_complete(spec, &dir) {
        download::install(spec, &dir, &Mirrors::default(), &|_| {}).unwrap();
    }
    let eng = OfflineEngine::new(dir.clone(), Compute::Int8);
    let out = eng
        .translate(
            "The default port is 8080. Set the environment variable first.",
            "en",
            "zh",
        )
        .unwrap();
    assert!(out.contains("8080"), "译文应保留端口号：{out}");
}

/// zh_en 离线：同构用例（中文 → 英文）。
#[test]
#[ignore = "真实下载 zh_en 模型（argos 直链约 71MB / HF 备源约 157MB）"]
fn e2e_zh_en_offline() {
    let dir = std::env::temp_dir().join("rumi-translate-e2e-zh_en");
    let spec = registry::by_key("zh_en").expect("注册表应含 zh_en");
    if !download::is_complete(spec, &dir) {
        download::install(spec, &dir, &Mirrors::default(), &|_| {}).unwrap();
    }
    let eng = OfflineEngine::new(dir.clone(), Compute::Int8);
    let out = eng
        .translate(
            "请在启动 Docker 容器前设置环境变量。默认端口为 8080。",
            "zh",
            "en",
        )
        .unwrap();
    assert!(out.contains("8080"), "译文应保留端口号：{out}");
}

/// google 在线：不依赖本地模型；polish=true 顺带覆盖 facade 的润色路径。
#[test]
#[ignore = "需可访问 translate.googleapis.com"]
fn e2e_google_online() {
    let out = rumi_translate::translate(
        &["The default port is 8080.".to_string()],
        "en",
        "zh",
        &Engine::Google,
        true,
    )
    .unwrap();
    assert!(!out[0].is_empty());
}

/// NLLB 多语言：装 nllb_600m（HF 约 622MB）→ zh→ja 翻译（target-prefix 路线）
/// 端口号必须原样保留。zh_en 等精确方向包未装，才会落到 NLLB 兜底解析。
#[test]
#[ignore = "真实下载 NLLB 模型（HF JustFrederik/nllb-200-distilled-600M-ct2-int8 约 593MB）"]
fn e2e_nllb_zh_ja_offline() {
    let dir = std::env::temp_dir().join("rumi-translate-e2e-nllb");
    let spec = registry::by_key("nllb_600m").expect("注册表应含 nllb_600m");
    if !download::is_complete(spec, &dir) {
        download::install(spec, &dir, &Mirrors::default(), &|_| {}).unwrap();
    }
    let eng = OfflineEngine::new(dir.clone(), Compute::Int8);
    let out = eng
        .translate("请在启动容器前设置环境变量。默认端口为 8080。", "zh", "ja")
        .unwrap();
    assert!(out.contains("8080"), "译文应保留端口号：{out}");
}

/// 目录条目安装：官方 index.json 拉取（fetch_catalog）→ 装 en_ja（argos 直链
/// 约 115MB）→ en→ja 翻译。单测不打网，install_catalog_entry 的真实路径在此补齐；
/// en→ja 无内置/注册表包，走导入包（manifest from/to）解析。
#[test]
#[ignore = "真实拉取官方目录并下载 en_ja 模型（argos 直链约 115MB）"]
fn e2e_catalog_en_ja_offline() {
    let dir = std::env::temp_dir().join("rumi-translate-e2e-catalog");
    let list = download::fetch_catalog(&Mirrors::default()).expect("官方目录应可拉取");
    let entry = list.iter().find(|e| e.key == "en_ja").expect("官方目录应含 en_ja");
    assert_eq!((entry.from.as_str(), entry.to.as_str(), entry.version.as_str()), ("en", "ja", "1.1"));
    download::install_catalog_entry(entry, &dir, &Mirrors::default(), &|_| {}).unwrap();
    let eng = OfflineEngine::new(dir.clone(), Compute::Int8);
    let out = eng
        .translate("The default port is 8080. Set the environment variable first.", "en", "ja")
        .unwrap();
    assert!(out.contains("8080"), "译文应保留端口号：{out}");
}
