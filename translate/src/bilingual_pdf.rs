//! bilingual_pdf —— 双语对照 PDF 出口（文档翻译第 9 种输出格式）。
//!
//! **纯生成式**：docforge IR 翻译单元 + 译文 → 全新 A4 对照 PDF，任何输入
//! 格式可出（docmt 分派层不要求同格式输入，见规格 docs/translate-spec-
//! bilingual-pdf.md 设计决策 1）。不合并原 PDF 页（v1）：原/译对照都排在
//! 生成页里——按源页分组（[`docforge::writeback::appendix`] 的
//! [`appendix::to_appendix_pages`]，文档序），每源页一节：节标题
//! （"第 {page} 节"）+ 条目 = 原文引用块（"> " 前缀灰字）+ 译文正文（黑字）。
//! 输入恰为 PDF 时即 PageAlt 的 PDF 形态；其他输入是"译文对照集"。
//!
//! 排版：A4、边距 50pt、原文引用 9pt 灰、译文 11pt 黑、节标题 13pt、
//! 行高 1.5 倍；内容溢出自动分页（[`Flow`] 游标兜底）。换行自管——
//! oxidize-pdf 的 TextFlow::write_wrapped 只按空白分词，纯 CJK 长段
//! （无空格）不会断行，这里按估宽（全宽 ≈ 1em、半宽 ≈ 0.55em，与 dual_pdf
//! 同款）先空白断词、整词超行宽再逐字断行，逐视觉行落位。
//!
//! CJK 字体方案**复制自 rumi ocr/src/dual_pdf.rs 的 load_embeddable_font
//! 链**（仅改日志前缀，逻辑逐行照抄并保留原注释）：oxidize-pdf 的 TrueType
//! 解析器不支持 TTC 集合容器，先剥 TTC 头重建独立 sfnt（优先名字含 "SC"
//! 的简体面），再走 Document::add_font_from_bytes 的 Type0/Unicode 路径
//! 整体嵌入；探测失败退化内置 Helvetica（仅 latin 可写，CJK 行计入
//! skipped——与 dual_pdf 同款降级语义）。刻意不加 translate→ocr 依赖，
//! 保持 translate 轻依赖。

use std::path::Path;

use docforge::emit::TranslationUnit;
use docforge::writeback::appendix;
use oxidize_pdf::{Color, Document, Font, Page};

/// A4 纵向页尺寸（pt），与 `Page::a4()` 一致。
const PAGE_H: f64 = 842.0;
/// 四边边距（pt，规格写死 50）。
const MARGIN: f64 = 50.0;
/// 正文可用宽度 = A4 宽 595 − 左右边距。
const CONTENT_W: f64 = 595.0 - 2.0 * MARGIN;
/// 行高倍数（规格写死 1.5）：所有文本行的行进 = 字号 × 1.5。
const LINE_HEIGHT: f64 = 1.5;
/// 原文引用字号（灰）。
const QUOTE_SIZE: f64 = 9.0;
/// 译文正文字号（黑）。
const BODY_SIZE: f64 = 11.0;
/// 源页节标题字号。
const TITLE_SIZE: f64 = 13.0;
/// 原文引用灰色（45% 灰）。
const COLOR_QUOTE: Color = Color::Gray(0.45);
/// 译文 / 节标题黑色。
const COLOR_BODY: Color = Color::Gray(0.0);

/// units + translated → 双语对照 PDF。返回写入的条目数与跳过数
/// （字体覆盖不足时 CJK 行 skipped，与 dual_pdf 降级语义一致）。
///
/// 统计口径：`written` = 至少写出一块（原文引用或译文正文）的条目数；
/// `skipped` = 因字体覆盖不足整块跳过的文本行数（原文行与译文行逐行计，
/// 与 dual_pdf "行不可写即计入 skipped" 同款）；空白行不计数。
/// 译文全未命中（无任何条目）报 Err——docmt 主链路 translated 恒全量，
/// 该分支只可能是直接调用方的构造错误。
pub(crate) fn export_bilingual_pdf(
    units: &[TranslationUnit],
    translated: &[(String, String)],
    out: &Path,
) -> Result<(usize, usize), String> {
    let mut pages = appendix::to_appendix_pages(units, translated);
    // 跨页重复（页眉/页脚类，unit.running）条目去重：翻译侧已合并翻一次，
    // 附录只收首现页——避免页眉在附录里逐页重复灌满（docforge 附录 API
    // 的 AppendixEntry 不带 running 标志，此处按 unit_id 反查 units）
    let running_ids: std::collections::HashSet<&str> = units
        .iter()
        .filter(|u| u.running)
        .map(|u| u.id.as_str())
        .collect();
    if !running_ids.is_empty() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for page in &mut pages {
            page.entries.retain(|e| {
                !running_ids.contains(e.unit_id.as_str())
                    || seen.insert(e.unit_id.clone())
            });
        }
        // 去重后可能出现的空页组剔除（该页全部条目都是重复页眉）
        pages.retain(|p| !p.entries.is_empty());
    }
    if pages.is_empty() {
        return Err("无可写入的双语对照条目（译文全部未命中或为空白）".to_string());
    }
    let mut doc = Document::new();
    doc.set_title("Rumi 双语对照");
    // 字体准备（复制自 dual_pdf）：找到可嵌入字体整体嵌入（Type0，CJK
    // 可写）；找不到退化内置 Helvetica，仅 latin 可写，CJK 行计入 skipped。
    let font = match load_embeddable_font() {
        Some(bytes) => {
            doc.add_font_from_bytes(EMBED_FONT_NAME, bytes)
                .map_err(|e| format!("字体嵌入失败：{e}"))?;
            Some(Font::Custom(EMBED_FONT_NAME.into()))
        }
        None => None,
    };

    let mut flow = Flow::new(font.as_ref());
    let mut written = 0usize;
    let mut skipped = 0usize;
    for (idx, page) in pages.iter().enumerate() {
        // 节标题（13pt 黑）：与上一节留白后独占一行，标题后小间隙接条目
        flow.gap(TITLE_SIZE * LINE_HEIGHT * 0.5);
        let title = section_title(page.source_page.as_deref(), idx + 1);
        flow.write_line(&title, TITLE_SIZE, COLOR_BODY)?;
        flow.gap(TITLE_SIZE * LINE_HEIGHT * 0.25);
        for entry in &page.entries {
            let mut entry_written = false;
            // 原文引用块（9pt 灰，逐源行 "> " 前缀——与 docforge 附录
            // markdown 的引用渲染同构）
            if write_block(&mut flow, &doc, font.as_ref(), &entry.original, true, &mut skipped)? {
                entry_written = true;
            }
            // 译文正文（11pt 黑）
            if write_block(&mut flow, &doc, font.as_ref(), &entry.translation, false, &mut skipped)? {
                entry_written = true;
            }
            if entry_written {
                written += 1;
            }
            // 条目间留白（规格未定，取 0.6 行的紧凑间距）
            flow.gap(QUOTE_SIZE * LINE_HEIGHT * 0.6);
        }
    }
    for page in flow.finish() {
        doc.add_page(page);
    }
    doc.save(out).map_err(|e| format!("PDF 写出失败：{e}"))?;
    Ok((written, skipped))
}

/// 写出一个文本块（条目的原文引用或译文正文）。字体覆盖不足时整块跳过
/// （skipped 按源文本行计）；空白块不写不计数。返回是否有输出。
fn write_block(
    flow: &mut Flow,
    doc: &Document,
    font: Option<&Font>,
    text: &str,
    quote: bool,
    skipped: &mut usize,
) -> Result<bool, String> {
    if text.trim().is_empty() {
        return Ok(false);
    }
    if !text_renderable(doc, font, text) {
        *skipped += text.lines().count();
        return Ok(false);
    }
    let (size, color) = if quote {
        (QUOTE_SIZE, COLOR_QUOTE)
    } else {
        (BODY_SIZE, COLOR_BODY)
    };
    for line in text.lines() {
        let rendered = if quote { quote_line(line) } else { line.to_string() };
        for visual in wrap_text(&rendered, CONTENT_W, size) {
            flow.write_line(&visual, size, color)?;
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// 排版纯函数（单测覆盖）
// ---------------------------------------------------------------------------

/// 节标题文案（规格写死 "第 {page} 节"）：`page` 取原文页名（docforge 的
/// 页序 / epub 部件名 / srt 时间轴……），`None` → 按节序号（1 起）。
fn section_title(page: Option<&str>, ordinal: usize) -> String {
    match page {
        Some(name) => format!("第 {name} 节"),
        None => format!("第 {ordinal} 节"),
    }
}

/// 原文引用行的视觉前缀（与 docforge writeback::appendix 的 markdown
/// 引用前缀同构，PDF 里直接画出 "> "）。
fn quote_line(line: &str) -> String {
    format!("> {line}")
}

/// 单字符估宽：全宽 ≈ 1em、半宽 ≈ 0.55em（WidthGuess，复制自 dual_pdf）。
fn char_estimate(ch: char, font_size: f64) -> f64 {
    if ch.width_guess() {
        font_size
    } else {
        font_size * 0.55
    }
}

/// 文本估宽（换行用）。对嵌入字体的实际度量偏保守：Noto 的半宽字形平均
/// advance < 0.55em，全宽恰为 1em，故按此估宽断行不会溢出右边距。
fn estimate_width(text: &str, font_size: f64) -> f64 {
    text.chars().map(|c| char_estimate(c, font_size)).sum()
}

/// 按估宽硬换行：优先空白断词（词间保留单空格）；整词超一行宽（纯 CJK
/// 长段、长 URL）逐字断行。空输入 → 空表。
fn wrap_text(text: &str, max_width: f64, font_size: f64) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut width = 0.0f64;
    for word in text.split_whitespace() {
        let space = if current.is_empty() {
            0.0
        } else {
            char_estimate(' ', font_size)
        };
        let word_w = estimate_width(word, font_size);
        if width + space + word_w <= max_width {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
            width += space + word_w;
            continue;
        }
        // 词接不下：先收束当前行
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            width = 0.0;
        }
        // 整词仍超一行宽 → 逐字断行（CJK 无空格长段的主路径）
        if word_w > max_width {
            for ch in word.chars() {
                let cw = char_estimate(ch, font_size);
                if width + cw > max_width {
                    lines.push(std::mem::take(&mut current));
                    width = 0.0;
                }
                current.push(ch);
                width += cw;
            }
        } else {
            current.push_str(word);
            width = word_w;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// 该文本在当前字体方案下是否可写（与 dual_pdf 的 line_renderable 同款
/// 语义）：有嵌入字体 → 嵌入字体基本覆盖（个别缺字不影响提取）即写；
/// 无嵌入字体（latin 回退）→ 全部字符需落在 WinAnsi（0x20..=0xFF）内。
fn text_renderable(doc: &Document, font: Option<&Font>, text: &str) -> bool {
    match font {
        Some(Font::Custom(name)) => {
            let missing = doc.font_missing_glyphs(name, text);
            missing.len() < text.chars().count()
        }
        _ => text.chars().all(|c| (0x20..=0xFF).contains(&(c as u32))),
    }
}

// ---------------------------------------------------------------------------
// 逐行排版游标
// ---------------------------------------------------------------------------

/// 逐行排版游标：A4 + 50pt 边距，行进 = 字号 × 1.5；写不下自动翻页
/// （当前页收进 pages，另起新页）。 oxidize-pdf 的 TextFlow 不自动分页
/// 且纯 CJK 不换行，故自管游标（见模块文档）。
struct Flow {
    font: Option<Font>,
    /// 已收档的满页。
    pages: Vec<Page>,
    /// 排版中的当前页。
    page: Page,
    /// 下一行的基线 y（PDF 坐标系，y 向上）。
    cursor_y: f64,
}

impl Flow {
    fn new(font: Option<&Font>) -> Self {
        Self {
            font: font.cloned(),
            pages: Vec::new(),
            page: Page::a4(),
            cursor_y: PAGE_H - MARGIN,
        }
    }

    /// 保证接下来一行（字号 size）放得下：放不下则当前页收档、另起新页。
    fn ensure_room(&mut self, size: f64) {
        if self.cursor_y - size * LINE_HEIGHT < MARGIN {
            let done = std::mem::replace(&mut self.page, Page::a4());
            self.pages.push(done);
            self.cursor_y = PAGE_H - MARGIN;
        }
    }

    /// 在当前行位写出一段文本（基线对齐 cursor_y），随后行进 字号 × 1.5。
    fn write_line(&mut self, text: &str, size: f64, color: Color) -> Result<(), String> {
        self.ensure_room(size);
        self.page
            .text()
            .set_font(self.font.clone().unwrap_or(Font::Helvetica), size)
            .at(MARGIN, self.cursor_y)
            .set_fill_color(color)
            .write(text)
            .map_err(|e| format!("文本写入失败：{e}"))?;
        self.cursor_y -= size * LINE_HEIGHT;
        Ok(())
    }

    /// 垂直空隙（不换页；下一行的 ensure_room 照常兜底）。
    fn gap(&mut self, amount: f64) {
        self.cursor_y -= amount;
    }

    /// 收档：末页入列，返回全部页面（调用方逐页 add_page）。
    fn finish(mut self) -> Vec<Page> {
        self.pages.push(self.page);
        self.pages
    }
}

// ---------------------------------------------------------------------------
// CJK 字体装载（复制自 rumi ocr/src/dual_pdf.rs 的 load_embeddable_font
// 链，仅改日志前缀为 bilingual_pdf；TTC 剥离逻辑与注释逐行照抄）
// ---------------------------------------------------------------------------

/// 系统里探测 CJK 字体的候选路径（按序尝试，取第一个可用的）。
const CJK_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc",
];

/// 嵌入字体在 PDF 资源字典里的名字。
const EMBED_FONT_NAME: &str = "RumiCjk";

/// 粗略字符宽度分类：true ≈ 全宽（CJK 及全角标点），false ≈ 半宽。
trait WidthGuess {
    fn width_guess(&self) -> bool;
}
impl WidthGuess for char {
    fn width_guess(&self) -> bool {
        let u = *self as u32;
        matches!(u,
            0x1100..=0x115F   // Hangul Jamo
            | 0x2E80..=0xA4CF // CJK 部首/标点/音节/汉字
            | 0xAC00..=0xD7A3 // Hangul
            | 0xF900..=0xFAFF // CJK 兼容
            | 0xFE30..=0xFE4F // CJK 兼容形式
            | 0xFF00..=0xFF60 // 全角形式
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD
        )
    }
}

/// 从 TTC 集合中取出一个可用 face 的独立字节：优先名字含 "SC" 的面，否则 index 0。
/// 表数据在原 TTC 中原位不动，仅重写头部为该 face 自己的表目录。
fn load_embeddable_font() -> Option<Vec<u8>> {
    for path in CJK_FONT_CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let face = if bytes.starts_with(b"ttcf") {
            extract_ttc_face(&bytes)
        } else {
            Some(bytes)
        };
        if face.is_some() {
            return face;
        }
        eprintln!("bilingual_pdf：字体 {path} 无法使用，尝试下一候选");
    }
    None
}

fn extract_ttc_face(ttc: &[u8]) -> Option<Vec<u8>> {
    let count = be_u32(ttc, 8)? as usize;
    if count == 0 {
        return None;
    }
    // 先找 SC（简体中文）面；找不到退回第 0 面（Noto CJK 各面覆盖一致，仅字形 regional 变体不同）
    let mut fallback = None;
    for i in 0..count {
        let face = ttc_face_bytes(ttc, i)?;
        if i == 0 {
            fallback = Some(face.clone());
        }
        if face_family_contains(&face, "SC") {
            return Some(face);
        }
    }
    fallback
}

/// 按 face 索引切出独立 sfnt 字节。
///
/// 注意 TTC 每个 subfont 自带头 4 字节 sfnt 签名（'OTTO' / 0x00010000），
/// numTables 在签名之后；且实测 NotoSansCJK TTC 的表数据与后续 face 的目录
/// 交错（首个表可能落在其它 face 目录区间内），不能简单"头部 + 原尾拷贝"，
/// 须逐表搬移并重写记录偏移（4 字节对齐填充）。
fn ttc_face_bytes(ttc: &[u8], index: usize) -> Option<Vec<u8>> {
    if ttc.len() < 12 || &ttc[0..4] != b"ttcf" {
        return None;
    }
    let count = be_u32(ttc, 8)? as usize;
    if index >= count {
        return None;
    }
    let off = be_u32(ttc, 12 + 4 * index)? as usize;
    if off + 6 > ttc.len() {
        return None;
    }
    let num_tables = be_u16(ttc, off + 4)? as usize;
    let header_len = 12 + num_tables * 16;
    if off + header_len > ttc.len() {
        return None;
    }
    let mut records = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let rec = off + 12 + i * 16;
        let tag = ttc.get(rec..rec + 4)?.to_vec();
        let tbl_off = be_u32(ttc, rec + 8)? as usize;
        let tbl_len = be_u32(ttc, rec + 12)? as usize;
        if tbl_off.checked_add(tbl_len)? > ttc.len() {
            return None;
        }
        records.push((tag, tbl_off, tbl_len));
    }
    // 目录（含 sfnt 签名）放最前，随后按原偏移序搬移各表数据并重写偏移
    records.sort_by_key(|(_, o, _)| *o);
    let mut out = ttc[off..off + header_len].to_vec();
    for (i, (_, tbl_off, tbl_len)) in records.iter().enumerate() {
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let new_off = out.len() as u32;
        let rec = 12 + i * 16;
        out[rec + 8..rec + 12].copy_from_slice(&new_off.to_be_bytes());
        out.extend_from_slice(&ttc[*tbl_off..tbl_off + tbl_len]);
    }
    Some(out)
}

/// face 字节（独立 sfnt：4 字节签名 + 表目录）的 name 表族名/全名是否包含 `needle`。
fn face_family_contains(face: &[u8], needle: &str) -> bool {
    // 签名占 4 字节，numTables 在 +4，表记录从 +12 开始
    let num_tables = match be_u16(face, 4) {
        Some(n) => n as usize,
        None => return false,
    };
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        if rec + 16 > face.len() {
            return false;
        }
        if &face[rec..rec + 4] == b"name" {
            let off = match be_u32(face, rec + 8) {
                Some(o) => o as usize,
                None => return false,
            };
            let len = match be_u32(face, rec + 12) {
                Some(l) => l as usize,
                None => return false,
            };
            if off + len > face.len() {
                return false;
            }
            return name_table_contains(&face[off..off + len], needle);
        }
    }
    false
}

/// name 表（format 0）的 nameID 1/4 记录（解码 UTF-16BE 或 Latin-1）是否包含 `needle`。
fn name_table_contains(nt: &[u8], needle: &str) -> bool {
    let (Some(count), Some(storage_rel)) = (be_u16(nt, 2), be_u16(nt, 4)) else {
        return false;
    };
    let storage = 6 + storage_rel as usize;
    for i in 0..count as usize {
        let rec = 6 + i * 12;
        if rec + 12 > nt.len() {
            return false;
        }
        let platform = be_u16(nt, rec).unwrap_or(0);
        let name_id = be_u16(nt, rec + 6).unwrap_or(0);
        if name_id != 1 && name_id != 4 {
            continue; // 只看 family / full name
        }
        let len = be_u16(nt, rec + 10).unwrap_or(0) as usize;
        let off = storage + be_u16(nt, rec + 8).unwrap_or(0) as usize;
        if off + len > nt.len() {
            continue;
        }
        let raw = &nt[off..off + len];
        let decoded: String = if platform == 0 || platform == 3 {
            raw.chunks_exact(2)
                .map(|c| char::from_u32(u16::from_be_bytes([c[0], c[1]]) as u32).unwrap_or('\u{fffd}'))
                .collect()
        } else {
            String::from_utf8_lossy(raw).into_owned()
        };
        if decoded.contains(needle) {
            return true;
        }
    }
    false
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

// ---------------------------------------------------------------------------
// 单元测试（排版纯函数 + 字体链；集成测试见 tests/bilingual_pdf_tests.rs
// ——本模块 API 是 pub(crate)，只能走模块内单测触达）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 节标题：有页名取页名，None 按序号（1 起）。
    #[test]
    fn section_title_uses_page_name_or_ordinal() {
        assert_eq!(section_title(Some("0"), 1), "第 0 节");
        assert_eq!(section_title(Some("ch3.xhtml"), 2), "第 ch3.xhtml 节");
        assert_eq!(section_title(None, 3), "第 3 节");
    }

    /// 引用块前缀：逐行 "> "（与 docforge 附录 markdown 同构）。
    #[test]
    fn quote_line_prefixes_each_line() {
        assert_eq!(quote_line("Hello world."), "> Hello world.");
        assert_eq!(quote_line(""), "> ");
    }

    /// 换行：latin 按空白断词；纯 CJK 长段（无空格）逐字断行且不超行宽。
    #[test]
    fn wrap_text_breaks_words_then_chars() {
        // 11pt 半宽 = 6.05pt：行宽 6 字符 → "aaa bbb ccc" 断成三行
        let lines = wrap_text("aaa bbb ccc", char_estimate('a', 11.0) * 6.0, 11.0);
        assert_eq!(lines, ["aaa", "bbb", "ccc"]);
        // 11pt 全宽 = 11pt：行宽 5 字符 → 7 字的 CJK 串逐字断为 5+2
        let lines = wrap_text("山有木兮木有枝", 5.0 * 11.0, 11.0);
        assert_eq!(lines, ["山有木兮木", "有枝"]);
        // 空输入
        assert!(wrap_text("   ", 100.0, 11.0).is_empty());
        // 整体放得下 → 单行
        assert_eq!(wrap_text("hello world", 1000.0, 11.0), ["hello world"]);
    }

    /// 换行的 latin 行不超行宽（估宽口径）。
    #[test]
    fn wrap_text_respects_max_width() {
        let max_w = CONTENT_W;
        let long = "The quick brown fox jumps over the lazy dog. ".repeat(5);
        for line in wrap_text(&long, max_w, BODY_SIZE) {
            assert!(
                estimate_width(&line, BODY_SIZE) <= max_w,
                "行超宽：{line:?}（{} > {max_w}）",
                estimate_width(&line, BODY_SIZE)
            );
        }
    }

    /// latin 回退（无嵌入字体）的 WinAnsi 可写性判定。
    #[test]
    fn renderable_fallback_requires_winansi() {
        let doc = Document::new();
        assert!(text_renderable(&doc, None, "Hello world 123."));
        // é（U+00E9）落在 WinAnsi 内，可写
        assert!(text_renderable(&doc, None, "café"));
        // CJK 超出 WinAnsi，不可写（Helvetica 降级时计入 skipped 的来源）
        assert!(!text_renderable(&doc, None, "你好世界"));
    }

    /// 字体链：候选字体存在的环境必须剥出独立 sfnt（非 ttcf 头）；
    /// 全缺席的环境（精简 CI）允许 None。
    #[test]
    fn font_candidates_yield_standalone_sfnt_when_present() {
        let any = CJK_FONT_CANDIDATES.iter().any(|p| Path::new(p).is_file());
        match load_embeddable_font() {
            Some(bytes) => {
                assert!(!bytes.starts_with(b"ttcf"), "TTC 剥离失败，仍是集合头");
                assert!(bytes.len() > 64, "face 字节过短：{}", bytes.len());
            }
            None => {
                assert!(!any, "候选字体存在却提取失败");
                eprintln!("跳过：系统无 CJK 字体候选");
            }
        }
    }
}
