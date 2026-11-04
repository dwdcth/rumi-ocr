//! docmt 单测（不打网不跑真模型）：BiStrategy 解析、docforge 双语回写组合
//! 契约（手搓 Document → units → 假译文 → to_bilingual_markdown_with 三形态
//! ——测 rumi 侧组合用法，不重测 docforge 自身）、translate_document 错误
//! 分支与 from==to 全链路（facade 短路原样，零模型零网络）。
//! 真模型 E2E 为 #[ignore]（en_zh 已装时才跑）。
//! 既有构造点全部 use_cache: false——测试不碰用户真实缓存目录
//! （缓存逻辑单测见 cache_tests.rs，同样走 load_at(tempdir)）。

use std::sync::{Arc, Mutex};

use rumi_translate::docmt::{self, BiStrategy, DocTranslateOptions};
use rumi_translate::glossary::Glossary;
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
        use_cache: false,
        glossary: Glossary::default(),
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
        use_cache: false,
        glossary: Glossary::default(),
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
        use_cache: false,
        glossary: Glossary::default(),
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
            use_cache: false,
            glossary: Glossary::default(),
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

// ---------- 输出格式分派（docx/epub 回写，docforge@616fc626 起） ----------

/// 现场构造最小可用 docx（writeback 解析侧读 word/document.xml，
/// [Content_Types] 与 rels 为标准件）。段落文本取 ≥2 字符的合格翻译位。
fn build_minimal_docx(dir: &std::path::Path, para: &str) -> std::path::PathBuf {
    use std::io::Write;
    let path = dir.join("in.docx");
    let file = std::fs::File::create(&path).unwrap();
    let mut z = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = Default::default();
    z.add_directory("_rels", opts).unwrap();
    z.add_directory("word", opts).unwrap();
    z.start_file("[Content_Types].xml", opts).unwrap();
    z.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
    )
    .unwrap();
    z.start_file("_rels/.rels", opts).unwrap();
    z.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#,
    )
    .unwrap();
    z.start_file("word/document.xml", opts).unwrap();
    let doc_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>{para}</w:t></w:r></w:p><w:p><w:r><w:t>Second plain paragraph here.</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    z.write_all(doc_xml.as_bytes()).unwrap();
    z.finish().unwrap();
    path
}

/// docx 回写端到端（from==to 直通当"翻译"，零模型依赖；译文=原文，
/// writeback 文本一致性双保险天然通过）。Inline 策略下输出 docx 的
/// document.xml 应含两份段落（原块 + 插入的译文块）。
#[test]
fn docx_writeback_dispatch_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let text = "Hello world this is a test paragraph.";
    let input = build_minimal_docx(dir.path(), text);
    let out = dir.path().join("out.bilingual.docx");
    docmt::translate_document(
        &input,
        &out,
        &DocTranslateOptions {
            engine: Engine::Offline,
            from: "en".into(),
            to: "en".into(), // 直通：译文 == 原文，链路零模型
            strategy: BiStrategy::Inline,
            polish: false,
            min_chars: 2,
            on_progress: None,
            use_cache: false,
            glossary: Glossary::default(),
        },
    )
    .unwrap();
    assert!(out.is_file(), "输出 docx 应生成");
    // 读回 zip 的 document.xml：Inline 语义 = 原段 + 译文段两份
    use std::io::Read as _;
    let f = std::fs::File::open(&out).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut buf = Vec::new();
    z.by_name("word/document.xml").unwrap().read_to_end(&mut buf).unwrap();
    let xml = String::from_utf8(buf).unwrap();
    let hits = xml.matches("Hello world this is a test paragraph.").count();
    assert!(hits >= 2, "Inline 应含原块+译文块两份，实得 {hits}：{xml}");
}

/// 不支持的输出扩展名 → 明确报错（不落盘）。
#[test]
fn unsupported_output_extension_errors() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.txt");
    std::fs::write(&input, "Hello world paragraph.").unwrap();
    let err = docmt::translate_document(
        &input,
        &dir.path().join("out.bilingual.xyz"),
        &DocTranslateOptions {
            engine: Engine::Offline,
            from: "en".into(),
            to: "en".into(),
            strategy: BiStrategy::Inline,
            polish: false,
            min_chars: 2,
            on_progress: None,
            use_cache: false,
            glossary: Glossary::default(),
        },
    )
    .unwrap_err();
    assert!(err.contains("不支持的输出格式"), "实得：{err}");
}


/// txt 回写分派端到端（from==to 直通，零模型）：Inline 下输出 txt 应含
/// 原文两份（原块 + 插入译文块=原文）。
#[test]
fn txt_writeback_dispatch_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.txt");
    std::fs::write(&input, "Hello world this is a test paragraph.\n").unwrap();
    let out = dir.path().join("out.bilingual.txt");
    docmt::translate_document(
        &input,
        &out,
        &DocTranslateOptions {
            engine: Engine::Offline,
            from: "en".into(),
            to: "en".into(),
            strategy: BiStrategy::Inline,
            polish: false,
            min_chars: 2,
            on_progress: None,
            use_cache: false,
            glossary: Glossary::default(),
        },
    )
    .unwrap();
    let txt = std::fs::read_to_string(&out).unwrap();
    let hits = txt.matches("Hello world this is a test paragraph.").count();
    assert!(hits >= 2, "Inline 应含原块+译文块两份，实得 {hits}：{txt}");
}

// ---------- 页级交替（PageAlt，rumi 层策略） ----------

/// PageAlt 遇无页概念格式（txt 输出）：退化为附录（原文+译文），note 携带说明。
#[test]
fn page_alt_falls_back_with_note_for_pageless_format() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.txt");
    std::fs::write(&input, "Hello world this is a paragraph.\n").unwrap();
    let out = dir.path().join("out.bilingual.txt");
    let stats = docmt::translate_document(
        &input,
        &out,
        &DocTranslateOptions {
            engine: Engine::Offline,
            from: "en".into(),
            to: "en".into(),
            strategy: BiStrategy::PageAlt,
            polish: false,
            min_chars: 2,
            on_progress: None,
            use_cache: false,
            glossary: Glossary::default(),
        },
    )
    .unwrap();
    let note = stats.note.expect("txt 页交替应携带退化说明");
    assert!(note.contains("附录"), "实得：{note}");
    assert!(out.is_file());
}

/// BiStrategy::parse 含 page 变体。
#[test]
fn bi_strategy_parse_includes_page() {
    assert_eq!(BiStrategy::parse("page"), Some(BiStrategy::PageAlt));
    assert_eq!(BiStrategy::parse("pagealt"), Some(BiStrategy::PageAlt));
    assert_eq!(BiStrategy::parse("inline"), Some(BiStrategy::Inline));
}

// ---------- 缓存命中端到端（use_cache: true，规格 §3 决策：#[ignore] 手动跑） ----------

/// 断点续翻端到端：同一输入连跑两遍，第一遍全引擎（cache_hits=0、缓存落盘），
/// 第二遍全命中（cache_hits==units，进度照常推进）。需 en_zh 已装且测试进程
/// 带 RUMI_TRANSLATE_CACHE=<tempdir>（测试禁止触碰用户真实缓存目录，env 未设
/// 即跳过）：
/// `RUMI_TRANSLATE_CACHE=/tmp/某临时目录 cargo test -p rumi-translate \
///  --test docmt_tests cache_hit_e2e -- --ignored --nocapture`
#[test]
#[ignore = "手动跑：需 en_zh 模型 + RUMI_TRANSLATE_CACHE=<tempdir>（见测试文档）"]
fn cache_hit_e2e_second_run_all_hits() {
    use rumi_translate::{download, registry};
    let Ok(cache_dir) = std::env::var("RUMI_TRANSLATE_CACHE") else {
        eprintln!("skip：未设置 RUMI_TRANSLATE_CACHE（不碰用户真实缓存）");
        return;
    };
    let spec = registry::by_key("en_zh").expect("注册表应含 en_zh");
    if !download::is_complete(spec, &rumi_translate::models_dir()) {
        eprintln!("skip：en_zh 未安装");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("doc.txt");
    std::fs::write(&input, "The default port is 8080.\n\nSet the environment variable first.\n")
        .unwrap();
    let opts = DocTranslateOptions {
        engine: Engine::Offline,
        from: "en".into(),
        to: "zh".into(),
        strategy: BiStrategy::Inline,
        polish: false,
        min_chars: 2,
        on_progress: None,
        use_cache: true,
        glossary: Glossary::default(),
    };
    let out1 = dir.path().join("first.md");
    let first = docmt::translate_document(&input, &out1, &opts).unwrap();
    assert_eq!(first.cache_hits, 0, "首翻应零命中");
    assert!(
        std::path::Path::new(&cache_dir).is_file(),
        "首翻后缓存文件应已落盘（含批边界与收尾 save）"
    );
    let out2 = dir.path().join("second.md");
    let second = docmt::translate_document(&input, &out2, &opts).unwrap();
    assert_eq!(
        second.cache_hits, second.units,
        "重跑应全命中（断点续翻：中断重跑零引擎成本）"
    );
    assert_eq!(second.units, 2);
    assert_eq!(
        std::fs::read_to_string(&out1).unwrap(),
        std::fs::read_to_string(&out2).unwrap(),
        "命中路径与引擎路径产出应一致"
    );
}

// ---------- 术语表（glossary 规格 §6）：from==to 直通 + 精确命中 ----------

/// 术语表精确命中端到端（不打网不跑模型）：判定链顺序铁律——glossary 插在
/// 防幻觉之后、running memo/缓存/引擎之前。from==to 的直通短路发生在
/// crate::translate（facade）内部，晚于本判定：术语单元照样回填固定译法
/// （glossary_hits==1），非术语单元原样直通；直通路径不经过缓存。
#[test]
fn glossary_exact_hit_beats_from_eq_to_passthrough() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("doc.txt");
    std::fs::write(&input, "thread pool\n\n另一段普通正文。\n").unwrap();
    let out = dir.path().join("doc.bilingual.md");
    let opts = DocTranslateOptions {
        engine: Engine::Offline, // from==to 短路，shared() 不会被触达
        from: "zh".into(),
        to: "zh".into(),
        strategy: BiStrategy::Inline,
        polish: false,
        min_chars: 2,
        on_progress: None,
        use_cache: false,
        glossary: Glossary::parse("thread pool = 线程池"),
    };
    let stats = docmt::translate_document(&input, &out, &opts).unwrap();
    assert_eq!(stats.units, 2);
    assert_eq!(stats.glossary_hits, 1, "仅术语单元应精确命中");
    assert_eq!(stats.cache_hits, 0, "直通路径不经过缓存");
    let md = std::fs::read_to_string(&out).unwrap();
    // Inline 形态：命中单元 = 原文 + 固定译文；未命中单元 = 原样直通
    assert!(
        md.contains("thread pool\n线程池"),
        "术语单元应回填固定译法：\n{md}"
    );
    assert!(
        md.contains("另一段普通正文。\n另一段普通正文。"),
        "非术语单元应原样直通：\n{md}"
    );
}
