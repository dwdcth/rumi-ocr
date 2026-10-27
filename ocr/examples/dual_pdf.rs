//! dual_pdf 示例：合成一张含文字的 PNG → rumi_ocr 识别 → 导出双层可搜索 PDF。
//!
//! 闭环演示：视觉层（原图 XObject）+ 隐形文字层（text render mode 3），
//! 产出 PDF 可用 `pdftotext` 提取、可用阅读器搜索。
//!
//! 运行：
//! ```bash
//! cargo run -p rumi-ocr --example dual_pdf
//! ```
//! 首次运行会下载 OCR 模型（约 2.4s，之后走 ~/.oar 缓存）。

use rumi_ocr::dual_pdf::{export_dual_layer_pdf, DualLayerStats};
use rumi_ocr::{load_frame, OcrEngine, OcrLine};

fn main() {
    let out_dir = std::path::Path::new("target/dual_pdf_demo");
    std::fs::create_dir_all(out_dir).expect("创建输出目录失败");
    let png_path = out_dir.join("sample.png");
    let pdf_path = out_dir.join("sample.pdf");

    // 1. 合成样本：白底黑字，中英混排
    let png = make_sample_png(&[
        "Rumi dual-layer PDF demo",
        "你好世界，可搜索文本层",
        "poppler pdftotext 12345",
    ]);
    std::fs::write(&png_path, &png).expect("写样本 PNG 失败");
    println!("样本图：{}", png_path.display());

    // 2. OCR 识别
    let img = load_frame(&png_path).expect("装载 PNG 失败");
    let engine = OcrEngine::global();
    engine.warmup().expect("OCR 引擎构建失败");
    let lines: Vec<OcrLine> = engine.recognize(&img).expect("识别失败");
    println!("识别 {} 行：", lines.len());
    for l in &lines {
        println!("  [{:.3}] {} @{:?}", l.score, l.text, l.r#box[0]);
    }

    // 3. 导出双层 PDF
    let stats: DualLayerStats =
        export_dual_layer_pdf(&pdf_path, &[(png, lines)]).expect("导出双层 PDF 失败");
    println!("导出：{}", pdf_path.display());
    println!(
        "文字层：写入 {} 行，跳过 {} 行{}",
        stats.written,
        stats.skipped,
        if stats.skipped > 0 { "（多为字体未覆盖的行）" } else { "" }
    );

    // 4. 验证提示：可用 pdftotext 抽取验证可搜索性
    println!(
        "验证：pdftotext {} -   # 提取结果应包含上面识别出的文本",
        pdf_path.display()
    );
}

/// 白底黑字合成 PNG（1200 宽、110px 行高、72px 字号，利于 OCR 检测）。
fn make_sample_png(lines: &[&str]) -> Vec<u8> {
    let font_path = find_font().expect("系统缺少可用字体（ttf/ttc/otf）");
    let bytes = std::fs::read(&font_path).expect("读取字体失败");
    let font = ab_glyph_font(bytes);

    let (w, line_h) = (1200u32, 110u32);
    let h = line_h * lines.len() as u32 + 40;
    let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
    for (i, text) in lines.iter().enumerate() {
        imageproc::drawing::draw_text_mut(
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

/// 找一个能渲染中文的字体（Noto Sans CJK 优先，回退任意 ttf/ttc/otf）。
fn find_font() -> Option<std::path::PathBuf> {
    let roots = ["/usr/share/fonts", "/usr/local/share/fonts"];
    let prefer = ["NotoSansCJK-Regular", "NotoSansCJK-Bold", "DejaVuSans.ttf"];
    for root in roots {
        let mut all = Vec::new();
        collect_fonts(std::path::Path::new(root), &mut all);
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

fn collect_fonts(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
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

/// ttc 字体集合按索引 0 加载，普通 ttf/otf 直接加载。
fn ab_glyph_font(bytes: Vec<u8>) -> ab_glyph::FontArc {
    if bytes.starts_with(b"ttcf") {
        ab_glyph::FontArc::from(
            ab_glyph::FontVec::try_from_vec_and_index(bytes, 0).expect("ttc 加载失败"),
        )
    } else {
        ab_glyph::FontArc::try_from_vec(bytes).expect("字体加载失败")
    }
}
