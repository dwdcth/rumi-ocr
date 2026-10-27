//! formula.rs：公式识别示例（oar-ocr 结构化管线：PP-DocLayout + PP-FormulaNet）。
//!
//! 合成一张简单公式图（ab_glyph 手绘，同 rumi-ocr 测试的做法）→
//! [`rumi_ocr::OcrEngine::recognize_structured`] → 逐行打印（公式行 text 为 LaTeX）。
//!
//! 运行（首次会下载 PP-DocLayout-S 约 5 MiB + PP-FormulaNet-S 约 221 MiB
//! 与分词器 2 MiB，缓存在 ~/.oar，之后秒开）：
//!
//! ```bash
//! cargo run -p rumi-ocr --example formula
//! ```
//!
//! 合成图会存到系统临时目录（路径见输出），便于对照识别效果。

use ab_glyph::FontArc;
use image::RgbaImage;
use imageproc::drawing::draw_text_mut;
use rumi_ocr::OcrEngine;
use std::path::{Path, PathBuf};

/// 找一个可用字体（Noto Sans CJK 优先，回退任意 ttf/ttc/otf）。
fn find_font() -> Option<PathBuf> {
    let roots = ["/usr/share/fonts", "/usr/local/share/fonts"];
    let prefer = [
        "NotoSansCJK-Regular",
        "NotoSansCJK-Bold",
        "NotoSansCJK",
        "DejaVuSans.ttf",
    ];
    for root in roots {
        let mut all: Vec<PathBuf> = Vec::new();
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

/// ttc 字体集合按索引 0 加载，普通 ttf/otf 直接加载。
fn ab_glyph_from_bytes(bytes: Vec<u8>) -> FontArc {
    if bytes.starts_with(b"ttcf") {
        let fv = ab_glyph::FontVec::try_from_vec_and_index(bytes, 0).expect("ttc 加载失败");
        FontArc::from(fv)
    } else {
        FontArc::try_from_vec(bytes).expect("字体加载失败")
    }
}

fn main() {
    // 1. 合成公式图：白底黑字，px 给大（96px），版面/公式模型对小字不友好
    let formula = "x^2 + y^2 = r^2";
    let font_path = find_font().expect("系统缺少可用字体（ttf/ttc/otf）");
    let font = ab_glyph_from_bytes(std::fs::read(&font_path).expect("读取字体失败"));
    let (w, h) = (1200u32, 300u32);
    let mut img = RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
    draw_text_mut(
        &mut img,
        image::Rgba([0, 0, 0, 255]),
        40,
        90,
        96.0,
        &font,
        formula,
    );
    // 存一份到临时目录，方便人工对照识别效果
    let png = std::env::temp_dir().join("rumi-formula-example.png");
    img.save(&png).expect("保存合成图失败");
    println!("合成图（{formula}）已存：{}", png.display());

    // 2. 识别（首次构建公式引擎会下载约 230 MiB 模型）
    println!("构建公式引擎/识别中（首次下载约 230 MiB，缓存 ~/.oar）……");
    let engine = OcrEngine::global();
    let t0 = std::time::Instant::now();
    let lines = engine.recognize_structured(&img).expect("识别失败");
    println!("识别完成（{:?}），共 {} 行：", t0.elapsed(), lines.len());
    for l in &lines {
        println!("  [{:.3}] {}", l.score, l.text);
    }
    if lines.is_empty() {
        eprintln!("（未识别出任何行）");
    }
}
