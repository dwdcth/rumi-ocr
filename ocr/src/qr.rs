//! 二维码生成（P2 #13 的纯函数部分，UI 工具卡后续接入）：
//! 文本 → 白底黑码 PNG，四周 4 模块静区。
//!
//! 选型：`qrcodegen`（Nayuki 出品、零依赖、自己画像素）从 dev-deps 转正——
//! 生成端只产布尔矩阵，PNG 编码走本 crate 已有的 `image`，不引入第二个图像栈；
//! 识别闭环复用 [`crate::scan_qr`]（rqrr）。

use crate::OcrError;
use qrcodegen::{QrCode, QrCodeEcc};
use std::io::Cursor;

/// 每侧静区（模块数）。QR 规范建议 4。
const QUIET_MODULES: u32 = 4;

/// 文本 → 二维码 PNG 字节。
///
/// - `scale`：每个模块的像素边长（≥1）；整图边长 = 模块数 × scale + 8
///   （左右各 4 模块静区）。
/// - 白底黑码、纠错级别 Medium（兼顾扫读容错与容量）。
/// - 文本按 UTF-8 字节模式编码；过长返回 [`OcrError::Qr`]。
pub fn make_qr_png(text: &str, scale: u8) -> Result<Vec<u8>, OcrError> {
    if scale == 0 {
        return Err(OcrError::Qr("scale 必须 ≥ 1".into()));
    }
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium)
        .map_err(|e| OcrError::Qr(format!("二维码编码失败（文本过长？）：{e}")))?;
    let n = qr.size() as u32;
    let s = u32::from(scale);
    let dim = (n + QUIET_MODULES * 2) * s;
    let mut img = image::GrayImage::from_pixel(dim, dim, image::Luma([255u8]));
    for y in 0..n {
        for x in 0..n {
            if !qr.get_module(x as i32, y as i32) {
                continue;
            }
            let x0 = (x + QUIET_MODULES) * s;
            let y0 = (y + QUIET_MODULES) * s;
            for dy in 0..s {
                for dx in 0..s {
                    img.put_pixel(x0 + dx, y0 + dy, image::Luma([0u8]));
                }
            }
        }
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load_frame_from_bytes, scan_qr};
    use image::GenericImageView;

    #[test]
    fn png_dimensions_match_modules_times_scale_plus_quiet_zone() {
        let text = "https://rumi.example/wave1b";
        let expect_side = {
            let qr = QrCode::encode_text(text, QrCodeEcc::Medium).expect("编码失败");
            qr.size() as u32 + QUIET_MODULES * 2
        };
        for scale in [1u8, 4, 8] {
            let png = make_qr_png(text, scale).expect("生成失败");
            let img = image::load_from_memory(&png).expect("PNG 回读失败");
            assert_eq!(img.dimensions(), (expect_side * scale as u32, expect_side * scale as u32));
        }
    }

    #[test]
    fn generated_qr_decodes_back_to_original_text() {
        let text = "Rumi 二维码 123";
        let png = make_qr_png(text, 8).expect("生成失败");
        let frame = load_frame_from_bytes(&png).expect("装载失败");
        let texts = scan_qr(&frame);
        assert!(
            texts.iter().any(|t| t == text),
            "scan_qr 未解回原文，实际：{texts:?}"
        );
    }

    #[test]
    fn zero_scale_is_rejected() {
        assert!(matches!(make_qr_png("x", 0), Err(OcrError::Qr(_))));
    }
}
