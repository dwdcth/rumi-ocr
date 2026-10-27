//! rumi-ocr-cli：M3 后端能力冒烟入口（供主管验收，不依赖 GUI）。
//!
//! 用法：
//! ```text
//! rumi-ocr-cli file a.png [b.jpg c.webp ...]    # 图片批量识别（png/jpg/webp）
//! rumi-ocr-cli --pdf x.pdf [--dpi 200] [--force-raster]   # PDF：默认智能路由，文字层页直接提取
//! rumi-ocr-cli --qr a.png                       # 二维码扫描
//! ```
//!
//! PDF 两种模式：
//! - 默认（智能路由）：pdf-inspector 逐页分类，文字层页直接提取文本+坐标
//!   （页标注 `[text-layer]`，跳过渲染与 OCR 引擎），扫描页才渲染+识别；
//! - `--force-raster`：旧行为，整本 pdftoppm 全渲染后走引擎。
//!
//! 输出：逐行 `[score] text @ 左上角坐标`，末尾统计（输入数 / 行数 / 平均置信度 / 耗时）。
//! 退出码：0 成功；1 运行失败；2 用法错误。

use rumi_ocr::pdf_route::PdfPage;
use rumi_ocr::{OcrEngine, OcrLine};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(Exit::Usage(msg)) => {
            eprintln!("用法错误：{msg}\n\n{}", USAGE.trim());
            2
        }
        Err(Exit::Fail(msg)) => {
            eprintln!("❌ {msg}");
            1
        }
    };
    std::process::exit(code);
}

const USAGE: &str = "用法:
  rumi-ocr-cli file a.png [b.jpg c.webp ...]
  rumi-ocr-cli --pdf x.pdf [--dpi 200] [--force-raster]
  rumi-ocr-cli --qr a.png";

enum Exit {
    Usage(String),
    Fail(String),
}

fn usage<T: Into<String>>(msg: T) -> Exit {
    Exit::Usage(msg.into())
}

struct Stats {
    inputs: usize,
    lines: usize,
    score_sum: f64,
}

impl Stats {
    fn new() -> Self {
        Self { inputs: 0, lines: 0, score_sum: 0.0 }
    }
    fn feed(&mut self, lines: &[OcrLine]) {
        self.inputs += 1;
        self.lines += lines.len();
        self.score_sum += lines.iter().map(|l| l.score as f64).sum::<f64>();
    }
    fn report(&self, elapsed: std::time::Duration) {
        let avg = if self.lines > 0 { self.score_sum / self.lines as f64 } else { 0.0 };
        println!(
            "== 统计：输入 {} 个，共 {} 行，平均置信度 {:.3}，总耗时 {:.1}s",
            self.inputs,
            self.lines,
            avg,
            elapsed.as_secs_f32()
        );
    }
}

fn run(args: &[String]) -> Result<(), Exit> {
    match args.first().map(|s| s.as_str()) {
        Some("file") => run_files(&args[1..]),
        Some("--pdf") => run_pdf(&args[1..]),
        Some("--qr") => run_qr(&args[1..]),
        Some(other) => Err(usage(format!("未知命令 {other}"))),
        None => Err(usage("缺少命令")),
    }
}

fn run_files(paths: &[String]) -> Result<(), Exit> {
    if paths.is_empty() {
        return Err(usage("file 模式至少需要一个图片文件"));
    }
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let mut frames = Vec::with_capacity(paths.len());
    for p in &paths {
        frames.push(
            rumi_ocr::load_frame(p).map_err(|e| Exit::Fail(e.to_string()))?,
        );
    }

    let engine = OcrEngine::global();
    let t0 = Instant::now();
    engine
        .warmup()
        .map_err(|e| Exit::Fail(format!("引擎预热失败：{e}")))?;
    let results = engine
        .recognize_many(&frames)
        .map_err(|e| Exit::Fail(format!("批量识别失败：{e}")))?;
    let elapsed = t0.elapsed();

    let mut stats = Stats::new();
    for (p, lines) in paths.iter().zip(&results) {
        print_lines(&p.display().to_string(), lines);
        stats.feed(lines);
    }
    stats.report(elapsed);
    Ok(())
}

fn run_pdf(args: &[String]) -> Result<(), Exit> {
    let mut pdf: Option<PathBuf> = None;
    let mut dpi = 200u32;
    let mut force_raster = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dpi" => {
                dpi = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| usage("--dpi 需要一个整数参数"))?;
                i += 2;
            }
            "--force-raster" => {
                force_raster = true;
                i += 1;
            }
            s if s.starts_with("--") => return Err(usage(format!("未知参数 {s}"))),
            s => {
                if pdf.is_some() {
                    return Err(usage("--pdf 只接受一个文件"));
                }
                pdf = Some(PathBuf::from(s));
                i += 1;
            }
        }
    }
    let pdf = pdf.ok_or_else(|| usage("--pdf 需要一个 PDF 文件路径"))?;
    let mut stats = Stats::new();
    let started = Instant::now();
    if force_raster {
        run_pdf_raster(&pdf, dpi, &mut stats)?;
    } else {
        run_pdf_route(&pdf, &mut stats)?;
    }
    stats.report(started.elapsed());
    Ok(())
}

/// 旧全渲染管线：整本 pdftoppm → 引擎逐页识别（--force-raster）。
fn run_pdf_raster(pdf: &PathBuf, dpi: u32, stats: &mut Stats) -> Result<(), Exit> {
    let t0 = Instant::now();
    let frames =
        rumi_ocr::load_pdf_dpi(pdf, dpi).map_err(|e| Exit::Fail(e.to_string()))?;
    let load_elapsed = t0.elapsed();

    let engine = OcrEngine::global();
    engine
        .warmup()
        .map_err(|e| Exit::Fail(format!("引擎预热失败：{e}")))?;
    let t1 = Instant::now();
    let results = engine
        .recognize_many(&frames)
        .map_err(|e| Exit::Fail(format!("批量识别失败：{e}")))?;
    let ocr_elapsed = t1.elapsed();

    for (i, lines) in results.iter().enumerate() {
        print_lines(&format!("{}#第{}页", pdf.display(), i + 1), lines);
        stats.feed(lines);
    }
    println!(
        "== PDF（--force-raster 全渲染）：{} 页渲染 {:.1}s，识别 {:.1}s",
        frames.len(),
        load_elapsed.as_secs_f32(),
        ocr_elapsed.as_secs_f32()
    );
    Ok(())
}

/// 智能路由：文字层页直接提取（零引擎），扫描页渲染后批量识别。
fn run_pdf_route(pdf: &PathBuf, stats: &mut Stats) -> Result<(), Exit> {
    let t0 = Instant::now();
    let pages =
        rumi_ocr::pdf_route::route_pdf(pdf).map_err(|e| Exit::Fail(e.to_string()))?;
    let route_elapsed = t0.elapsed();

    // 扫描页帧收集起来，一次引擎锁批量识别；文字页行零成本直出
    let raster_frames: Vec<_> = pages
        .iter()
        .filter_map(|p| match p {
            PdfPage::Raster(frame) => Some(frame.clone()),
            PdfPage::Text(_) => None,
        })
        .collect();
    let mut recognized: Vec<Vec<OcrLine>> = Vec::new();
    let mut ocr_elapsed = std::time::Duration::ZERO;
    if !raster_frames.is_empty() {
        let engine = OcrEngine::global();
        engine
            .warmup()
            .map_err(|e| Exit::Fail(format!("引擎预热失败：{e}")))?;
        let t1 = Instant::now();
        recognized = engine
            .recognize_many(&raster_frames)
            .map_err(|e| Exit::Fail(format!("批量识别失败：{e}")))?;
        ocr_elapsed = t1.elapsed();
    }
    let mut it = recognized.into_iter();

    let mut text_pages = 0usize;
    let mut raster_pages = 0usize;
    for (i, page) in pages.iter().enumerate() {
        match page {
            PdfPage::Text(lines) => {
                text_pages += 1;
                print_lines(&format!("{}#第{}页 [text-layer]", pdf.display(), i + 1), lines);
                stats.feed(lines);
            }
            PdfPage::Raster(_) => {
                raster_pages += 1;
                let lines = it.next().unwrap_or_default();
                print_lines(&format!("{}#第{}页", pdf.display(), i + 1), &lines);
                stats.feed(&lines);
            }
        }
    }
    println!(
        "== PDF（智能路由）：{} 页 = 文字层 {text_pages}（直提取）+ 扫描 {raster_pages}（渲染+识别 {:.1}s），路由 {:.1}s",
        pages.len(),
        ocr_elapsed.as_secs_f32(),
        route_elapsed.as_secs_f32()
    );
    Ok(())
}

fn run_qr(args: &[String]) -> Result<(), Exit> {
    let path = match args {
        [p] if !p.starts_with("--") => PathBuf::from(p),
        _ => return Err(usage("--qr 需要恰好一个图片文件路径")),
    };
    let frame = rumi_ocr::load_frame(&path).map_err(|e| Exit::Fail(e.to_string()))?;
    let t0 = Instant::now();
    let found = rumi_ocr::scan_qr(&frame);
    println!(
        "== {}：{} 个二维码，耗时 {:?}",
        path.display(),
        found.len(),
        t0.elapsed()
    );
    for (i, text) in found.iter().enumerate() {
        println!("  [{i}] {text}");
    }
    if found.is_empty() {
        eprintln!("（未识别到二维码）");
    }
    Ok(())
}

fn print_lines(tag: &str, lines: &[OcrLine]) {
    println!("== {tag}：{} 行", lines.len());
    for l in lines {
        println!("  [{:.3}] {} @ {:?}", l.score, l.text, l.r#box[0]);
    }
}
