//! 冒烟辅助：生成二维码 PNG（供 `rumi-ocr-cli --qr` 验收）。
//!
//! 用法：`cargo run -p rumi-ocr --example make_qr -- "文本" out.png [scale]`
//!
//! 例（examples 属 dev 目标，可用 qrcodegen）：
//! 生成 8px/模块 + 4 模块静区的灰度二维码。

use qrcodegen::{QrCode, QrCodeEcc};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [text, out] = args.as_slice() else {
        eprintln!("用法: make_qr <文本> <输出.png> [scale=8]");
        std::process::exit(2);
    };
    let scale: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);

    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).expect("QR 编码失败");
    let size = qr.size() as u32;
    let quiet = 4u32;
    let dim = (size + quiet * 2) * scale;
    let mut img = image::GrayImage::from_pixel(dim, dim, image::Luma([255]));
    for y in 0..size as i32 {
        for x in 0..size as i32 {
            if qr.get_module(x, y) {
                let (x0, y0) = ((quiet + x as u32) * scale, (quiet + y as u32) * scale);
                for dy in 0..scale {
                    for dx in 0..scale {
                        img.put_pixel(x0 + dx, y0 + dy, image::Luma([0]));
                    }
                }
            }
        }
    }
    image::DynamicImage::ImageLuma8(img)
        .save(out)
        .expect("写 PNG 失败");
    println!("已生成 {out}（{dim}x{dim}）");
}
