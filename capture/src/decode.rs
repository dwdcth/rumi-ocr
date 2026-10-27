//! 图像/路径解码工具

use crate::{Error, Frame, Result};
use image::GenericImageView;

/// 读取 PNG 文件 → RGBA Frame
pub fn png_file_to_frame(path: &str) -> Result<Frame> {
    let img = image::open(path).map_err(|e| Error::Backend(format!("解码 {path}: {e}")))?;
    let (width, height) = img.dimensions();
    let rgba = img.to_rgba8().into_raw();
    Ok(Frame { rgba, width, height })
}

pub fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// portal 返回的 file:// URI 中非 ASCII 路径会被百分号编码（如中文目录）
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
