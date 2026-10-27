//! rumi-capture CLI：后端探测、三种捕获模式冒烟，以及 `ocr` 一条龙
//! （截图 → 原生引擎识别 → 逐行打印，可 --copy 进剪贴板）。
//!
//! 用法：
//! ```text
//! rumi-capture [info|full|region|window] [输出.png]
//! rumi-capture ocr [--full] [--copy]
//! ```
//!
//! `ocr` 子命令：默认区域原生选区（--full 全屏）→ 原生引擎（rumi-ocr 的
//! oar-ocr PP-OCRv6 管线，固定 native，无引擎参数）→ 逐行 `[score] text`
//! 打印到 stdout。`--copy` 把全部文本写进剪贴板。
//!
//! 实现说明（K1 重构）：本 bin 位于组合 crate rumi-cli，同时链接
//! rumi-capture（截图）与 rumi-ocr（识别）两个 lib，`ocr` 子命令进程内
//! 直调 `OcrEngine::recognize`——capture 的 `Frame` 经 `into_rgba_image`
//! 零拷贝转成 ocr 的统一图像载体 `image::RgbaImage`（K2：ocr 不再依赖
//! capture crate）。旧方案曾因 capture↔ocr 包循环依赖而 spawn 姊妹二进制
//! rumi-ocr-cli 并解析其 stdout，随本重构退役（rumi-ocr-cli 本体仍在，
//! 作 M3 冒烟入口）。
//!
//! 退出码：0 成功；1 识别失败；2 用法错误。

use rumi_capture::{Capturer, Error};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("info");

    if cmd == "ocr" {
        // ocr 子命令不取输出路径参数，其余走原样
        let code = match parse_ocr_args(&args[1..]) {
            Ok(opts) => match run_ocr(&opts) {
                Ok(()) => 0,
                Err(msg) => {
                    eprintln!("OCR 失败：{msg}");
                    1
                }
            },
            Err(msg) => {
                eprintln!("用法错误：{msg}\n\n{}", OCR_USAGE.trim());
                2
            }
        };
        std::process::exit(code);
    }

    let out = args.get(1).cloned().unwrap_or_else(|| "capture.png".to_string());
    let r = futures::executor::block_on(run(cmd, &out));
    if let Err(e) = r {
        eprintln!("❌ {e}");
        std::process::exit(1);
    }
}

const OCR_USAGE: &str = "用法:
  rumi-capture ocr [--full] [--copy]
  --full  全屏截图（默认区域原生选区）
  --copy  识别文本写入剪贴板";

/// `ocr` 子命令参数（纯解析，便于单测）。
#[derive(Debug, Default, PartialEq)]
struct OcrOpts {
    /// 全屏而非区域选区
    full: bool,
    /// 识别文本进剪贴板
    copy: bool,
}

/// 解析 `ocr` 子命令的参数（不含子命令名本身）。未知参数报用法错误。
fn parse_ocr_args(args: &[String]) -> Result<OcrOpts, String> {
    let mut opts = OcrOpts::default();
    for a in args {
        match a.as_str() {
            "--full" => opts.full = true,
            "--copy" => opts.copy = true,
            other => return Err(format!("未知参数 {other}（支持 --full / --copy）")),
        }
    }
    Ok(opts)
}

/// 一条龙：选区/全屏截图 → 原生引擎进程内识别 → 逐行打印
/// `[score] text` → 可选进剪贴板。
fn run_ocr(opts: &OcrOpts) -> Result<(), String> {
    // 1. 截图（共享实例：探测/建连一次并缓存，KDE 失败自动降级 Portal 重试；
    //    阻塞选区/捕获过程在 async 内部自行处理，这里包一层 block_on）
    let frame = futures::executor::block_on(async {
        let cap = Capturer::shared().await.map_err(|e| e.to_string())?;
        if opts.full {
            cap.fullscreen().await.map_err(|e| e.to_string())
        } else {
            cap.region_native().await.map_err(|e| e.to_string())
        }
    })?;
    eprintln!("截图完成 ({}x{})，识别中……", frame.width, frame.height);

    // 2. 原生引擎进程内识别（首次含模型下载与 ONNX 构建，之后命中 ~/.oar 缓存）。
    //    Frame → ocr 统一图像载体零拷贝 move（K2：ocr 只认 image::RgbaImage）
    let img = frame.into_rgba_image();
    let engine = rumi_ocr::OcrEngine::global();
    engine.warmup().map_err(|e| format!("引擎预热失败：{e}"))?;
    let lines = engine.recognize(&img).map_err(|e| format!("识别失败：{e}"))?;

    // 3. 逐行打印 [score] text，同时收集全文
    let mut texts: Vec<String> = Vec::new();
    for l in &lines {
        println!("{}", format_line(l));
        texts.push(l.text.clone());
    }
    if texts.is_empty() {
        eprintln!("（未识别到文本）");
        return Ok(());
    }

    // 4. --copy：全部文本进剪贴板。Wayland 数据源要求进程短暂存活，
    //    拷完 park 一瞬再退（不能立即 drop/退出，否则剪贴板为空）。
    if opts.copy {
        let joined = texts.join("\n");
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("打开剪贴板失败：{e}"))?;
        clipboard
            .set_text(&joined)
            .map_err(|e| format!("写入剪贴板失败：{e}"))?;
        eprintln!("已复制 {} 行到剪贴板", texts.len());
        std::thread::park_timeout(Duration::from_millis(300));
    }
    Ok(())
}

/// 单行输出格式：`[score 三位小数] text`（与旧姊妹二进制方案的 stdout 逐字节一致）。
fn format_line(l: &rumi_ocr::OcrLine) -> String {
    format!("[{:.3}] {}", l.score, l.text)
}

async fn run(cmd: &str, out: &str) -> Result<(), Error> {
    let cap = match Capturer::probe().await {
        Ok(c) => c,
        Err(e) => {
            // 允许 --force-portal 调试
            if std::env::var("RUMI_FORCE_PORTAL").is_ok() {
                let conn = zbus::Connection::session()
                    .await
                    .map_err(|e| Error::NoBackend(e.to_string()))?;
                Capturer::Portal { conn }
            } else {
                return Err(e);
            }
        }
    };
    let caps = cap.caps();
    println!("后端: {:?}  caps: {caps:?}", cap.id());

    let frame = match cmd {
        "info" => {
            println!("OK：探测完成，不捕获");
            return Ok(());
        }
        "full" => cap.fullscreen().await?,
        "region" => cap.region_native().await?,
        "window" => cap.active_window().await?,
        other => {
            eprintln!("用法: rumi-capture [info|full|region|window|ocr] [输出.png]，未知命令 {other}");
            std::process::exit(2);
        }
    };

    // 简单存 PNG
    image::save_buffer(
        out,
        &frame.rgba,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| Error::Backend(format!("保存 {out}: {e}")))?;
    println!("✅ {out}  ({}x{})", frame.width, frame.height);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ocr_args_reads_flags() {
        let opts = parse_ocr_args(&["--full".into(), "--copy".into()]).expect("解析失败");
        assert_eq!(opts, OcrOpts { full: true, copy: true });

        assert_eq!(parse_ocr_args(&[]), Ok(OcrOpts::default()));
        assert_eq!(
            parse_ocr_args(&["--copy".into()]),
            Ok(OcrOpts { full: false, copy: true })
        );
    }

    #[test]
    fn parse_ocr_args_rejects_unknown_and_positional() {
        assert!(parse_ocr_args(&["--nope".into()]).is_err());
        // 历史行为残留：第二个位置参数是输出路径，ocr 模式不接受位置参数
        assert!(parse_ocr_args(&["out.png".into()]).is_err());
    }

    #[test]
    fn format_line_keeps_legacy_cli_format() {
        // 输出格式契约：`[score 三位小数] text`——与旧姊妹二进制方案的
        // stdout 逐行格式逐字节一致（此处原为 parse_cli_line 的解析端测试，
        // stdout 解析随 K1 退役后改为锁定打印端格式）。
        let mk = |text: &str, score: f32| rumi_ocr::OcrLine {
            text: text.into(),
            score,
            r#box: [[0.0; 2]; 4],
        };
        assert_eq!(format_line(&mk("你好世界", 0.987)), "[0.987] 你好世界");
        // 分数固定补足三位小数
        assert_eq!(format_line(&mk("纯文本", 0.1)), "[0.100] 纯文本");
    }
}
