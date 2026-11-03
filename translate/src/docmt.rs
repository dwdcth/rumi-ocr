//! 文档翻译：docforge 解析 → 翻译单元 → 引擎翻译 → 双语 Markdown 回写。
//!
//! v1 链路（doc-translate-spec §2）：任意 docforge 支持格式进、双语 Markdown
//! 出；docx/epub 回写上游到位后只加出口不改链路。解析不注入 OCR 引擎——
//! 扫描页 PDF 由 pdf 后端预检报错，本层转译为面向用户的行动指引（见
//! [`translate_parse_error`]）。

use std::path::Path;

/// 双语策略（镜像 docforge::emit::BiStrategy）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BiStrategy {
    /// 原/译同段交错（原文块后紧跟译文块）——阅读对照形态。
    #[default]
    Inline,
    /// 仅译文（替换原文块文本）。
    Only,
    /// 原文完整 + 文末追加全部译文附录（按章节分组）。
    Appendix,
}

impl BiStrategy {
    /// CLI/UI 字符串 → 策略；未知 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inline" => Some(BiStrategy::Inline),
            "only" => Some(BiStrategy::Only),
            "appendix" => Some(BiStrategy::Appendix),
            _ => None,
        }
    }
}

impl From<BiStrategy> for docforge::emit::BiStrategy {
    fn from(s: BiStrategy) -> Self {
        match s {
            BiStrategy::Inline => docforge::emit::BiStrategy::BilingualInline,
            BiStrategy::Only => docforge::emit::BiStrategy::TranslationOnly,
            BiStrategy::Appendix => docforge::emit::BiStrategy::OriginalPlusAppendix,
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
}

/// 进度回调节流：每 16 单元回调一次（末批不足也回调），UI 不必逐单元重绘。
const UNITS_PER_BATCH: usize = 16;

/// 主入口：input（任意 docforge 支持格式）→ out_md 双语 Markdown。
///
/// 流程：parse → to_translation_units_with(min_chars) → 逐单元调
/// [`crate::translate`]（facade 自带 from==to 原样、空串原样、zh 润色与
/// ▁ 清理；离线引擎内部有 Translator 批缓存，量级可接受）→ 每 16 单元/批
/// 回调 on_progress → 回写落盘。任一单元失败即整体 Err（"单元 {id} 翻译失败"）。
pub fn translate_document(
    input: &Path,
    out_md: &Path,
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
    // doc 需在函数内持有（units 从它出、回写消费同一份，锚点不漂移）
    let md = docforge::emit::to_bilingual_markdown_with(&doc, &translated, opts.strategy.into());
    std::fs::write(out_md, md).map_err(|e| format!("写入 {} 失败：{e}", out_md.display()))?;
    Ok(DocStats {
        units: total,
        chars,
        elapsed_ms: t0.elapsed().as_millis(),
    })
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
