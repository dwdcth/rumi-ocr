//! dual_pdf —— 双层可搜索 PDF 导出（图像视觉层 + 隐形文字层）。
//!
//! 基于 oxidize-pdf 的 Type0 / Identity-H / ToUnicode CMap 路线：
//! - **视觉层**：整页 PNG 作 Image XObject 铺满页面（页尺寸 = 像素 × 72/DPI pt）；
//! - **文字层**：每条 [`OcrLine`] 按四点框包围盒定位，写入隐形文本
//!   （`TextRenderingMode::Invisible`，即 PDF `3 Tr`），字号 ≈ 盒高、基线放盒底；
//!   行估宽超出盒宽时按比例水平压缩（Tz），保证搜索高亮落回原位置附近；
//! - **CJK 字体**：oxidize-pdf 的 TrueType 解析器**不支持 TTC 集合容器**
//!   （其 truetype.rs 明确报 "TrueType Collection (TTC) files are not supported"），
//!   本模块自带最小 TTC 头剥离：按 face 索引重建独立 sfnt 头（表数据原位不动），
//!   优先选名字含 "SC" 的简体中文字面，再走 `Document::add_font_from_bytes`
//!   的 Type0/Unicode 路径整体嵌入（写字时 UTF-16BE 十六进制串 + ToUnicode CMap，
//!   pdftotext 等工具可正常提取）。

use crate::{OcrError, OcrLine};
use oxidize_pdf::text::TextRenderingMode;
use oxidize_pdf::{Document, Font, Image, Page};
use std::path::Path;

/// 默认渲染 DPI（与 [`crate::load_pdf`] 的约定一致：像素 → pt 按 72/DPI 换算）。
pub const DEFAULT_DPI: f64 = 200.0;

/// 系统里探测 CJK 字体的候选路径（按序尝试，取第一个可用的）。
const CJK_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc",
];

/// 嵌入字体在 PDF 资源字典里的名字。
const EMBED_FONT_NAME: &str = "RumiCjk";

/// 导出统计：成功写入隐形文字层的行数 / 因字体覆盖或退化框跳过的行数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DualLayerStats {
    /// 成功写入隐形文字层的行数
    pub written: usize,
    /// 无法写入的行数（当前字体完全不覆盖该行文字，或包围盒退化）
    pub skipped: usize,
}

/// 导出双层可搜索 PDF（默认 [`DEFAULT_DPI`]）。
///
/// `pages`：每项为 (页 PNG 字节, 该页识别行列表)，按输入顺序成页。
/// 返回写入/跳过统计；失败返回 [`OcrError`]。
pub fn export_dual_layer_pdf(
    out_path: &Path,
    pages: &[(Vec<u8>, Vec<OcrLine>)],
) -> Result<DualLayerStats, OcrError> {
    export_dual_layer_pdf_dpi(out_path, pages, DEFAULT_DPI)
}

/// [`export_dual_layer_pdf`] 的显式 DPI 版本（必须与页图像的渲染 DPI 一致）。
pub fn export_dual_layer_pdf_dpi(
    out_path: &Path,
    pages: &[(Vec<u8>, Vec<OcrLine>)],
    dpi: f64,
) -> Result<DualLayerStats, OcrError> {
    if pages.is_empty() {
        return Err(OcrError::Pdf("export_dual_layer_pdf：pages 为空".into()));
    }
    if !dpi.is_finite() || dpi <= 0.0 {
        return Err(OcrError::Pdf(format!("非法 DPI：{dpi}")));
    }

    let mut doc = Document::new();
    doc.set_title("Rumi OCR");

    // 字体准备：找到可嵌入字体则整体嵌入（Type0，支持 CJK）；
    // 找不到则退化为内置 Helvetica，只写 latin 行，CJK 行计入 skipped。
    let font = match load_embeddable_font() {
        Some(bytes) => {
            doc.add_font_from_bytes(EMBED_FONT_NAME, bytes)
                .map_err(|e| OcrError::Pdf(format!("字体嵌入失败：{e}")))?;
            Some(Font::Custom(EMBED_FONT_NAME.into()))
        }
        None => None,
    };

    let mut stats = DualLayerStats::default();
    for (png, lines) in pages {
        let page = render_page(&doc, png, lines, dpi, font.as_ref(), &mut stats)?;
        doc.add_page(page);
    }
    doc.save(out_path).map_err(|e| OcrError::Pdf(format!("PDF 写出失败：{e}")))?;
    Ok(stats)
}

/// 组装一页：图像 XObject 铺满 + 隐形文字层。
fn render_page(
    doc: &Document,
    png: &[u8],
    lines: &[OcrLine],
    dpi: f64,
    font: Option<&Font>,
    stats: &mut DualLayerStats,
) -> Result<Page, OcrError> {
    let img = image::load_from_memory(png)?;
    let (w_px, h_px) = (img.width() as f64, img.height() as f64);
    let scale = 72.0 / dpi;
    let (w_pt, h_pt) = (w_px * scale, h_px * scale);

    let mut page = Page::new(w_pt, h_pt);
    let oxidize_img = Image::from_png_data(png.to_vec())
        .map_err(|e| OcrError::Pdf(format!("PNG 装载失败：{e}")))?;
    page.add_image("Im0", oxidize_img);
    page.draw_image("Im0", 0.0, 0.0, w_pt, h_pt)
        .map_err(|e| OcrError::Pdf(format!("图像绘制失败：{e}")))?;

    for line in lines {
        let text = line.text.trim();
        if text.is_empty() {
            continue; // 空行不算失败，也不计数
        }
        let Some(((x0, y0), (x1, y1))) = bbox_px(line) else {
            stats.skipped += 1; // 包围盒退化（无面积）
            continue;
        };
        if !line_renderable(doc, font, text) {
            stats.skipped += 1;
            continue;
        }

        let (bw_pt, bh_pt) = ((x1 - x0) * scale, (y1 - y0) * scale);
        if bh_pt < 1.0 || bw_pt < 1.0 {
            stats.skipped += 1;
            continue;
        }
        // 字号 ≈ 盒高（略缩避免上下溢出盒）；基线放在盒底上方 ~1/5 盒高处
        let font_size = (bh_pt * 0.9).max(1.0);
        let baseline_y = h_pt - y1 * scale + bh_pt * 0.2;
        // 估宽（CJK/全角 ≈ 1em，其余 ≈ 0.55em），超出盒宽则水平压缩
        let est_w = text
            .chars()
            .map(|c| if c.width_guess() { font_size } else { font_size * 0.55 })
            .sum::<f64>();
        let hscale = if est_w > bw_pt { (bw_pt / est_w).clamp(0.05, 1.0) } else { 1.0 };

        page.text()
            .set_font(font.unwrap_or(&Font::Helvetica).clone(), font_size)
            .at(x0 * scale, baseline_y)
            .set_horizontal_scaling(hscale)
            .set_rendering_mode(TextRenderingMode::Invisible)
            .write(text)
            .map_err(|e| OcrError::Pdf(format!("隐形文字写入失败：{e}")))?;
        stats.written += 1;
    }
    Ok(page)
}

/// 该行文字在当前字体方案下是否可写入：
/// 有嵌入字体 → 嵌入字体基本覆盖（个别缺字不影响提取）即写；
/// 无嵌入字体（latin 回退）→ 全部字符需落在 WinAnsi（0x20..=0xFF）内。
fn line_renderable(doc: &Document, font: Option<&Font>, text: &str) -> bool {
    match font {
        Some(Font::Custom(name)) => {
            let missing = doc.font_missing_glyphs(name, text);
            missing.len() < text.chars().count()
        }
        _ => text.chars().all(|c| (0x20..=0xFF).contains(&(c as u32))),
    }
}

/// OcrLine 四点框的轴对齐包围盒（图像像素坐标，y 向下）；退化（无面积）返回 None。
fn bbox_px(line: &OcrLine) -> Option<((f64, f64), (f64, f64))> {
    let mut xs = [0f64; 4];
    let mut ys = [0f64; 4];
    for (i, p) in line.r#box.iter().enumerate() {
        xs[i] = p[0] as f64;
        ys[i] = p[1] as f64;
    }
    let (x0, x1) = (xs.iter().cloned().fold(f64::INFINITY, f64::min), xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    let (y0, y1) = (ys.iter().cloned().fold(f64::INFINITY, f64::min), ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    (x1 > x0 && y1 > y0).then_some(((x0, y0), (x1, y1)))
}

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

// ---------------------------------------------------------------------------
// TTC 剥离：oxidize-pdf 不吃 TTC 容器，这里把目标 face 重建为独立 sfnt。
// ---------------------------------------------------------------------------

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
        eprintln!("dual_pdf：字体 {path} 无法使用，尝试下一候选");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OcrEngine;
    use ab_glyph::FontArc;
    use image::RgbaImage;
    use imageproc::drawing::draw_text_mut;
    use std::path::PathBuf;

    // ----- 测试辅助：合成白底黑字 PNG（与 lib.rs 测试同一套路） -----

    fn find_font() -> Option<PathBuf> {
        let roots = ["/usr/share/fonts", "/usr/local/share/fonts"];
        let prefer = ["NotoSansCJK-Regular", "NotoSansCJK-Bold", "DejaVuSans.ttf"];
        for root in roots {
            let mut all = Vec::new();
            collect_fonts(Path::new(root), &mut all);
            for key in prefer {
                if let Some(p) = all.iter().find(|p| p.to_string_lossy().contains(key)) {
                    return Some(p.clone());
                }
            }
            if let Some(p) = all
                .iter()
                .find(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("ttf" | "ttc" | "otf")))
            {
                return Some(p.clone());
            }
        }
        None
    }

    fn collect_fonts(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_fonts(&p, out);
            } else {
                out.push(p);
            }
        }
    }

    fn ab_glyph_font(bytes: Vec<u8>) -> FontArc {
        if bytes.starts_with(b"ttcf") {
            FontArc::from(ab_glyph::FontVec::try_from_vec_and_index(bytes, 0).expect("ttc 加载失败"))
        } else {
            FontArc::try_from_vec(bytes).expect("字体加载失败")
        }
    }

    /// 白底黑字 PNG（1200 宽，每行 110px，72px 字号）。
    fn text_png(lines: &[&str]) -> Vec<u8> {
        let font_path = find_font().expect("系统缺少可用字体");
        let font = ab_glyph_font(std::fs::read(&font_path).expect("读取字体失败"));
        let (w, line_h) = (1200u32, 110u32);
        let h = line_h * lines.len() as u32 + 40;
        let mut img = RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
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
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .expect("PNG 编码失败");
        buf
    }

    /// 用真实 OCR 引擎识别 PNG 字节（首跑含模型下载/缓存）。
    fn recognize_png(png: &[u8]) -> Vec<OcrLine> {
        let img = crate::load_frame_from_bytes(png).expect("PNG 装载失败");
        OcrEngine::global().recognize(&img).expect("识别失败")
    }

    fn run_pdftotext(pdf: &Path, first_page: Option<u32>, last_page: Option<u32>) -> String {
        let mut cmd = std::process::Command::new("pdftotext");
        if let (Some(f), Some(l)) = (first_page, last_page) {
            cmd.args(["-f", &f.to_string(), "-l", &l.to_string()]);
        }
        let out = cmd
            .arg(pdf)
            .arg("-")
            .output()
            .expect("pdftotext 运行失败（需安装 poppler-utils）");
        assert!(out.status.success(), "pdftotext 失败：{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// 去掉全部空白后是否包含（pdftotext 可能改变词间距/插入空白）。
    fn nospace(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rumi-dual-pdf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录失败");
        d
    }

    // ----- 单元测试 -----

    #[test]
    fn ttc_face_extraction_recovers_noto_sc() {
        // 候选字体都缺席的环境（如精简 CI）才允许跳过；本机有 Noto 时必须成
        let any_candidate = CJK_FONT_CANDIDATES.iter().any(|p| Path::new(p).is_file());
        let Some(bytes) = load_embeddable_font() else {
            assert!(!any_candidate, "候选字体存在却提取失败");
            eprintln!("跳过：系统无 CJK 字体候选");
            return;
        };
        // 独立 sfnt 头：不再是 ttcf，且 oxidize 解析器能吃
        assert!(!bytes.starts_with(b"ttcf"), "TTC 剥离失败，仍是集合头");
        let ttf = oxidize_pdf::text::fonts::truetype::TrueTypeFont::parse(bytes.clone())
            .expect("剥离后的 face 应能被 oxidize-pdf 解析");
        let name = ttf.get_font_name().unwrap_or_default();
        eprintln!("嵌入字体：{name}（{} 字节）", bytes.len());
        assert!(name.contains("Noto"), "取到的不是 Noto face：{name:?}");
    }

    #[test]
    fn ttc_face_bytes_rejects_truncated_input() {
        assert!(extract_ttc_face(b"").is_none());
        assert!(extract_ttc_face(b"not ttc").is_none());
        // 头部声明 2 个 face，但偏移指向越界位置（subfont 签名都读不到）
        let mut truncated = b"ttcf".to_vec();
        truncated.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 200]);
        assert!(extract_ttc_face(&truncated).is_none());
        // 偏移可读但没有 numTables / 表目录字节
        let mut short_face = b"ttcf".to_vec();
        short_face.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 12]);
        short_face.extend_from_slice(b"OTTO"); // 签名在，numTables 缺
        assert!(extract_ttc_face(&short_face).is_none());
    }

    #[test]
    fn export_rejects_empty_pages_and_bad_dpi() {
        let dir = tmp_dir("guard");
        let out = dir.join("out.pdf");
        assert!(matches!(
            export_dual_layer_pdf(&out, &[]),
            Err(OcrError::Pdf(_))
        ));
        let png = text_png(&["x"]);
        let pages = vec![(png, Vec::new())];
        assert!(matches!(
            export_dual_layer_pdf_dpi(&out, &pages, 0.0),
            Err(OcrError::Pdf(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----- 端到端：合成图 → OCR → 双层 PDF → pdftotext 提取回原文 -----

    #[test]
    fn export_dual_layer_pdf_text_is_extractable() {
        let png = text_png(&["Rumi OCR 123", "你好世界 searchable", "Dual Layer PDF"]);
        let lines = recognize_png(&png);
        assert!(!lines.is_empty(), "OCR 识别结果为空");
        let written_texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
        eprintln!("OCR {} 行：{written_texts:?}", lines.len());

        let dir = tmp_dir("e2e");
        let out = dir.join("dual.pdf");
        let stats = export_dual_layer_pdf(&out, &[(png, lines)]).expect("导出失败");
        eprintln!("stats = {stats:?}");
        assert!(stats.written >= 1, "没有任何行写入文字层");
        assert!(out.is_file() && out.metadata().unwrap().len() > 0);

        // 关键验证：写入什么就该提取回什么（去空白比对），这才是真·可搜索
        let extracted = nospace(&run_pdftotext(&out, None, None));
        assert!(!extracted.is_empty(), "pdftotext 提取结果为空");
        let hits = written_texts
            .iter()
            .filter(|t| t.chars().count() >= 2 && extracted.contains(&nospace(t)))
            .count();
        eprintln!("提取命中 {hits}/{} 行：{extracted}", written_texts.len());
        assert!(hits >= 1, "提取文本与写入行不匹配：{extracted}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exported_pdf_renders_back_to_visible_image() {
        let png = text_png(&["Visual Layer 42"]);
        let lines = recognize_png(&png);
        let dir = tmp_dir("render");
        let out = dir.join("dual.pdf");
        export_dual_layer_pdf(&out, &[(png, lines)]).expect("导出失败");

        // 72dpi 渲染回位图：1pt = 1px，尺寸应与页 pt 尺寸一致
        let page_png = dir.join("page");
        let rendered = std::process::Command::new("pdftoppm")
            .args(["-png", "-r", "72", "-singlefile"])
            .arg(&out)
            .arg(&page_png)
            .output()
            .expect("pdftoppm 运行失败（需安装 poppler-utils）");
        assert!(rendered.status.success(), "pdftoppm 失败");
        let img = image::open(page_png.with_extension("png")).expect("渲染 PNG 解码失败");
        let (w, h) = (img.width(), img.height());
        let src = image::load_from_memory(&text_png(&["Visual Layer 42"])).unwrap();
        let (ew, eh) = (src.width(), src.height());
        let scale = 72.0 / DEFAULT_DPI;
        assert!(
            (w as f64 - ew as f64 * scale).abs() <= 2.0 && (h as f64 - eh as f64 * scale).abs() <= 2.0,
            "渲染尺寸 {w}x{h} 与页尺寸 {}x{} 不符",
            ew as f64 * scale,
            eh as f64 * scale
        );
        // 非全白：视觉层（图像 XObject）确实在
        let luma = img.to_luma8();
        let non_white = luma.pixels().filter(|p| p.0[0] < 200).count();
        assert!(non_white > 0, "渲染结果全白，视觉层缺失");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multi_page_export_keeps_page_order() {
        let page1 = text_png(&["Alpha First 11"]);
        let page2 = text_png(&["Beta Second 22"]);
        let l1 = recognize_png(&page1);
        let l2 = recognize_png(&page2);
        assert!(!l1.is_empty() && !l2.is_empty());

        let dir = tmp_dir("multipage");
        let out = dir.join("two.pdf");
        let empty = count_empty(&[&l1, &l2]);
        let (n1, n2) = (l1.len(), l2.len());
        let stats = export_dual_layer_pdf(&out, &[(page1, l1), (page2, l2)]).expect("导出失败");
        assert_eq!(stats.written, n1 + n2 - empty);

        let p1 = nospace(&run_pdftotext(&out, Some(1), Some(1)));
        let p2 = nospace(&run_pdftotext(&out, Some(2), Some(2)));
        eprintln!("page1: {p1} | page2: {p2}");
        assert!(p1.contains("Alpha") || p1.contains("11"), "第 1 页未含第 1 页文字：{p1}");
        assert!(p2.contains("Beta") || p2.contains("22"), "第 2 页未含第 2 页文字：{p2}");
        assert!(!p1.contains("Beta"), "第 1 页串入了第 2 页内容");
        assert!(!p2.contains("Alpha"), "第 2 页串入了第 1 页内容");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn count_empty(pages: &[&[OcrLine]]) -> usize {
        pages
            .iter()
            .flat_map(|p| p.iter())
            .filter(|l| l.text.trim().is_empty())
            .count()
    }
}
