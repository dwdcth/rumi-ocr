//! M3a 集成测试：批量识别、文件装载（png/jpg/webp）、PDF 分页（pdftoppm）、二维码（rqrr）。
//!
//! 依赖：系统 `pdftoppm`（poppler）——缺失时 PDF 用例自动跳过并说明。
//! 临时文件统一放 std::env::temp_dir() + 进程号，用完即删。

use image::{GrayImage, Luma, RgbaImage};
use qrcodegen::{QrCode, QrCodeEcc};
use rumi_ocr::{OcrEngine, load_frame, load_frame_from_bytes, load_pdf, scan_qr};
use std::path::PathBuf;

fn unique_tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    std::env::temp_dir().join(format!("rumi-m3test-{}-{}-{}", tag, std::process::id(), nanos))
}

fn has_pdftoppm() -> bool {
    std::process::Command::new("pdftoppm")
        .arg("-v")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 纯色 RGBA 图像（K2：ocr 统一图像载体 image::RgbaImage）。
fn solid_img(w: u32, h: u32, px: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(w, h, image::Rgba(px))
}

/// 图像 → PNG 字节（无损）。
fn encode_png(img: &RgbaImage) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("PNG 编码失败");
    buf.into_inner()
}

// ---------------------------------------------------------------------------
// load_frame / load_frame_from_bytes
// ---------------------------------------------------------------------------

#[test]
fn load_frame_roundtrip_png_jpeg_webp() {
    let img = solid_img(24, 16, [200, 30, 40, 255]);
    let png = encode_png(&img);

    // 内存字节入口（HTTP base64 路径）
    let f = load_frame_from_bytes(&png).expect("png 字节解码");
    assert_eq!(f.dimensions(), (24, 16));

    // 文件入口 ×3 种格式
    let dir = unique_tmp("fmt");
    std::fs::create_dir_all(&dir).unwrap();
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).into_rgb8();

    std::fs::write(dir.join("a.png"), &png).unwrap();
    rgb.save(dir.join("b.jpg")).expect("存 jpg");
    rgb.save(dir.join("c.webp")).expect("存 webp"); // image 无损 webp 编码

    for name in ["a.png", "b.jpg", "c.webp"] {
        let f = load_frame(&dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(f.dimensions(), (24, 16), "{name} 尺寸不符");
        let px = f.get_pixel(0, 0).0;
        assert!(px[1].abs_diff(30) < 10, "{name} 色彩偏差过大：{px:?}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_frame_reports_missing_file_and_bad_format() {
    let dir = unique_tmp("err");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.png"), b"not an image").unwrap();

    assert!(load_frame(&dir.join("nope.png")).is_err());
    assert!(load_frame(&dir.join("bad.png")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// load_pdf（pdftoppm 子进程）
// ---------------------------------------------------------------------------

/// 手写最小合法 PDF（N 页，Helvetica 文本），xref 偏移逐字节计算。
fn build_pdf(texts: &[&str]) -> Vec<u8> {
    let n = texts.len();
    let mut body = String::from("%PDF-1.4\n");
    let mut offsets: Vec<usize> = Vec::new();
    fn add(body: &mut String, offsets: &mut Vec<usize>, obj: String) {
        offsets.push(body.len());
        body.push_str(&obj);
        body.push('\n');
    }

    add(&mut body, &mut offsets, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj".into());
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + i)).collect();
    add(
        &mut body,
        &mut offsets,
        format!("2 0 obj\n<< /Type /Pages /Kids [{}] /Count {n} >>\nendobj", kids.join(" ")),
    );
    for i in 0..n {
        add(
            &mut body,
            &mut offsets,
            format!(
                "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>\nendobj",
                3 + i,
                3 + 2 * n,
                3 + n + i
            ),
        );
    }
    for (i, text) in texts.iter().enumerate() {
        let stream = format!("BT /F1 48 Tf 72 700 Td ({text}) Tj ET");
        add(
            &mut body,
            &mut offsets,
            format!(
                "{} 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj",
                3 + n + i,
                stream.len(),
                stream
            ),
        );
    }
    add(
        &mut body,
        &mut offsets,
        format!(
            "{} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj",
            3 + 2 * n
        ),
    );

    let xref_off = body.len();
    let count = offsets.len() + 1;
    let mut xref = format!("xref\n0 {count}\n0000000000 65535 f \n");
    for off in &offsets {
        xref.push_str(&format!("{off:010} 00000 n \n"));
    }
    body.push_str(&xref);
    body.push_str(&format!(
        "trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n"
    ));
    body.into_bytes()
}

#[test]
fn load_pdf_splits_pages_in_order() {
    if !has_pdftoppm() {
        eprintln!("SKIP：系统无 pdftoppm（poppler-utils），PDF 用例跳过");
        return;
    }
    let dir = unique_tmp("pdf");
    std::fs::create_dir_all(&dir).unwrap();
    let pdf_path = dir.join("two-pages.pdf");
    std::fs::write(&pdf_path, build_pdf(&["Rumi PDF Page One", "Rumi PDF Page Two"])).unwrap();

    let frames = load_pdf(&pdf_path).expect("PDF 分页");
    assert_eq!(frames.len(), 2, "应产出 2 页，实际 {}", frames.len());
    let (w0, h0) = frames[0].dimensions();
    assert!(w0 > 100 && h0 > 100, "A4 页面渲染尺寸异常");
    assert_eq!(frames[0].len(), (w0 * h0 * 4) as usize);
    let (w1, h1) = frames[1].dimensions();
    eprintln!("load_pdf：2 页 -> {}x{}, {}x{}", w0, h0, w1, h1);

    assert!(load_pdf(&dir.join("missing.pdf")).is_err(), "缺失文件应报错");
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// scan_qr（rqrr）
// ---------------------------------------------------------------------------

/// 用 qrcodegen 生成指定文本的灰度图（8px/模块 + 4 模块静区）。
fn qr_frame(text: &str) -> RgbaImage {
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).expect("QR 编码失败");
    let size = qr.size() as u32;
    let (scale, quiet) = (8u32, 4u32);
    let dim = (size + quiet * 2) * scale;
    let mut img: GrayImage = GrayImage::from_pixel(dim, dim, Luma([255]));
    for y in 0..size as i32 {
        for x in 0..size as i32 {
            if qr.get_module(x, y) {
                let (x0, y0) = ((quiet + x as u32) * scale, (quiet + y as u32) * scale);
                for dy in 0..scale {
                    for dx in 0..scale {
                        img.put_pixel(x0 + dx, y0 + dy, Luma([0]));
                    }
                }
            }
        }
    }
    image::DynamicImage::ImageLuma8(img).into_rgba8()
}

#[test]
fn scan_qr_decodes_generated_code() {
    let frame = qr_frame("HELLO RUMI QR");
    let found = scan_qr(&frame);
    assert_eq!(found, vec!["HELLO RUMI QR".to_string()], "二维码解码不符：{found:?}");
}

#[test]
fn scan_qr_no_code_returns_empty() {
    assert!(scan_qr(&solid_img(64, 64, [255, 255, 255, 255])).is_empty());
}

// ---------------------------------------------------------------------------
// recognize_many（真实引擎，模型命中 ~/.oar 缓存后离线可跑）
// ---------------------------------------------------------------------------

#[test]
fn recognize_many_matches_input_order() {
    let dir = unique_tmp("batch");
    std::fs::create_dir_all(&dir).unwrap();
    let p1 = dir.join("a.png");
    let p2 = dir.join("b.png");
    std::fs::write(&p1, encode_png(&solid_img(64, 64, [255, 255, 255, 255]))).unwrap();
    std::fs::write(&p2, encode_png(&solid_img(48, 48, [10, 10, 10, 255]))).unwrap();
    let frames = vec![load_frame(&p1).unwrap(), load_frame(&p2).unwrap()];

    let engine = OcrEngine::global();
    let t0 = std::time::Instant::now();
    let results = engine.recognize_many(&frames).expect("批量识别");
    eprintln!("recognize_many 2 帧：{:?}", t0.elapsed());
    assert_eq!(results.len(), 2, "结果组数须等于输入帧数");

    // 空输入直接返回空
    assert!(engine.recognize_many(&[]).unwrap().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}
