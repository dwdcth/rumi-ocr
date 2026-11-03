//! 文档翻译：docforge 解析 → 翻译单元 → 引擎翻译 → 双语 Markdown 回写。
//!
//! v1 链路（doc-translate-spec §2）：任意 docforge 支持格式进、双语 Markdown
//! 出；docx/epub 回写上游到位后只加出口不改链路。解析不注入 OCR 引擎——
//! 扫描页 PDF 由 pdf 后端预检报错，本层转译为面向用户的行动指引（见
//! [`translate_parse_error`]）。

use std::path::Path;

/// 双语策略（Inline/Only/Appendix 镜像 docforge::emit::BiStrategy；
/// PageAlt 为 rumi 层扩展，仅 markdown 输出完整实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BiStrategy {
    /// 原/译同段交错（原文块后紧跟译文块）——阅读对照形态。
    #[default]
    Inline,
    /// 仅译文（替换原文块文本）。
    Only,
    /// 原文完整 + 文末追加全部译文附录（按章节分组）。
    Appendix,
    /// 页级交替：一页原文接一页译文（页间 `---` 分隔）。
    /// 仅 **markdown 输出**完整实现；无页概念的格式（docx/odt 渲染时才分页、
    /// epub 页=spine 部件需动 OPF 结构）退化为 Appendix（原文全文+译文附录
    /// 的文档级两段式最近似），DocStats.note 携带退化说明。
    PageAlt,
}

impl BiStrategy {
    /// CLI/UI 字符串 → 策略；未知 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inline" => Some(BiStrategy::Inline),
            "only" => Some(BiStrategy::Only),
            "appendix" => Some(BiStrategy::Appendix),
            "page" | "pagealt" => Some(BiStrategy::PageAlt),
            _ => None,
        }
    }
}

impl From<BiStrategy> for docforge::emit::BiStrategy {
    fn from(s: BiStrategy) -> Self {
        match s {
            BiStrategy::Inline => docforge::emit::BiStrategy::BilingualInline,
            BiStrategy::Only => docforge::emit::BiStrategy::TranslationOnly,
            // PageAlt 的 docforge 侧映射 = Appendix（不支持页交替的格式的退化目标；
            // markdown 的页交替在 docmt 层自行拼接，不走该映射）
            BiStrategy::Appendix | BiStrategy::PageAlt => {
                docforge::emit::BiStrategy::OriginalPlusAppendix
            }
        }
    }
}

/// 文档翻译选项。
pub struct DocTranslateOptions {
    pub engine: crate::Engine,
    /// 内部码；单元级不自动检测（unit.lang 留 v2）。
    pub from: String,
    pub to: String,
    pub strategy: BiStrategy,
    /// 与 facade 同义（zh 目标时润色）。
    pub polish: bool,
    /// 最小字符数（docforge 同款默认 2：滤单字符噪声）。
    pub min_chars: usize,
    /// 进度回调 (已翻译单元数, 总单元数)；调用方取消语义 v1 不做
    /// （返回 false 中止留 v2）。
    pub on_progress: Option<std::sync::Arc<dyn Fn(usize, usize) + Send + Sync>>,
}

/// 文档翻译统计。
#[derive(Debug)]
pub struct DocStats {
    /// 翻译单元数。
    pub units: usize,
    /// 送译原文总字符数（单元 text 按 chars 计）。
    pub chars: usize,
    pub elapsed_ms: u128,
    /// 策略退化说明（如 PageAlt 遇无页概念格式退化为附录时的提示）；
    /// None = 策略按所选完整生效。UI/CLI 在完成态展示。
    pub note: Option<String>,
}

/// 进度回调节流：每 16 单元回调一次（末批不足也回调），UI 不必逐单元重绘。
const UNITS_PER_BATCH: usize = 16;

/// 主入口：input（任意 docforge 支持格式）→ out 双语成品。
///
/// **输出格式按 out 扩展名分派**（大小写不敏感）：`.md`/`.markdown` → 双语
/// Markdown；`.docx` → docforge 定位改写回写（保原样式/资源，仅动正文 XML，
/// 原件为 docx）；`.epub` → epub 回写（原件为 epub）。其他扩展名 Err。
/// docx/epub 回写由 docforge `writeback::*` 重解析原文件对齐 id 锚点
/// （与本函数提取侧同口径），translation 单元 id 不漂移。
///
/// 流程：parse → to_translation_units_with(min_chars) → 逐单元调
/// [`crate::translate`]（facade 自带 from==to 原样、空串原样、zh 润色与
/// ▁ 清理；离线引擎内部有 Translator 批缓存，量级可接受）→ 每 16 单元/批
/// 回调 on_progress → 回写落盘。任一单元失败即整体 Err（"单元 {id} 翻译失败"）。
pub fn translate_document(
    input: &Path,
    out: &Path,
    opts: &DocTranslateOptions,
) -> Result<DocStats, String> {
    let t0 = std::time::Instant::now();
    // 文件存在性预检先于解析：确定性结论统一口径（pdf 后端对缺文件报
    // "文件不存在"，文本族则报 cannot read——docmt 层先行拦截）
    if !input.is_file() {
        return Err(format!("文件不存在：{}", input.display()));
    }
    let doc = parse_document(input)?;
    let units = docforge::emit::to_translation_units_with(&doc, opts.min_chars);
    if units.is_empty() {
        return Err("文档无可翻译文本单元（空文件或全为非文本块）".to_string());
    }
    let total = units.len();
    let mut translated: Vec<(String, String)> = Vec::with_capacity(total);
    let mut chars = 0usize;
    for (done, unit) in units.iter().enumerate() {
        let text = crate::translate(
            std::slice::from_ref(&unit.text),
            &opts.from,
            &opts.to,
            &opts.engine,
            opts.polish,
        )
        .map_err(|e| format!("单元 {} 翻译失败：{}", unit.id, e))?
        .pop()
        .unwrap_or_default();
        chars += unit.text.chars().count();
        translated.push((unit.id.clone(), text));
        let done = done + 1;
        if done % UNITS_PER_BATCH == 0 || done == total {
            if let Some(cb) = &opts.on_progress {
                cb(done, total);
            }
        }
    }
    // 输出分派（见函数文档）。扩展名小写比较，无扩展名按不支持处理
    // （CLI/UI 的默认输出恒带 .md，走到这里的无扩展名只会是显式误用）
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    // 保原格式回写要求输入与输出同格式（writeback 重解析原文件定位改写）；
    // 跨格式需求（如 txt→docx）一律走 .md 输出。前置友好报错替代深层 ZIP 错误。
    let in_ext = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let needs_same_input = matches!(
        ext.as_str(),
        "docx" | "epub" | "txt" | "html" | "htm" | "fb2" | "odt"
    );
    if needs_same_input && in_ext != ext {
        return Err(format!(
            "输出 .{ext} 需要同格式输入（当前输入 .{in_ext}）；跨格式请输出 .md"
        ));
    }
    // PageAlt 的格式退化说明（仅 markdown 完整支持页级交替，见 BiStrategy）
    let note = if opts.strategy == BiStrategy::PageAlt && !matches!(ext.as_str(), "md" | "markdown") {
        Some(format!(
            ".{ext} 无固定页概念（Word 渲染时才分页、EPUB 页为部件），已按「原文全文 + 译文附录」输出"
        ))
    } else {
        None
    };
    match ext.as_str() {
        "md" | "markdown" => {
            let md = if opts.strategy == BiStrategy::PageAlt {
                page_alternating_markdown(&doc, &translated)
            } else {
                docforge::emit::to_bilingual_markdown_with(&doc, &translated, opts.strategy.into())
            };
            std::fs::write(out, md).map_err(|e| format!("写入 {} 失败：{e}", out.display()))?;
        }
        "docx" => docforge::writeback::writeback_docx(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("docx 回写失败：{e}"))?,
        "epub" => docforge::writeback::writeback_epub(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("epub 回写失败：{e}"))?,
        // 纯文本/标记族回写（docforge@82cdbb46 起；txt 恒开、html 随 default、
        // fb2/odt 已在本 crate feature 集中开启）
        "txt" => docforge::writeback::writeback_txt(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("txt 回写失败：{e}"))?,
        "html" | "htm" => docforge::writeback::writeback_html(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("html 回写失败：{e}"))?,
        "fb2" => docforge::writeback::writeback_fb2(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("fb2 回写失败：{e}"))?,
        "odt" => docforge::writeback::writeback_odt(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("odt 回写失败：{e}"))?,
        other => {
            return Err(format!(
                "不支持的输出格式 .{other}（可选 .md / .docx / .epub / .txt / .html / .fb2 / .odt）"
            ))
        }
    }
    Ok(DocStats {
        units: total,
        chars,
        elapsed_ms: t0.elapsed().as_millis(),
        note,
    })
}

/// 页级交替 Markdown（BiStrategy::PageAlt 的 markdown 实现）：
/// 每页渲染原文页 + 译文页（译文页用 TranslationOnly 语义），页间 `---` 分隔。
///
/// 实现要点：逐页构造**单页 Document** 复用 docforge 的块渲染（heading/quote/
/// list/table 不复刻）。id 锚点不漂移的关键：`translation_id` 的页号取
/// `Page.name`（无名为页序）——切单页后页序恒 0，因此对无名页把原页序写进
/// `name`（`Some("2")` → id 仍为 `p2-bN`），有名页原样保留。译文侧传全量
/// `translated`（to_bilingual_markdown_with 按 id 精确匹配，跨页条目自然忽略）。
fn page_alternating_markdown(doc: &docforge::Document, translated: &[(String, String)]) -> String {
    let mut pages: Vec<String> = Vec::with_capacity(doc.pages.len());
    for (idx, page) in doc.pages.iter().enumerate() {
        let mut single = doc.clone();
        single.pages = vec![page.clone()];
        // 无名页补页序为 name，保持 id 锚点与全量文档一致（见函数文档）
        if single.pages[0].name.is_none() {
            single.pages[0].name = Some(idx.to_string());
        }
        let original = docforge::emit::to_markdown(&single);
        let translated_page = docforge::emit::to_bilingual_markdown_with(
            &single,
            translated,
            docforge::emit::BiStrategy::TranslationOnly,
        );
        // 组内（原文页 ↔ 该页译文）空行分隔；组间（页 ↔ 页）用 --- 分隔
        pages.push(format!("{original}\n\n{translated_page}"));
    }
    pages.join("\n\n---\n\n")
}

/// docforge 解析（文档翻译场景装配）：文本族内置 + docx/epub/pdf（feature
/// 随本 crate 依赖声明启用）。pdf 显式 `PdfOptions{mode: Auto, engine: None}`
/// ——与 docforge 默认装配同款（发布版不内置引擎），写出以固定 v1 契约：
/// 纯文字层 PDF 正常直提；扫描页由 pdf 后端预检报错（无引擎时 Auto 不产
/// 空页而是 Err，见 [`translate_parse_error`]）。
fn parse_document(input: &Path) -> Result<docforge::Document, String> {
    let path = input
        .to_str()
        .ok_or_else(|| format!("路径非 UTF-8，无法解析：{}", input.display()))?;
    let mut registry = docforge::Registry::new();
    docforge::plugins::text::register_defaults(&mut registry);
    docforge::backends::office::register_defaults(&mut registry);
    docforge::backends::epub::register_defaults(&mut registry);
    docforge::backends::pdf::register(
        &mut registry,
        docforge::backends::pdf::PdfOptions {
            mode: docforge::backends::pdf::OcrMode::Auto,
            engine: None,
            ..Default::default()
        },
    );
    docforge::DocumentEngine::new(registry)
        .parse(path)
        .map_err(translate_parse_error)
}

/// docforge 解析错误 → 面向用户的中文文案。扫描页无引擎是 v1 已知边界：
/// pdf 后端预检（先于任何页产出）报 "第 N 页需 OCR 引擎：…"，转译为行动指引。
fn translate_parse_error(e: docforge::DocError) -> String {
    if e.message.contains("需 OCR 引擎") {
        "含扫描页且未注入 OCR 引擎（v1 暂不支持扫描 PDF 翻译，请转图片批量 OCR）".to_string()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod page_alt_tests {
    use super::page_alternating_markdown;
    use docforge::model::{Block, BlockKind, Document, Page};

    fn para(text: &str) -> Block {
        Block {
            content: text.to_string(),
            kind: BlockKind::Paragraph,
            ..Default::default()
        }
    }

    /// 手搓两页（无名页，验证 name 补页序的 id 锚点稳定性）：
    /// 输出应为 页1原文 → 页1译文 → --- → 页2原文 → 页2译文。
    #[test]
    fn alternates_pages_with_separator() {
        let doc = Document {
            pages: vec![
                Page { blocks: vec![para("Alpha first page paragraph.")], ..Default::default() },
                Page { blocks: vec![para("Beta second page paragraph.")], ..Default::default() },
            ],
            ..Default::default()
        };
        // id 锚点：无名页 → 页序（p0-b0 / p1-b0）；译文前缀 T- 便于断言归属
        let translated = vec![
            ("p0-b0".to_string(), "T-alpha 第一页译文".to_string()),
            ("p1-b0".to_string(), "T-beta 第二页译文".to_string()),
        ];
        let md = page_alternating_markdown(&doc, &translated);
        let p1o = md.find("Alpha first page").expect("页1原文");
        let p1t = md.find("T-alpha").expect("页1译文");
        let sep = md.find("\n---\n").expect("页分隔线");
        let p2o = md.find("Beta second page").expect("页2原文");
        let p2t = md.find("T-beta").expect("页2译文");
        assert!(p1o < p1t && p1t < sep && sep < p2o && p2o < p2t,
            "应为 页1原文→页1译文→---→页2原文→页2译文：\n{md}");
    }

    /// 有名页（epub 部件名）name 保留原样，id 不漂移。
    #[test]
    fn named_page_keeps_anchor() {
        let doc = Document {
            pages: vec![Page {
                name: Some("ch3.xhtml".to_string()),
                blocks: vec![para("Named page body text.")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let translated = vec![("pch3.xhtml-b0".to_string(), "T-有名页译文".to_string())];
        let md = page_alternating_markdown(&doc, &translated);
        assert!(md.contains("T-有名页译文"), "有名页锚点应匹配：\n{md}");
        assert!(md.contains("Named page body text."));
    }
}
