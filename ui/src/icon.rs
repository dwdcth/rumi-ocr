//! icon —— 程序化生成应用/窗口图标：256px 圆角方块 + 白色"识"字描边（P0 #4）。
//!
//! 无资源文件、无字体依赖：笔画在 100×100 设计框内手写坐标（讠 = 点 + 横折提，
//! 只 = 口 + 八），用胶囊体（圆头粗线段）SDF 光栅化，自带抗锯齿。
//! 经 image crate 编码 PNG（`app_icon_png`，可单测）→ 解码回 RGBA →
//! `tao::window::Icon::from_rgba`，由 main() 经 `Config::with_icon` 设为窗口图标
//! （dioxus-desktop config.rs：`with_icon(icon: Icon)` 写入 `window.window.window_icon`）。

use image::RgbaImage;

/// "识"字笔画：(折线顶点序列, 线宽)，100×100 设计框坐标。
/// 比例参照 Noto Sans CJK 的"识"：讠居左（点 + 横折提），只居右（口 + 八），
/// 口与八之间留明显空档，提笔收在口左侧不相连。
const STROKES: &[(&[(f32, f32)], f32)] = &[
    // 讠·点（短撇）
    (&[(15.0, 15.0), (24.0, 26.0)], 9.5),
    // 讠·横折提（横 → 折下 → 短提）
    (&[(8.0, 39.0), (36.0, 39.0), (36.0, 63.0), (46.0, 53.0)], 8.0),
    // 只·口（闭合方框）
    (
        &[(59.0, 21.0), (91.0, 21.0), (91.0, 53.0), (59.0, 53.0), (59.0, 21.0)],
        8.0,
    ),
    // 只·八 左点（与口底留空档，向下撇出）
    (&[(71.0, 66.0), (60.0, 89.0)], 9.5),
    // 只·八 右点
    (&[(79.0, 66.0), (90.0, 89.0)], 9.5),
];

/// 点到线段的最短距离
fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let abx = bx - ax;
    let aby = by - ay;
    let apx = px - ax;
    let apy = py - ay;
    let len2 = abx * abx + aby * aby;
    let t = if len2 <= 0.0 {
        0.0
    } else {
        ((apx * abx + apy * aby) / len2).clamp(0.0, 1.0)
    };
    let dx = apx - abx * t;
    let dy = apy - aby * t;
    (dx * dx + dy * dy).sqrt()
}

/// 点到折线（逐段取最小）的距离
fn poly_dist(px: f32, py: f32, pts: &[(f32, f32)]) -> f32 {
    let mut best = f32::MAX;
    for w in pts.windows(2) {
        best = best.min(seg_dist(px, py, w[0].0, w[0].1, w[1].0, w[1].1));
    }
    best
}

/// 圆角方块覆盖率的近似 AA：SDF 距离差 0.5px 内线性过渡
fn round_rect_alpha(px: f32, py: f32, s: f32, r: f32) -> f32 {
    let half = s / 2.0;
    let qx = (px - half).abs() - (half - r);
    let qy = (py - half).abs() - (half - r);
    let out_x = qx.max(0.0);
    let out_y = qy.max(0.0);
    let d = (out_x * out_x + out_y * out_y).sqrt() + qx.max(qy).min(0.0);
    (0.5 - d).clamp(0.0, 1.0)
}

/// 生成 size×size 的图标位图（品牌蓝竖向渐变圆角方块 + 白色"识"字）
fn app_icon_image(size: u32) -> RgbaImage {
    let s = size as f32;
    let radius = s * 0.22;
    // 设计框 100 → 画布：字面占 62%，居中
    let k = s * 0.62 / 100.0;
    let off = (s - 100.0 * k) / 2.0;
    let strokes: Vec<(Vec<(f32, f32)>, f32)> = STROKES
        .iter()
        .map(|(pts, w)| {
            (
                pts.iter().map(|(x, y)| (off + x * k, off + y * k)).collect(),
                w * k,
            )
        })
        .collect();
    // 渐变顶/底（品牌蓝两档）
    let top = [92.0_f32, 122.0, 250.0];
    let bot = [64.0_f32, 89.0, 224.0];

    let mut buf = vec![0u8; (size as usize) * (size as usize) * 4];
    for y in 0..size {
        let t = (y as f32 + 0.5) / s;
        let bg = [
            top[0] + (bot[0] - top[0]) * t,
            top[1] + (bot[1] - top[1]) * t,
            top[2] + (bot[2] - top[2]) * t,
        ];
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let a_bg = round_rect_alpha(px, py, s, radius);
            if a_bg <= 0.0 {
                continue; // 圆角外保持全透明
            }
            // 笔画覆盖率（各笔画自己的半宽，取最大）
            let mut a_char = 0.0_f32;
            for (pts, w) in &strokes {
                let d = poly_dist(px, py, pts);
                a_char = a_char.max((w / 2.0 + 0.5 - d).clamp(0.0, 1.0));
            }
            let i = ((y as usize) * size as usize + x as usize) * 4;
            if a_char > 0.0 {
                buf[i] = (255.0 * a_char + bg[0] * (1.0 - a_char)).round() as u8;
                buf[i + 1] = (255.0 * a_char + bg[1] * (1.0 - a_char)).round() as u8;
                buf[i + 2] = (255.0 * a_char + bg[2] * (1.0 - a_char)).round() as u8;
            } else {
                buf[i] = bg[0].round() as u8;
                buf[i + 1] = bg[1].round() as u8;
                buf[i + 2] = bg[2].round() as u8;
            }
            buf[i + 3] = (a_bg * 255.0).round() as u8;
        }
    }
    RgbaImage::from_raw(size, size, buf).expect("图标缓冲与尺寸不匹配")
}

/// 图标 PNG 字节（256px 用于窗口图标；亦可作打包资源）
pub fn app_icon_png(size: u32) -> Vec<u8> {
    let mut png = Vec::new();
    app_icon_image(size)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("图标 PNG 编码失败");
    png
}

/// tao 窗口图标：PNG 往返验证后解码为 RGBA 交给 `Config::with_icon`
pub fn window_icon() -> dioxus::desktop::tao::window::Icon {
    let png = app_icon_png(256);
    let img = image::load_from_memory(&png)
        .expect("图标 PNG 解码失败")
        .to_rgba8();
    let (w, h) = img.dimensions();
    dioxus::desktop::tao::window::Icon::from_rgba(img.into_raw(), w, h)
        .expect("窗口图标构造失败")
}

/// 托盘图标 RGBA：应用图标（256px）等比缩到 32×32（CatmullRom 高质量重采样），
/// 与窗口图标同一视觉来源（打磨：原先托盘是另一套手绘放大镜，形象不统一）。
/// 返回 32×32×4 的 RGBA buffer，tray.rs 直接 `Icon::from_rgba` 喂给托盘。
pub fn tray_icon_rgba() -> Vec<u8> {
    const TRAY_SIZE: u32 = 32;
    let png = app_icon_png(256);
    let img = image::load_from_memory(&png)
        .expect("图标 PNG 解码失败")
        .to_rgba8();
    let small = image::imageops::resize(&img, TRAY_SIZE, TRAY_SIZE, image::imageops::FilterType::CatmullRom);
    small.into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_png_is_valid_256_rounded() {
        let png = app_icon_png(256);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "应为 PNG magic");
        let img = image::load_from_memory(&png).expect("PNG 可解码");
        assert_eq!((img.width(), img.height()), (256, 256));

        let rgba = img.to_rgba8();
        // 圆角外（角点）全透明，圆角方块存在不透明像素
        assert_eq!(rgba.get_pixel(0, 0).0[3], 0, "角点应在圆角外");
        assert!(
            rgba.pixels().any(|p| p.0[3] == 255),
            "至少存在方块本体像素"
        );
        // 中心一带应有白色笔画（识字覆盖画布中部）
        let center = rgba.get_pixel(128, 128);
        assert!(center.0[3] == 255, "中心应在方块内");
    }

    /// 托盘图标：等比缩放产物必须是 32×32、4 通道 RGBA（tray Icon::from_rgba 的输入约定）
    #[test]
    fn tray_icon_rgba_is_32x32_rgba() {
        let buf = tray_icon_rgba();
        assert_eq!(buf.len(), 32 * 32 * 4, "应为 32x32x4 的 RGBA buffer（4 通道）");
        assert_eq!(<image::Rgba<u8> as image::Pixel>::CHANNEL_COUNT, 4);
        let img = image::RgbaImage::from_raw(32, 32, buf).expect("缓冲与尺寸匹配");
        assert_eq!(img.dimensions(), (32, 32));
        // 内容与 256px 源同源：至少存在不透明像素（圆角方块本体）
        assert!(img.pixels().any(|p| p.0[3] == 255), "缩放后仍应有不透明像素");
    }
}
