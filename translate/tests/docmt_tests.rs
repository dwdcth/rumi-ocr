//! docmt 单测（不打网不跑真模型）：BiStrategy 解析、docforge 双语回写组合
//! 契约（手搓 Document → units → 假译文 → to_bilingual_markdown_with 三形态
//! ——测 rumi 侧组合用法，不重测 docforge 自身）、translate_document 错误
//! 分支与 from==to 全链路（facade 短路原样，零模型零网络）。
//! 真模型 E2E 为 #[ignore]（en_zh 已装时才跑）。

use std::sync::{Arc, Mutex};

use rumi_translate::docmt::{self, BiStrategy, DocTranslateOptions};
use rumi_translate::Engine;

/// 手搓两页文档：各一块 Paragraph（无页名 → 锚点口径 p{页序}-b{块序}）。
fn two_page_doc() -> docforge::Document {
    docforge::Document {
        pages: vec![
            docforge::Page {
                blocks: vec![docforge::Block {
                    kind: docforge::BlockKind::Paragraph,
                    content: "第一页原文句。".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            docforge::Page {
                blocks: vec![docforge::Block {
                    kind: docforge::BlockKind::Paragraph,
                    content: "第二页原文句。".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

/// 假译文：按锚点给固定串（不含原文子串，Only 形态的"原文消失"断言才成立）。
fn fake_translations(units: &[docforge::emit::TranslationUnit]) -> Vec<(String, String)> {
    units
        .iter()
        .map(|u| {
            let t = match u.id.as_str() {
                "p0-b0" => "第一页译文。",
                "p1-b0" => "第二页译文。",
                other => panic!("未知锚点：{other}"),
            };
            (u.id.clone(), t.to_string())
        })
        .collect()
}

#[test]
fn bi_strategy_parse_four_cases() {
    assert_eq!(BiStrategy::parse("inline"), Some(BiStrategy::Inline));
    assert_eq!(BiStrategy::parse("only"), Some(BiStrategy::Only));
    assert_eq!(BiStrategy::parse("appendix"), Some(BiStrategy::Appendix));
    assert_eq!(BiStrategy::parse("bilingual"), None);
}

/// 锚点契约：to_translation_units 从 doc 现算 id，两页各一块 → p0-b0 / p1-b0。
#[test]
fn units_anchor_ids_two_pages() {
    let units = docforge::emit::to_translation_units(&two_page_doc());
    assert_eq!(
        units.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
        ["p0-b0", "p1-b0"]
    );
}

/// Inline：原文 + 译文同段交错（\n 直连不空行），页间空行分隔。
#[test]
fn bilingual_inline_interleaves() {
    let doc = two_page_doc();
    let units = docforge::emit::to_translation_units(&doc);
    let translated = fake_translations(&units);
    let md = docforge::emit::to_bilingual_markdown_with(
        &doc,
        &translated,
        docforge::emit::BiStrategy::BilingualInline,
    );
    assert_eq!(
        md,
        "第一页原文句。\n第一页译文。\n\n第二页原文句。\n第二页译文。"
    );
}

/// Only：有译文的块文本整体替换，原文不再出现。
#[test]
fn translation_only_replaces() {
    let doc = two_page_doc();
    let units = docforge::emit::to_translation_units(&doc);
    let translated = fake_translations(&units);
    let md = docforge::emit::to_bilingual_markdown_with(
        &doc,
        &translated,
        docforge::emit::BiStrategy::TranslationOnly,
    );
    assert_eq!(md, "第一页译文。\n\n第二页译文。");
    assert!(!md.contains("原文句"), "原文被译文替换：{md}");
}

/// Appendix：正文为完整原文，文末追加译文附录（无 Heading → 归（无章节））。
#[test]
fn original_plus_appendix() {
    let doc = two_page_doc();
    let units = docforge::emit::to_translation_units(&doc);
    let translated = fake_translations(&units);
    let md = docforge::emit::to_bilingual_markdown_with(
        &doc,
        &translated,
        docforge::emit::BiStrategy::OriginalPlusAppendix,
    );
    assert!(md.starts_with("第一页原文句。\n\n第二页原文句。"), "正文应是完整原文：{md}");
    assert!(
        md.ends_with("---\n\n# 译文\n\n## （无章节）\n\n第一页译文。\n\n第二页译文。"),
        "文末应追加按章分组的译文附录：{md}"
    );
}

/// 错误分支：文件不存在（不触发解析，Err 含"文件不存在"，不产输出文件）。
#[test]
fn missing_file_errors_before_parse() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("nope.txt");
    let out = dir.path().join("out.md");
    let opts = DocTranslateOptions {
        engine: Engine::Google,
        from: "en".into(),
        to: "zh".into(),
        strategy: BiStrategy::Inline,
        polish: false,
        min_chars: 2,
        on_progress: None,
    };
    let err = docmt::translate_document(&input, &out, &opts).unwrap_err();
    assert!(err.contains("文件不存在"), "实际：{err}");
    assert!(!out.exists(), "失败不应产出文件");
}

/// 错误分支：可解析但无可翻译单元（空白 txt）→ Err 固定文案。
/// 引擎选 Google 仅为占位——units 为空的判定先于任何翻译调用，不触网。
#[test]
fn empty_units_errors() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("blank.txt");
    std::fs::write(&input, "   \n\n").unwrap();
    let out = dir.path().join("out.md");
    let opts = DocTranslateOptions {
        engine: Engine::Google,
        from: "en".into(),
        to: "zh".into(),
        strategy: BiStrategy::Inline,
        polish: false,
        min_chars: 2,
        on_progress: None,
    };
    let err = docmt::translate_document(&input, &out, &opts).unwrap_err();
    assert!(err.contains("文档无可翻译文本单元"), "实际：{err}");
}

/// 全链路（零模型零网络）：from==to 走 facade 短路原样返回——覆盖
/// parse → units → 逐单元翻译 → 回写落盘 → 统计/进度的完整管线。
#[test]
fn full_pipeline_from_eq_to_passes_through() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("doc.txt");
    std::fs::write(&input, "这是一段测试正文。\n\n这是另一段正文。\n").unwrap();
    let out = dir.path().join("doc.bilingual.md");
    let calls: Arc<Mutex<Vec<(usize, usize)>>> = Arc::default();
    let sink = calls.clone();
    let opts = DocTranslateOptions {
        engine: Engine::Offline, // from==to 短路，shared() 不会被触达
        from: "zh".into(),
        to: "zh".into(),
        strategy: BiStrategy::Inline,
        polish: false,
        min_chars: 2,
        on_progress: Some(Arc::new(move |done, total| {
            sink.lock().unwrap().push((done, total));
        })),
    };
    let stats = docmt::translate_document(&input, &out, &opts).unwrap();
    assert_eq!(stats.units, 2);
    assert_eq!(
        stats.chars,
        "这是一段测试正文。".chars().count() + "这是另一段正文。".chars().count()
    );
    // Inline：原文后紧跟"译文"（此处即原文自身）
    let md = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        md,
        "这是一段测试正文。\n这是一段测试正文。\n\n这是另一段正文。\n这是另一段正文。"
    );
    // 2 单元 < 16：进度只在末批完成点回调一次，且到达 (total, total)
    assert_eq!(*calls.lock().unwrap(), vec![(2, 2)]);
}

/// 真模型 E2E：en_zh 已装时，最小 txt（docforge 文本插件可解析）→
/// 双语 md（存在、非空、含中文）。
#[test]
#[ignore = "en_zh 已装时跑真模型（doc-translate-spec §4）"]
fn e2e_doc_translate_txt_en_zh() {
    use rumi_translate::{download, registry};
    let spec = registry::by_key("en_zh").expect("注册表应含 en_zh");
    if !download::is_complete(spec, &rumi_translate::models_dir()) {
        eprintln!("skip：en_zh 未安装");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("sample.txt");
    std::fs::write(
        &input,
        "The default port is 8080.\n\nSet the environment variable first.\n",
    )
    .unwrap();
    let out = dir.path().join("sample.bilingual.md");
    let stats = docmt::translate_document(
        &input,
        &out,
        &DocTranslateOptions {
            engine: Engine::Offline,
            from: "en".into(),
            to: "zh".into(),
            strategy: BiStrategy::Inline,
            polish: false,
            min_chars: 2,
            on_progress: None,
        },
    )
    .unwrap();
    assert_eq!(stats.units, 2);
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(!md.trim().is_empty(), "输出应非空");
    assert!(
        md.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)),
        "应含中文：{md}"
    );
}
