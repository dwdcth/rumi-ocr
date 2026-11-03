//! pdf_route —— PDF 智能路由（docforge 端到端）。
//!
//! 文字层页直提行（pt、左下原点、y 向上，score=1.0）；扫描页 docforge 内部
//! pdftoppm 渲染 + 注入的 rumi 引擎识别（像素、左上原点，带 confidence/quad）。
//! 坐标系按页区分（与旧实现契约一致，消费者按 source/坐标系分派）。
//!
//! 选型（2026-11，替换自 pdf-inspector 直调）：docforge 的 pdf 后端本身就构建于
//! pdf-inspector 之上（行分组同一函数 `group_into_lines_preserving_all_text`，
//! 文字层行与旧实现逐字节一致），且把"分类→渲染→OCR"整条管线收进一个
//! 引擎注入式的 `parse` 调用——rumi 侧只剩适配器（[`RumiOcrAdapter`]）与
//! 块→行的坐标重排。git 依赖为分发渠道，跟 main 滚动（`cargo update -p docforge`）。

use crate::{OcrError, OcrLine};
use docforge::backends::pdf::{OcrMode, PdfOptions};
use docforge::ocr::OcrEngine as DocforgeOcrEngine;
use docforge::ocr::RecognizedLine;
use docforge::{BlockSource, DocumentEngine, Registry};
use std::path::Path;
use std::sync::Arc;

/// 路由选项。
///
/// engine 取 `&'static`：docforge 的 `PdfOptions.engine` 是 `Arc<dyn OcrEngine>`
///（对象默认 'static），借用式适配器活不过 parse 调用栈。rumi 的引擎本就是
/// 进程级单例（[`crate::OcrEngine::global`] 等都返回 `&'static`），无损。
pub struct RouteOptions {
    /// rumi OCR 引擎（注入 docforge，扫描页识别用；避免模型双重加载）
    pub engine: &'static crate::OcrEngine,
    /// 页级进度回调透传：(已处理页数, 总页数) -> 是否继续（false 中止，
    /// 已产出页保留，余页空占位——docforge Ok 语义）
    pub on_progress: Option<Arc<dyn Fn(u32, u32) -> bool + Send + Sync>>,
    /// 扫描页渲染 DPI（默认 200，对齐 docforge DEFAULT_DPI）
    pub dpi: u32,
}

impl RouteOptions {
    /// 手写构造函数（engine 无默认值，`Default` 不适用）：on_progress None、
    /// dpi 200。需要进度/自定义 DPI 时结构体字面量补齐其余字段。
    pub fn new(engine: &'static crate::OcrEngine) -> Self {
        Self {
            engine,
            on_progress: None,
            dpi: docforge::backends::pdf::DEFAULT_DPI,
        }
    }
}

/// 单页路由结果。Raster 变体已删除：渲染+OCR 在 docforge 内完成，
/// OCR 页的行同样出现在 Text 里（source 区分见 docforge `BlockSource`；
/// 文字层行 score=1.0、坐标 pt 左下原点，OCR 行带 confidence、坐标渲染像素）。
#[derive(Debug)]
pub enum PdfPage {
    /// 本页全部行（文字层直提与扫描页 OCR 识别统一在此变体）
    Text(Vec<OcrLine>),
}

/// rumi 引擎 → docforge OcrEngine 适配器（零拷贝：image 0.25 同实例，
/// `&RgbaImage` 直通不复制）。engine 为 `&'static`（见 [`RouteOptions`] 注），
/// 才能满足 `PdfOptions.engine = Arc<dyn OcrEngine>` 的 'static 对象约束。
struct RumiOcrAdapter {
    engine: &'static crate::OcrEngine,
}

impl DocforgeOcrEngine for RumiOcrAdapter {
    fn name(&self) -> &str {
        "rumi-oar"
    }

    fn recognize(&self, image: &image::RgbaImage) -> Result<Vec<RecognizedLine>, String> {
        // engine.recognize 的 OcrLine{text, box, score} → RecognizedLine
        // {text, score, quad}（box 四点直通，PaddleOCR det polys 天然形态）
        self.engine
            .recognize(image)
            .map(|lines| lines.into_iter().map(to_recognized_line).collect())
            .map_err(|e| e.to_string())
    }
}

/// [`OcrLine`] → docforge [`RecognizedLine`] 字段映射（适配器单测的纯转换层）。
fn to_recognized_line(l: OcrLine) -> RecognizedLine {
    RecognizedLine {
        text: l.text,
        score: l.score,
        quad: l.r#box,
    }
}

/// 分类 PDF 并按页路由（docforge 端到端）。
///
/// 装配：`Registry` + pdf 后端（`OcrMode::Auto` 逐页判定：文字层页直提、
/// 扫描页按连续区间 pdftoppm 渲染后走注入的 rumi 引擎）+ `DocumentEngine::parse`。
/// 扫描页识别在 docforge 内逐页完成（旧实现是收集帧后批量 recognize_many，
/// 行文本/行序一致，见模块注释）。
pub fn route_pdf(path: &Path, opts: &RouteOptions) -> Result<Vec<PdfPage>, OcrError> {
    // 与 load_pdf 同款预检：错误文案保持"文件不存在：…"（server http 测试据此
    // 区分分支；docforge 预检同文案，但 rumi 侧先查可省一次装配）
    if !path.is_file() {
        return Err(OcrError::Pdf(format!("文件不存在：{}", path.display())));
    }

    let mut registry = Registry::new();
    docforge::backends::pdf::register(
        &mut registry,
        PdfOptions {
            mode: OcrMode::Auto,
            dpi: opts.dpi,
            engine: Some(Arc::new(RumiOcrAdapter { engine: opts.engine })),
            intra_page: false,
            on_progress: opts.on_progress.clone(),
            keep_raster_pages: false,
        },
    );
    let doc = DocumentEngine::new(registry)
        // DocumentEngine::parse 收 impl AsRef<Path>（docforge@976d0808 起，
        // 顺手支持非 UTF-8 路径）——直接传 Path，不再经 to_string_lossy
        .parse(path)
        .map_err(|e| OcrError::Pdf(e.message))?;
    // docforge 的 warnings 挂在 meta.extra（末条优先），rumi 现状无 warnings
    // 概念：一行 eprintln 透传即可（正常解析无告警，静默）
    if let Some(w) = doc.meta.extra.get("warnings") {
        eprintln!("[pdf_route] docforge：{w}");
    }

    let mut out = Vec::with_capacity(doc.pages.len());
    for page in &doc.pages {
        let mut lines = Vec::new();
        for b in &page.blocks {
            // 空文本块跳过（等价旧 text_line_to_ocr 的空文本过滤）
            if b.content.trim().is_empty() {
                continue;
            }
            let (score, r#box) = match b.source {
                // OCR 行：score=confidence；quad 直通（旋转保真）；
                // quad None（理论不发生，防御）退 bbox 左上原点四点；
                // 两者皆缺给全零框（不产出比产出错框更稳）
                BlockSource::Ocr => (
                    b.confidence.unwrap_or(0.0),
                    match (b.quad, b.bbox) {
                        (Some(q), _) => q,
                        (None, Some(r)) => [[r.x0, r.y0], [r.x1, r.y0], [r.x1, r.y1], [r.x0, r.y1]],
                        (None, None) => [[0.0; 2]; 4],
                    },
                ),
                // 文字层（Native 等来源 pdf 不产生，防御性同 TextLayer）：
                // score=1.0（文字就在文件里，不存在"识别置信度"）；box 从
                // bbox(pt, y 向上) 构造，顺序沿旧契约"左上→右上→右下→左下"
                //（"上" = PDF y 更大侧）
                _ => (
                    1.0,
                    match b.bbox {
                        Some(r) => [[r.x0, r.y1], [r.x1, r.y1], [r.x1, r.y0], [r.x0, r.y0]],
                        None => [[0.0; 2]; 4],
                    },
                ),
            };
            lines.push(OcrLine {
                text: b.content.clone(),
                score,
                r#box,
            });
        }
        out.push(PdfPage::Text(lines));
    }
    Ok(out)
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
    /// docforge 分类应判 Mixed，第 2 页走渲染 + 注入引擎识别。
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

        // global() 懒构建：纯文字页不触发识别，无模型下载
        let opts = RouteOptions::new(crate::OcrEngine::global());
        let pages = route_pdf(&pdf, &opts).expect("路由失败");
        assert_eq!(pages.len(), 2, "两页全是文字层，全部走 Text（字体：{font}）");

        for (i, page) in pages.iter().enumerate() {
            let PdfPage::Text(lines) = page;
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

    /// 混合样例端到端：文字页直提，扫描页 docforge 内渲染 + 注入引擎识别
    /// （真推理：命中 ~/.oar 模型缓存，与 lib 既有真推理测试同前提）。
    #[test]
    fn route_pdf_mixed_routes_scan_page_lines_through_engine() {
        let dir = probe_dir("mixed");
        let pdf = dir.join("mixed.pdf");
        sample::generate_mixed_pdf(&pdf).expect("混合样例生成失败");

        let opts = RouteOptions::new(crate::OcrEngine::global());
        let pages = route_pdf(&pdf, &opts).expect("路由失败");
        assert_eq!(pages.len(), 2);
        let PdfPage::Text(text_lines) = &pages[0];
        assert!(
            text_lines.iter().any(|l| l.text.contains("深秋的校园")),
            "第 1 页应提取到中文行：{:?}",
            text_lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        // 文字层行 score 恒 1.0
        assert!(text_lines.iter().all(|l| (l.score - 1.0).abs() < 1e-6));

        // 第 2 页扫描页：行同样以 Text 变体出现（渲染+OCR 在 docforge 内完成），
        // 识别结果带真实置信度（合成黑块可能识别不出任何行，允许为空）
        let PdfPage::Text(scan_lines) = &pages[1];
        for l in scan_lines {
            assert!(l.score > 0.0 && l.score < 1.0, "OCR 行应为真实置信度：{l:?}");
            // OCR 页坐标 = 渲染像素（A4@200dpi ≈ 1654×2339）左上原点，非负
            for pt in l.r#box {
                assert!(pt[0] >= 0.0 && pt[1] >= 0.0, "OCR 坐标应为非负像素：{:?}", l.r#box);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn route_pdf_missing_file_reports_missing() {
        let opts = RouteOptions::new(crate::OcrEngine::global());
        let err = route_pdf(Path::new("/nonexistent/rumi/definitely-missing.pdf"), &opts).unwrap_err();
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

    /// RumiOcrAdapter 转发正确性（mock，不跑真模型）：OcrLine 字段
    /// {text, score, box} → RecognizedLine {text, score, quad} 直通，
    /// 引擎名固定 "rumi-oar"（docforge 诊断/日志用）。
    #[test]
    fn adapter_maps_line_fields_without_model() {
        let adapter = RumiOcrAdapter {
            engine: crate::OcrEngine::global(),
        };
        assert_eq!(DocforgeOcrEngine::name(&adapter), "rumi-oar");

        let mapped = to_recognized_line(OcrLine {
            text: "斜行 ABC".into(),
            score: 0.87,
            r#box: [[1.0, 2.0], [3.0, 2.5], [2.5, 8.0], [0.5, 7.0]],
        });
        assert_eq!(mapped.text, "斜行 ABC");
        assert!((mapped.score - 0.87).abs() < 1e-6);
        assert_eq!(
            mapped.quad,
            [[1.0, 2.0], [3.0, 2.5], [2.5, 8.0], [0.5, 7.0]],
            "box 四点应原样进 quad（旋转保真）"
        );
    }

    /// 归一化编辑距离相似度（与旧 pdf_text_probe 同一算法）。
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
