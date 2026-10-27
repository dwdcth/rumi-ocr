//! pdf_route —— PDF 智能路由（快路径）。
//!
//! 流程：pdf-inspector 分类（逐页）→ 文字层页直接提取文本+坐标（跳过渲染与 OCR），
//! 扫描/图像页走 pdftoppm 渲染 + 图像识别。
//!
//! 坐标系说明（重要）：pdf-inspector 的 [`pdf_inspector::TextItem`] 报告的是
//! **可见页框（CropBox∩MediaBox）内、左下角为原点、y 向上、单位 PDF 点（1/72 英寸）**的
//! 坐标；`y` 对水平文本是基线（下缘），`height` 是 em 高。其公开 API 不暴露页框高度，
//! 无法在不引入 lopdf 直读的前提下换算成渲染图像的左上原点坐标系，故文字层行的
//! [`OcrLine::r#box`] 保持 **pdf-inspector 原坐标系**：四点顺序仍按我们的约定
//! 左上→右上→右下→左下（这里的"上"= PDF 空间 y 更大的一侧），单位为 PDF 点。
//! 该框自洽（并集/聚合等后处理几何不受 y 方向影响）；与渲染页（真图像坐标）混在
//! 同一响应时，消费者按页区分即可。
//!
//! 探针结论（阶段一，2026-10）：oxidize-pdf 嵌入中文 TrueType 字体 + pdf-inspector
//! 提取，整体相似度 ≥98%，快路径过关，详见 examples/pdf_text_probe.rs。
//! 字体注意：oxidize-pdf 不支持 .ttc 集合（见 [`sample`]），探针用抽取的单面字体。

use crate::{OcrError, OcrLine};
use image::RgbaImage;
use pdf_inspector::extractor::{group_into_lines_preserving_all_text, ItemType};
use pdf_inspector::{DetectionConfig, ScanStrategy};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// 单页路由结果
#[derive(Debug)]
pub enum PdfPage {
    /// 文字层完整，直接提取的行（含 box）
    Text(Vec<OcrLine>),
    /// 需渲染后图像识别（RGBA 图像，K2：与 [`crate`] 的统一图像载体一致）
    Raster(RgbaImage),
}

/// 分类 PDF 并按页路由。
///
/// 分类用 [`ScanStrategy::Full`] 逐页扫描（默认 Sample(8) 对"图封面+正文"型文档
/// 会误判，且逐页路由需要全量 per-page 结论）：`pages_needing_ocr` 即逐页路由依据。
/// 扫描页按**连续区间**合并渲染（一次 pdftoppm -f/-l），页图顺序与页序一致。
pub fn route_pdf(path: &Path) -> Result<Vec<PdfPage>, crate::OcrError> {
    // 与 load_pdf 同款预检：错误文案保持"文件不存在：…"（server http 测试据此区分分支）
    if !path.is_file() {
        return Err(OcrError::Pdf(format!("文件不存在：{}", path.display())));
    }

    let info = pdf_inspector::detect_pdf_type_with_config(
        path,
        DetectionConfig {
            strategy: ScanStrategy::Full,
            ..Default::default()
        },
    )
    .map_err(|e| OcrError::Pdf(format!("PDF 分类失败（{}）：{e}", path.display())))?;

    let page_count = info.page_count;
    let raster_pages: HashSet<u32> = info.pages_needing_ocr.iter().copied().collect();
    let text_pages: HashSet<u32> = (1..=page_count).filter(|p| !raster_pages.contains(p)).collect();

    // 文字层页：提取 + 按行分组，按页归拢
    let mut lines_by_page: HashMap<u32, Vec<OcrLine>> = HashMap::new();
    if !text_pages.is_empty() {
        let items = pdf_inspector::extract_text_with_positions_pages(path, Some(&text_pages))
            .map_err(|e| OcrError::Pdf(format!("文字层提取失败（{}）：{e}", path.display())))?;
        for line in group_into_lines_preserving_all_text(items) {
            if let Some(l) = text_line_to_ocr(&line) {
                lines_by_page.entry(line.page).or_default().push(l);
            }
        }
    }

    // 按页序组装：文字页直接出；连续扫描页并成一个渲染区间
    let mut out = Vec::with_capacity(page_count as usize);
    let mut p = 1u32;
    while p <= page_count {
        if !raster_pages.contains(&p) {
            let lines = lines_by_page.remove(&p).unwrap_or_default();
            out.push(PdfPage::Text(lines));
            p += 1;
            continue;
        }
        let first = p;
        while p <= page_count && raster_pages.contains(&p) {
            p += 1;
        }
        let last = p - 1;
        for img in crate::load_pdf_pages(path, first, last)? {
            out.push(PdfPage::Raster(img));
        }
    }
    Ok(out)
}

/// pdf-inspector 的 [`pdf_inspector::TextLine`] → [`OcrLine`]。
///
/// 文本取行文本；框 = 成员 item 框（x 左缘 / y 底缘 / width / height）的并集，
/// 坐标系见模块注释。只有文本非空的 item 参与并集（图像占位/空链路不污染行框）。
/// 置信度按文字层定义恒为 1.0（文字就在文件里，不存在"识别置信度"）。
fn text_line_to_ocr(line: &pdf_inspector::extractor::TextLine) -> Option<OcrLine> {
    let text = line.text();
    if text.trim().is_empty() {
        return None;
    }
    let mut x0 = f32::MAX;
    let mut x1 = f32::NEG_INFINITY;
    let mut y0 = f32::MAX;
    let mut y1 = f32::NEG_INFINITY;
    for it in &line.items {
        if it.text.trim().is_empty()
            || matches!(it.item_type, ItemType::Image)
        {
            continue;
        }
        x0 = x0.min(it.x);
        x1 = x1.max(it.x + it.width);
        y0 = y0.min(it.y);
        y1 = y1.max(it.y + it.height);
    }
    if x0 > x1 {
        return None;
    }
    Some(OcrLine {
        text,
        score: 1.0,
        // 左上→右上→右下→左下（"上"= PDF 空间 y 大的一侧）
        r#box: [[x0, y1], [x1, y1], [x1, y0], [x0, y0]],
    })
}

// ---------------------------------------------------------------------------
// 探针/测试样例生成（依赖系统 CJK 字体；oxidize-pdf 不支持 .ttc，需抽面）
// ---------------------------------------------------------------------------

/// 探针与路由测试的样例生成辅助。
///
/// 依赖系统字体：优先微软雅黑（TrueType 单面抽取，简体覆盖全），
/// 回退 Noto Sans CJK（CFF）、HYZYB5（繁体为主，简体缺字则被字形检查淘汰）。
pub mod sample {
    use super::*;
    use oxidize_pdf::text::Font;
    use oxidize_pdf::{Document, Page};

    /// 样例页文本：两页，每页 2-3 行中文 + 1 行英文数字混排。
    pub const SAMPLE_PAGES: [&[&str]; 2] = [
        &[
            "深秋的校园格外安静，银杏叶铺满了整条林荫道。",
            "图书馆的阅览室里，学生们埋头写着各自的报告。",
            "Rumi OCR text-layer 2026 page-001 测试行",
        ],
        &[
            "会议室的灯一直亮到深夜，方案改了第七稿。",
            "他推开门，把打印好的文件放在桌上就走了。",
            "PDF 快路径 probe 12345 abcDEF 第二页-002",
        ],
    ];

    /// 候选字体路径（家庭目录运行时拼，无法在编译期写死）。
    fn font_candidates() -> Vec<std::path::PathBuf> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut v = Vec::new();
        if let Some(h) = home {
            v.push(h.join(".local/share/fonts/msyh.ttc")); // 微软雅黑：简体 TrueType
        }
        v.push("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".into()); // CFF，覆盖广
        v.push("/usr/share/fonts/wps-office/HYZYB5.ttf".into()); // 繁体为主，兜底
        v
    }

    /// 生成两页中文文字层样例 PDF。返回实际使用的字体路径。
    pub fn generate_text_pdf(path: &Path) -> Result<String, OcrError> {
        let (mut doc, font_used) = doc_with_cjk_font()?;
        for lines in SAMPLE_PAGES {
            doc.add_page(text_page(lines)?);
        }
        doc.save(path).map_err(|e| OcrError::Pdf(format!("样例保存失败：{e}")))?;
        Ok(font_used)
    }

    /// 生成混合样例：第 1 页文字层，第 2 页纯图像（模拟扫描页，无文字算子）。
    /// pdf-inspector 应判 Mixed，pages_needing_ocr = [2]。
    pub fn generate_mixed_pdf(path: &Path) -> Result<String, OcrError> {
        let (mut doc, font_used) = doc_with_cjk_font()?;
        doc.add_page(text_page(SAMPLE_PAGES[0])?);

        // 合成"扫描页"：白底黑块 JPEG（1200×1600），内存编码不落盘
        let mut img = image::RgbImage::new(1200, 1600);
        for (_, _, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([250, 250, 245]);
        }
        for (x0, y0, w, h) in [(100, 150, 1000, 40), (100, 260, 800, 40), (100, 400, 1000, 300)] {
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    img.put_pixel(x, y, image::Rgb([40, 40, 40]));
                }
            }
        }
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .map_err(|e| OcrError::Image(format!("JPEG 编码失败：{e}")))?;
        let scan = oxidize_pdf::Image::from_jpeg_data(jpeg)
            .map_err(|e| OcrError::Pdf(format!("嵌入图像失败：{e}")))?;
        let mut page = Page::a4();
        page.add_image("scan", scan);
        page.draw_image("scan", 40.0, 40.0, 515.0, 762.0)
            .map_err(|e| OcrError::Pdf(format!("绘制图像失败：{e}")))?;
        doc.add_page(page);

        doc.save(path).map_err(|e| OcrError::Pdf(format!("样例保存失败：{e}")))?;
        Ok(font_used)
    }

    /// 一页文字：A4，行高 30pt，从 y=780 向下排。
    fn text_page(lines: &[&str]) -> Result<Page, OcrError> {
        let mut page = Page::a4();
        let ctx = page.text();
        for (i, line) in lines.iter().enumerate() {
            ctx.set_font(Font::Custom("CJK".into()), 14.0)
                .at(60.0, 780.0 - 30.0 * i as f64)
                .write(line)
                .map_err(|e| OcrError::Pdf(format!("写入文本失败：{e}")))?;
        }
        Ok(page)
    }

    /// 构建注册了"覆盖样例全部字符"的 CJK 字体的 Document。
    /// 逐候选尝试：注册 → 查缺字形 → 缺字/失败换下一个。返回 (doc, 字体路径)。
    fn doc_with_cjk_font() -> Result<(Document, String), OcrError> {
        let all_text: Vec<String> = SAMPLE_PAGES.iter().flat_map(|p| p.iter().map(|s| s.to_string())).collect();
        for cand in font_candidates() {
            if !cand.is_file() {
                continue;
            }
            let data = std::fs::read(&cand).map_err(OcrError::Io)?;
            // .ttc 集合 → 抽第 0 面为单体字体（oxidize-pdf 明确不支持 ttcf）
            let data = if is_ttc(&data) {
                match ttc_face(&data, 0) {
                    Some(face) => face,
                    None => {
                        eprintln!("[字体] {} 抽面失败，跳过", cand.display());
                        continue;
                    }
                }
            } else {
                data
            };
            let mut doc = Document::new();
            if let Err(e) = doc.add_font_from_bytes("CJK", data) {
                eprintln!("[字体] {} 注册失败：{e}", cand.display());
                continue;
            }
            let missing: Vec<char> = all_text
                .iter()
                .flat_map(|t| doc.font_missing_glyphs("CJK", t))
                .collect();
            if !missing.is_empty() {
                eprintln!("[字体] {} 缺字形 {missing:?}，跳过", cand.display());
                continue;
            }
            return Ok((doc, cand.to_string_lossy().into_owned()));
        }
        Err(OcrError::Pdf(
            "没有找到覆盖样例全部字符的 CJK 字体（.ttc 已自动抽面）".into(),
        ))
    }

    fn is_ttc(data: &[u8]) -> bool {
        data.len() >= 4 && &data[0..4] == b"ttcf"
    }

    /// 从 TrueType Collection 抽出第 `face_index` 个 sfnt 单体字体。
    ///
    /// TTC = 'ttcf' 头（版本 + 成员数 + 每个成员 sfnt 的偏移表）；每个成员本身就是
    /// 完整 sfnt。抽面 = 复制该成员的表目录并搬移各表数据（offset 重写、4 字节对齐；
    /// 校验和置 0，PDF 嵌入路径不校验字体内部校验和）。
    fn ttc_face(data: &[u8], face_index: u32) -> Option<Vec<u8>> {
        if data.len() < 12 {
            return None;
        }
        let num = u32::from_be_bytes(data[8..12].try_into().ok()?) as usize;
        let off_pos = 12 + face_index as usize * 4;
        if face_index as usize >= num || off_pos + 4 > data.len() {
            return None;
        }
        let face_off = u32::from_be_bytes(data[off_pos..off_pos + 4].try_into().ok()?) as usize;
        if face_off + 12 > data.len() {
            return None;
        }
        let num_tables = u16::from_be_bytes(data[face_off + 4..face_off + 6].try_into().ok()?) as usize;
        let dir_len = 12 + num_tables * 16;
        if face_off + dir_len > data.len() {
            return None;
        }
        // 收集各表（tag, 数据）
        let mut tables: Vec<([u8; 4], &[u8])> = Vec::with_capacity(num_tables);
        for t in 0..num_tables {
            let rec = face_off + 12 + t * 16;
            let tag = [data[rec], data[rec + 1], data[rec + 2], data[rec + 3]];
            let toff = u32::from_be_bytes(data[rec + 8..rec + 12].try_into().ok()?) as usize;
            let tlen = u32::from_be_bytes(data[rec + 12..rec + 16].try_into().ok()?) as usize;
            let end = toff.checked_add(tlen)?;
            if end > data.len() {
                return None;
            }
            tables.push((tag, &data[toff..end]));
        }
        // 组装单体 sfnt：头 + 表目录（offset 重写）+ 表数据（对齐）
        let dir_len_padded = (dir_len + 3) & !3;
        let mut out = Vec::with_capacity(dir_len_padded + tables.iter().map(|(_, d)| (d.len() + 3) & !3).sum::<usize>());
        out.extend_from_slice(&data[face_off..face_off + 12]); // sfntVersion + numTables + searchRange 等
        let mut offset = dir_len_padded;
        for (tag, d) in &tables {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u32.to_be_bytes()); // checksum 占位
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(d.len() as u32).to_be_bytes());
            offset += (d.len() + 3) & !3;
        }
        out.resize(dir_len_padded, 0);
        for (_, d) in &tables {
            out.extend_from_slice(d);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// 测试：样例走 route_pdf 断言 Text 页文本正确；缺失文件报错；坐标框自洽
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 样例输出统一放 target/（纪律要求），目录随进程隔离避免并行互踩。
    fn probe_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("pdf-route-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn route_pdf_extracts_text_pages_without_engine() {
        let dir = probe_dir("text");
        let pdf = dir.join("sample.pdf");
        let font = sample::generate_text_pdf(&pdf).expect("样例生成失败");

        let pages = route_pdf(&pdf).expect("路由失败");
        assert_eq!(pages.len(), 2, "两页全是文字层，全部走 Text（字体：{font}）");

        for (i, page) in pages.iter().enumerate() {
            let PdfPage::Text(lines) = page else {
                panic!("第 {} 页应为文字层，却路由到渲染", i + 1);
            };
            assert!(!lines.is_empty(), "第 {} 页提取结果为空", i + 1);
            // 本页期望行逐一断言：按字符相似度 ≥98% 找到对应提取行
            for exp in sample::SAMPLE_PAGES[i] {
                let best = lines
                    .iter()
                    .map(|l| similarity(exp, &l.text))
                    .fold(0.0f64, f64::max);
                assert!(
                    best >= 0.98,
                    "第 {} 页期望行 {exp:?} 最相似度 {best:.3} < 0.98，实际行集：{:?}",
                    i + 1,
                    lines.iter().map(|l| &l.text).collect::<Vec<_>>()
                );
            }
            // 框自洽：4 点、score=1.0、x/y 有序且坐标为正（PDF 点空间）
            for l in lines {
                assert!((l.score - 1.0).abs() < 1e-6);
                assert!(l.text.contains(|c: char| !c.is_ascii()), "中文行应含非 ASCII：{:?}", l.text);
                let [tl, tr, br, bl] = l.r#box;
                assert!(tl[0] <= tr[0] && bl[0] <= br[0] && tl[1] >= br[1] && tr[1] >= br[1]);
                assert!(tl[0] > 0.0 && tl[1] > 0.0, "坐标应为正：{:?}", l.r#box);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn route_pdf_mixed_routes_scan_page_to_raster() {
        let dir = probe_dir("mixed");
        let pdf = dir.join("mixed.pdf");
        sample::generate_mixed_pdf(&pdf).expect("混合样例生成失败");

        let pages = route_pdf(&pdf).expect("路由失败");
        assert_eq!(pages.len(), 2);
        let PdfPage::Text(lines) = &pages[0] else {
            panic!("第 1 页是文字层，应走 Text");
        };
        assert!(
            lines.iter().any(|l| l.text.contains("深秋的校园")),
            "第 1 页应提取到中文行：{:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        let PdfPage::Raster(img) = &pages[1] else {
            panic!("第 2 页是纯图像，应走 Raster");
        };
        // A4 @200dpi ≈ 1654×2339
        let (w, h) = img.dimensions();
        assert!(
            (1500..1800).contains(&w) && (2200..2500).contains(&h),
            "第 2 页渲染尺寸异常：{}×{}",
            w,
            h
        );
        assert_eq!(img.len(), w as usize * h as usize * 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn route_pdf_missing_file_reports_missing() {
        let err = route_pdf(Path::new("/nonexistent/rumi/definitely-missing.pdf")).unwrap_err();
        assert!(err.to_string().contains("文件不存在"), "实际错误：{err}");
    }

    /// --force-raster 等价旧行为的根基：load_pdf_pages 走同一条 pdftoppm 路径、
    /// 同一 DPI，只渲染请求的闭区间页。
    #[test]
    fn load_pdf_pages_renders_only_requested_range() {
        let dir = probe_dir("range");
        let pdf = dir.join("sample.pdf");
        sample::generate_text_pdf(&pdf).expect("样例生成失败");

        let page2 = crate::load_pdf_pages(&pdf, 2, 2).expect("范围渲染失败");
        assert_eq!(page2.len(), 1, "只渲染第 2 页");
        let all = crate::load_pdf(&pdf).expect("全量渲染失败");
        assert_eq!(all.len(), 2);
        // 范围帧与全量渲染对应页同尺寸（同一 pdftoppm -f/-l 路径）
        assert_eq!(page2[0].dimensions(), all[1].dimensions());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 归一化编辑距离相似度（与 examples/pdf_text_probe.rs 同一算法）。
    fn similarity(a: &str, b: &str) -> f64 {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        if a.is_empty() && b.is_empty() {
            return 1.0;
        }
        let max = a.len().max(b.len());
        if max == 0 {
            return 1.0;
        }
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        let mut cur = vec![0usize; b.len() + 1];
        for i in 1..=a.len() {
            cur[0] = i;
            for j in 1..=b.len() {
                let cost = usize::from(a[i - 1] != b[j - 1]);
                cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            }
            std::mem::swap(&mut prev, &mut cur);
        }
        1.0 - (prev[b.len()] as f64) / (max as f64)
    }
}
