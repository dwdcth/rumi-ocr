//! batch —— 批量 OCR 页（M3c + Wave2-C P2 #13 二维码工具卡 + P0 双层 PDF 导出）。
//!
//! 业务核心是纯函数 [`render_batch`]（文件 → 分组结果，可单测），
//! UI 只负责文件选择（rfd，经 run_blocking 避免 GTK 主线程死锁）、
//! 进度显示与导出。识别走 [`recognize_raw`]（native 直调引擎 / 插件 runBytes，
//! 与主页面共享引擎选择、busy 互斥与排版/忽略选项 OcrOpts::from_settings），
//! 展示行由原始 OcrLine 经同一 postprocess 规则映射（避免双份识别）。
//!
//! P0：识别时同时保留每页 PNG 字节（[`export_page`]）与原始 [`OcrLine`]（含 box），
//! 工具行"导出双层 PDF"按钮对识别成功的文件逐个调用
//! `rumi_ocr::dual_pdf::export_dual_layer_pdf`（图像视觉层 + 隐形可搜索文字层）。
//!
//! P2 #13：页尾"生成二维码"折叠卡——文本 + 模块边长 → `rumi_ocr::qr::make_qr_png`
//! → 内嵌预览 + rfd 另存。

use crate::i18n;
use crate::ocr_flow::{OcrOpts, UiLine};
use base64::Engine;
use dioxus::prelude::*;
use rumi_ocr::OcrLine;
use std::path::{Path, PathBuf};

/// 单个文件的批量结果
#[derive(Clone, PartialEq)]
pub struct BatchFileOut {
    pub path: String,
    /// 每页的识别行（普通图片 = 1 页）
    pub pages: Vec<Vec<UiLine>>,
    /// 每页 PNG 字节（双层 PDF 视觉层用；与 pages 同页序）
    pub page_pngs: Vec<Vec<u8>>,
    /// 每页原始识别行（保留 box，双层 PDF 文字层定位用；与 pages 同页序）
    pub lines_full: Vec<Vec<OcrLine>>,
    /// 二维码内容（顺带扫描，可空）
    pub qr: Vec<String>,
    /// 该文件失败原因（失败文件标红继续下一个）
    pub error: Option<String>,
}

impl BatchFileOut {
    pub fn ok(
        path: String,
        pages: Vec<Vec<UiLine>>,
        page_pngs: Vec<Vec<u8>>,
        lines_full: Vec<Vec<OcrLine>>,
        qr: Vec<String>,
    ) -> Self {
        Self { path, pages, page_pngs, lines_full, qr, error: None }
    }
    pub fn err(path: String, why: String) -> Self {
        Self {
            path,
            pages: vec![],
            page_pngs: vec![],
            lines_full: vec![],
            qr: vec![],
            error: Some(why),
        }
    }
    /// 页 PNG 与给定行按页序配对（`rumi_ocr::dual_pdf::export_dual_layer_pdf`
    /// 的 `pages` 参数形态）。J2：行改为显式传入——导出前 [`Self::lines_full`]
    /// 会先过忽略规则过滤（[`filter_ignored_lines`]），配对直接用过滤后的行。
    pub fn dual_pages(&self, lines: Vec<Vec<OcrLine>>) -> Vec<(Vec<u8>, Vec<OcrLine>)> {
        self.page_pngs.iter().cloned().zip(lines).collect()
    }
    /// 全部文本行（导出用）
    pub fn all_text(&self) -> Vec<&str> {
        self.pages.iter().flat_map(|p| p.iter().map(|l| l.text.as_str())).collect()
    }
}

/// 批量核心：逐文件 装载(png/jpg/webp/pdf) → 识别(引擎分发) → 可选 QR。
/// `progress(done, total)` 在每个文件完成后回调（UI 进度行）。
pub fn render_batch<F: FnMut(usize, usize)>(
    files: &[String],
    with_qr: bool,
    mut progress: F,
) -> Vec<BatchFileOut> {
    let total = files.len();
    let mut out = Vec::with_capacity(total);
    for (i, path) in files.iter().enumerate() {
        let r = process_one(path, with_qr);
        out.push(r);
        progress(i + 1, total);
    }
    out
}

fn process_one(path: &str, with_qr: bool) -> BatchFileOut {
    let p = Path::new(path);
    // K2：load_pdf / load_frame 返回统一图像载体 Vec<image::RgbaImage>
    let frames = if p.extension().map_or(false, |e| e.eq_ignore_ascii_case("pdf")) {
        rumi_ocr::load_pdf(p)
    } else {
        rumi_ocr::load_frame(p).map(|f| vec![f])
    };
    let frames = match frames {
        Ok(f) => f,
        Err(e) => return BatchFileOut::err(path.to_string(), e.to_string()),
    };
    let qr = if with_qr {
        frames.iter().flat_map(rumi_ocr::scan_qr).collect()
    } else {
        vec![]
    };
    // 批量与识别页共用一份排版/忽略选项（开始时取一次快照）
    let opts = OcrOpts::from_settings();
    let mut pages = Vec::with_capacity(frames.len());
    let mut page_pngs = Vec::with_capacity(frames.len());
    let mut lines_full = Vec::with_capacity(frames.len());
    for frame in &frames {
        // 原始识别行只取一次：含 box 的 OcrLine 进 lines_full（双层 PDF 文字层），
        // 展示行 UiLine 由同一份行经同一 postprocess 规则映射（避免双份识别）
        let raw = match recognize_raw(frame) {
            Ok(r) => r,
            Err(e) => return BatchFileOut::err(path.to_string(), e),
        };
        // 帧 + 行 → 导出页数据（PNG 字节，纯函数见 [`export_page`]）
        let (png, raw) = match export_page(frame, raw) {
            Ok(v) => v,
            Err(e) => return BatchFileOut::err(path.to_string(), e),
        };
        pages.push(to_ui_lines(raw.clone(), &opts));
        lines_full.push(raw);
        page_pngs.push(png);
    }
    BatchFileOut::ok(path.to_string(), pages, page_pngs, lines_full, qr)
}

/// 识别一帧为原始 [`OcrLine`]（保留 box，std 线程调用）。
///
/// 与 `ocr_flow::recognize_frame` 的区别：不后处理、不丢 box——批量页要同时
/// 产出展示行（UiLine）与导出行（OcrLine），从原始行各映射一份即可，无需识别两次。
/// - native → `OcrEngine::global().recognize`（模型常驻）；
/// - plugin:<名> → worker runBytes（图像内存编码 PNG）。插件输出无真实坐标，
///   box_ 缺失时填**全零占位框**：双层 PDF 仍可导出，视觉层整页图不受影响，
///   但隐形文字层的定位/字号会失真（导出侧会把全零框行计入 skipped）。
fn recognize_raw(img: &image::RgbaImage) -> Result<Vec<OcrLine>, String> {
    match crate::settings::get().engine_plugin() {
        None => rumi_ocr::OcrEngine::global().recognize(img).map_err(|e| e.to_string()),
        Some(name) => {
            let png = crate::ocr_flow::frame_to_png(img)?;
            Ok(crate::plugins_ui::run_bytes_recognize(name, &png)?
                .into_iter()
                .map(|l| OcrLine {
                    text: l.text,
                    score: l.score.unwrap_or(0.0),
                    r#box: l.box_.unwrap_or([[0.0; 2]; 4]),
                })
                .collect())
        }
    }
}

/// 原始识别行 → 面板展示行：直接复用 `ocr_flow::processed_to_ui`
/// （postprocess 入口 + 组内分数均值 + 定位框并集；此前这里手抄过一份映射，
/// 收敛到单一来源，行为以 ocr_flow 的单测为准）。
fn to_ui_lines(raw: Vec<OcrLine>, opts: &OcrOpts) -> Vec<UiLine> {
    let rules = rumi_ocr::postprocess::PostRules { ignore_regex: opts.ignore.clone() };
    let p = rumi_ocr::postprocess::postprocess(&raw, opts.layout, &rules);
    crate::ocr_flow::processed_to_ui(p, &raw)
}

/// 帧 + 该页原始识别行 → 单页导出数据 (PNG 字节, 行)。
/// 纯函数（仅内存编码不落盘）：process_one 填充 [`BatchFileOut`] 的
/// page_pngs / lines_full 与双层 PDF 导出共用同一形态。
fn export_page(img: &image::RgbaImage, lines: Vec<OcrLine>) -> Result<(Vec<u8>, Vec<OcrLine>), String> {
    Ok((crate::ocr_flow::frame_to_png(img)?, lines))
}

/// 导出文件名：`<原文件名去扩展>.searchable.pdf`（无扩展名时直接拼，异常路径回退 rumi）。
fn searchable_pdf_name(path: &str) -> String {
    let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("rumi");
    format!("{stem}.searchable.pdf")
}

/// 忽略规则 → 已编译正则（坏正则跳过，与 ocr crate PostRules 同口径）。
fn ignore_predicates(rules: &[String]) -> Vec<regex::Regex> {
    rules.iter().filter_map(|r| regex::Regex::new(r.as_str()).ok()).collect()
}

/// 双层 PDF 文字层的忽略规则过滤（修 H 遗留的语义问题）：导出前按
/// `settings.ignore_rules`（每条一个正则，与设置页忽略规则同源）逐行
/// `is_match` 剔除命中行——水印行不再进可搜索层。
///
/// - 只剔行、**不做任何合并**：保留行的 box 原样进隐形文字层，坐标不失真；
/// - 坏正则跳过（与 ocr crate PostRules 同口径，不整体失败）；
/// - 空规则原样返回（零开销路径）。纯函数，单测覆盖命中/未命中混排。
pub fn filter_ignored_lines(
    pages: Vec<Vec<OcrLine>>,
    rules: &[String],
) -> Vec<Vec<OcrLine>> {
    let rules = ignore_predicates(rules);
    if rules.is_empty() {
        return pages;
    }
    pages
        .into_iter()
        .map(|page| {
            page.into_iter()
                .filter(|l| !rules.iter().any(|re| re.is_match(&l.text)))
                .collect()
        })
        .collect()
}

/// 逐文件导出双层可搜索 PDF（std 线程调用，IO/PDF 生成重活不卡 UI）：
/// 识别成功的文件各写 `<stem>.searchable.pdf` 到 `dir`，单文件失败标红（❌）继续，
/// 失败文件（`error` 非空）整体跳过。文字层先过忽略规则
/// （settings.ignore_rules，水印行不进可搜索层；与 [`filter_ignored_lines`]
/// 同一谓词）。返回状态汇总串：
/// "✅ 导出 N 个（写入 X 行/跳过 Y 行）"，其后附逐文件失败原因。
fn export_dual_pdfs(outs: &[BatchFileOut], dir: &Path) -> String {
    let rules = crate::settings::get().ignore_rules;
    let (mut ok, mut written, mut skipped) = (0usize, 0usize, 0usize);
    let mut errs: Vec<String> = Vec::new();
    for o in outs {
        if o.error.is_some() {
            continue; // 失败文件不导出（识别没成功就没有行/PNG 数据）
        }
        // 文字层过滤走同一个纯函数：视觉层（页 PNG）不动，只剔命中行，
        // 保留行 box 不合并不失真（水印行不进可搜索层）
        let lines = filter_ignored_lines(o.lines_full.clone(), &rules);
        let pages = o.dual_pages(lines);
        let out_path = dir.join(searchable_pdf_name(&o.path));
        match rumi_ocr::dual_pdf::export_dual_layer_pdf(&out_path, &pages) {
            Ok(s) => {
                ok += 1;
                written += s.written;
                skipped += s.skipped;
            }
            Err(e) => errs.push(format!("{}: {e}", short_name(&o.path))),
        }
    }
    let mut s = format!("✅ 导出 {ok} 个（写入 {written} 行/跳过 {skipped} 行）");
    for e in &errs {
        s.push_str(&format!("；❌ {e}"));
    }
    s
}

/// 导出 TXT：文件名做小节标题，QR 行加前缀
pub fn export_txt(outs: &[BatchFileOut]) -> String {
    let mut s = String::new();
    for o in outs {
        s.push_str(&format!("==== {} ====\n", short_name(&o.path)));
        for t in o.all_text() {
            s.push_str(t);
            s.push('\n');
        }
        for q in &o.qr {
            s.push_str(&format!("[QR] {q}\n"));
        }
        if let Some(e) = &o.error {
            s.push_str(&format!("[错误] {e}\n"));
        }
        s.push('\n');
    }
    s
}

/// 导出 Markdown：文件名二级标题，行进引用块保持可读
pub fn export_md(outs: &[BatchFileOut]) -> String {
    let mut s = String::from("# Rumi 批量识别结果\n\n");
    for o in outs {
        s.push_str(&format!("## {}\n\n", short_name(&o.path)));
        if let Some(e) = &o.error {
            s.push_str(&format!("> ❌ {e}\n\n"));
            continue;
        }
        for t in o.all_text() {
            s.push_str(&format!("{}  \n", t.trim_end()));
        }
        for q in &o.qr {
            s.push_str(&format!("\n`[QR] {q}`\n"));
        }
        s.push('\n');
    }
    s
}

fn short_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_batch_reports_missing_file_and_continues() {
        // 错误文件不中断批次：[坏, 好(错误), 坏] → 3 个结果，2 个带 error
        let outs = render_batch(
            &["/nonexistent/a.png".into(), "/nonexistent/b.pdf".into()],
            false,
            |_, _| {},
        );
        assert_eq!(outs.len(), 2);
        assert!(outs[0].error.is_some() && outs[1].error.is_some());
        assert!(outs[0].pages.is_empty() && outs[0].qr.is_empty());
    }

    #[test]
    fn exports_contain_headers_lines_and_qr() {
        let line = |t: &str| UiLine { text: t.into(), score: "1.00".into(), box_px: None };
        let outs = vec![
            BatchFileOut::ok(
                "/tmp/dir/alpha.png".into(),
                vec![vec![line("你好"), line("World")]],
                vec![],
                vec![],
                vec!["https://x.example".into()],
            ),
            BatchFileOut::err("/tmp/beta.pdf".into(), "打不开".into()),
        ];
        let txt = export_txt(&outs);
        assert!(txt.contains("==== alpha.png ===="));
        assert!(txt.contains("你好"));
        assert!(txt.contains("[QR] https://x.example"));
        assert!(txt.contains("[错误] 打不开"));

        let md = export_md(&outs);
        assert!(md.contains("## alpha.png"));
        assert!(md.contains("## beta.pdf"));
        assert!(md.contains("❌ 打不开"));
    }

    /// 独立临时目录（进程 id + 原子计数，并行测试安全）
    fn test_dir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("rumi-batch-{tag}-{}-{n}", std::process::id()))
    }

    /// 4x4 纯色 PNG 字节（双层 PDF 视觉层最小合法输入）
    fn tiny_png(shade: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([shade, shade, shade, 255]));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        png
    }

    /// BatchFileOut 填充逻辑（P0 双层 PDF）：`export_page` 把"帧 + 原始行"配成
    /// 导出页数据——PNG 可解码且与帧同尺寸、行按原样保留；多页时
    /// `dual_pages` 按页序把 page_pngs / lines_full 一一配对。
    #[test]
    fn export_page_pairs_png_with_lines_for_dual_pdf() {
        let (w, h) = (7u32, 4u32);
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([9, 9, 9, 255]));
        let line = |t: &str| OcrLine { text: t.into(), score: 0.8, r#box: [[0.0; 2]; 4] };
        let (png, lines) = export_page(&img, vec![line("你好"), line("ok")]).expect("PNG 编码");
        let decoded = image::load_from_memory(&png).expect("PNG 可解码");
        assert_eq!((decoded.width(), decoded.height()), (w, h));
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["你好", "ok"]);

        // 多页填充：两列同序 → dual_pages 逐页配对（页序即 PDF 页序）
        let (png2, _) = export_page(&img, vec![]).expect("第二页编码");
        let out = BatchFileOut::ok(
            "/tmp/x.png".into(),
            vec![vec![], vec![]],
            vec![png, png2],
            vec![vec![line("a")], vec![]],
            vec![],
        );
        let pages = out.dual_pages(out.lines_full.clone());
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].1.len(), 1);
        assert_eq!(pages[0].1[0].text, "a");
        assert!(pages[1].1.is_empty());
        let page2 = image::load_from_memory(&pages[1].0).unwrap();
        assert_eq!((page2.width(), page2.height()), (w, h));
    }

    /// 导出文件名：原文件名去扩展 + `.searchable.pdf`
    #[test]
    fn searchable_pdf_name_strips_extension() {
        assert_eq!(searchable_pdf_name("/tmp/dir/扫描件.pdf"), "扫描件.searchable.pdf");
        assert_eq!(searchable_pdf_name("a/b/c.PNG"), "c.searchable.pdf");
        assert_eq!(searchable_pdf_name("noext"), "noext.searchable.pdf");
    }

    /// 双层 PDF 批量导出：成功文件落盘、坏 PNG 单文件失败标红继续、
    /// error 文件整体跳过；汇总串报告 N/X/Y。
    #[test]
    fn export_dual_pdfs_skips_failures_and_summarizes() {
        let dir = test_dir("dual");
        std::fs::create_dir_all(&dir).unwrap();
        let line = || OcrLine { text: String::new(), score: 1.0, r#box: [[0.0; 2]; 4] };
        let outs = vec![
            BatchFileOut::ok(
                "/d/甲.png".into(),
                vec![vec![]],
                vec![tiny_png(200)],
                vec![vec![line()]],
                vec![],
            ),
            // 识别成功但视觉层字节坏了 → 单文件失败不中断
            BatchFileOut::ok(
                "/d/乙.png".into(),
                vec![vec![]],
                vec![b"not a png".to_vec()],
                vec![vec![line()]],
                vec![],
            ),
            // 识别失败文件 → 整体跳过
            BatchFileOut::err("/d/丙.png".into(), "打不开".into()),
        ];
        let summary = export_dual_pdfs(&outs, &dir);
        assert!(summary.contains("导出 1 个（写入 0 行/跳过 0 行）"), "{summary}");
        assert!(summary.contains("❌ 乙.png"), "失败文件要标红出现在汇总：{summary}");
        assert!(!summary.contains("丙"), "识别失败的文件不参与导出：{summary}");
        assert!(dir.join("甲.searchable.pdf").is_file());
        assert!(!dir.join("乙.searchable.pdf").exists());
        assert!(!dir.join("丙.searchable.pdf").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// J2 #2：递归收集纯函数——嵌套目录深搜、扩展名过滤（大小写不敏感）、
    /// 隐藏目录/隐藏文件跳过、符号链接不跟（文件软链不收、目录软链不进）、
    /// 非 png/jpg 等扩展不收；上限 FOLDER_SCAN_CAP 截断并返回 true。
    #[test]
    fn collect_files_recursive_walks_filters_and_caps() {
        use std::fs;
        let root = test_dir("folder-scan");
        let nested = root.join("子目录").join("更深");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(root.join(".hidden_dir")).unwrap();
        // 命中：根、子目录、更深 各一层 + 大写扩展
        fs::write(root.join("a.png"), b"p").unwrap();
        fs::write(root.join("b.txt"), b"not-ocr").unwrap(); // 扩展不收
        fs::write(nested.join("c.JPG"), b"p").unwrap(); // 大写扩展也收
        fs::write(nested.join("d.jpeg"), b"p").unwrap();
        fs::write(nested.join("e.webp"), b"p").unwrap();
        fs::write(nested.join("f.pdf"), b"p").unwrap();
        fs::write(nested.join("g"), b"noext").unwrap(); // 无扩展不收
        fs::write(root.join(".hidden_dir").join("h.png"), b"p").unwrap(); // 隐藏目录不进
        fs::write(root.join(".dotfile.png"), b"p").unwrap(); // 隐藏文件不收
        fs::write(root.join("hidden-link.png"), b"p").unwrap(); // 给软链用

        #[cfg(unix)]
        {
            // 文件软链：指向一张图片 → 不收
            std::os::unix::fs::symlink(root.join("a.png"), root.join("link-to-file.png"))
                .unwrap();
            // 目录软链：指向子目录 → 不递归（其中 c/d/e/f 都不该经此重复出现）
            std::os::unix::fs::symlink(&nested, root.join("link-to-dir")).unwrap();
        }

        let (files, truncated) = collect_files_recursive(&root);
        assert!(!truncated, "未达上限不算截断");
        let names: Vec<String> = files
            .iter()
            .map(|p| Path::new(p).file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), files.len(), "文件名不重复");
        assert!(names.contains(&"a.png".to_string()), "{names:?}");
        assert!(names.contains(&"c.JPG".to_string()));
        assert!(names.contains(&"d.jpeg".to_string()));
        assert!(names.contains(&"e.webp".to_string()));
        assert!(names.contains(&"f.pdf".to_string()));
        assert!(!names.contains(&"b.txt".to_string()), "非图片/PDF 扩展不收：{names:?}");
        assert!(!names.contains(&"g".to_string()));
        assert!(!names.contains(&"h.png".to_string()), "隐藏目录不进：{names:?}");
        assert!(!names.contains(&".dotfile.png".to_string()), "隐藏文件不收：{names:?}");
        assert!(!names.contains(&"link-to-file.png".to_string()), "软链文件不收：{names:?}");
        // 目录软链不递归：c/d 经真实子目录各只出现一次，且无软链路径泄漏
        assert_eq!(
            files.iter().filter(|p| p.contains("更深")).count(),
            4,
            "软链目录不产生重复/额外路径：{files:?}"
        );
        // 所有路径都以 root 开头（目录软链没把树外路径带进来）
        assert!(files.iter().all(|p| p.starts_with(root.to_string_lossy().as_ref())));

        // 上限截断：只留 2 个命中文件 + 追加 FOLDER_SCAN_CAP 个 png → 截断
        let capped = test_dir("folder-cap");
        fs::create_dir_all(&capped).unwrap();
        fs::write(capped.join("keep1.pdf"), b"p").unwrap();
        fs::write(capped.join("keep2.pdf"), b"p").unwrap();
        for i in 0..FOLDER_SCAN_CAP {
            fs::write(capped.join(format!("{i:04}.png")), b"p").unwrap();
        }
        fs::write(capped.join("overflow.pdf"), b"p").unwrap(); // 第 CAP+1 个 → 截断
        let (files2, truncated2) = collect_files_recursive(&capped);
        assert!(truncated2, "超上限要报截断");
        assert_eq!(files2.len(), FOLDER_SCAN_CAP, "结果数恰为上限");

        // 目录不存在 / 空目录：空列表不报错
        let (empty, t3) = collect_files_recursive(&test_dir("folder-absent"));
        assert!(empty.is_empty() && !t3);

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&capped).ok();
    }

    /// J2 #1：双层 PDF 文字层的忽略规则过滤——命中行整行剔除（box 一起走），
    /// 未命中行原样保留（box 不合并不失真）；多页各自过滤；坏正则跳过；
    /// 空规则零拷贝返回。
    #[test]
    fn filter_ignored_lines_drops_matched_keeps_boxes() {
        let line = |t: &str, y: f32| OcrLine {
            text: t.into(),
            score: 0.9,
            r#box: [[0.0, y], [100.0, y], [100.0, y + 20.0], [0.0, y + 20.0]],
        };
        let pages = vec![
            vec![
                line("正文第一行", 0.0),
                line("内部资料 严禁传播", 30.0), // 水印命中
                line("正文第二行", 60.0),
            ],
            vec![line("https://ads.example.com", 0.0), line("第二页正文", 40.0)],
        ];
        let rules = vec!["^内部资料".to_string(), "https?://".to_string(), "([unclosed".to_string()];
        let out = filter_ignored_lines(pages, &rules);
        assert_eq!(out.len(), 2, "页数不变");
        let texts0: Vec<&str> = out[0].iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts0, vec!["正文第一行", "正文第二行"]);
        let texts1: Vec<&str> = out[1].iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts1, vec!["第二页正文"]);
        // 未命中行 box 原样保留（剔行不合并，坐标不失真）
        assert_eq!(out[0][1].r#box, [[0.0, 60.0], [100.0, 60.0], [100.0, 80.0], [0.0, 80.0]]);
        // 空规则 → 原样返回
        let pages2 = vec![vec![line("x", 0.0)]];
        assert_eq!(filter_ignored_lines(pages2.clone(), &[]), pages2);
    }
}

/// 批量页可选扩展名（小写；与 pick_files 的过滤器同口径）
const BATCH_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "pdf"];

/// 递归收集文件数上限：超限截断并在状态行提示（防止选到巨型目录卡死）
pub const FOLDER_SCAN_CAP: usize = 500;

/// 递归收集文件夹下的图片/PDF（J2 #2，纯函数——测试注入 tmp 目录树）。
///
/// - 仅收 [`BATCH_EXTS`] 扩展名（大小写不敏感）；
/// - 符号链接一律不跟（文件软链不收、目录软链不递归，防环/防越界）；
/// - 隐藏目录/隐藏文件（`.` 开头）跳过；
/// - 命中 [`FOLDER_SCAN_CAP`] 即停（截断），返回 `(文件列表, 是否截断)`；
/// - 目录内按文件名排序后遍历（结果确定性），返回绝对路径串。
pub fn collect_files_recursive(root: &Path) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let truncated = walk_dir(root, &mut out);
    (out, truncated)
}

/// [`collect_files_recursive`] 的递归体：返回 true = 已到上限（逐层短路返回）。
fn walk_dir(dir: &Path, out: &mut Vec<String>) -> bool {
    if out.len() >= FOLDER_SCAN_CAP {
        return true;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false; // 打不开的目录（权限）跳过，不整体失败
    };
    let mut entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue; // 隐藏目录/隐藏文件
        }
        // file_type() 不解引用：symlink 在此被识别（is_dir/is_file 均为 false 之外的判断）
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue; // 软链不跟：文件软链不收，目录软链不递归
        }
        if ft.is_dir() {
            if walk_dir(&e.path(), out) {
                return true;
            }
        } else if ft.is_file() {
            let matched = e
                .path()
                .extension()
                .map_or(false, |ext| BATCH_EXTS.iter().any(|x| ext.eq_ignore_ascii_case(x)));
            if matched {
                if out.len() >= FOLDER_SCAN_CAP {
                    return true; // 第 FOLDER_SCAN_CAP+1 个被拒 → 真截断
                }
                out.push(e.path().display().to_string());
            }
        }
    }
    false
}

fn timestamp() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{t}")
}

/// rfd 同步对话框在 Linux GTK 后端会死锁主线程，xdg-portal 后端线程安全；
/// 统一经 run_blocking 放 std 线程，两种后端都成立。
fn pick_files() -> Option<Vec<String>> {
    rfd::FileDialog::new()
        .add_filter("图片 / PDF", &["png", "jpg", "jpeg", "webp", "pdf"])
        .pick_files()
        .map(|fs| fs.into_iter().map(|f| f.display().to_string()).collect())
}

fn save_as(default: &str) -> Option<String> {
    rfd::FileDialog::new().set_file_name(default).save_file().map(|f| f.display().to_string())
}

/// 双层 PDF 导出目录（rfd 同步对话框同样经 run_blocking 放线程，见 [`pick_files`]）
fn pick_export_dir() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}

/// 选择文件（spawn 线程跑 rfd）→ 覆盖文件列表。空状态占位与工具行两个入口共用。
fn pick_into(mut files: Signal<Vec<String>>, mut status: Signal<String>) {
    spawn(async move {
        let rx = crate::ocr_flow::run_blocking(pick_files);
        if let Ok(Some(fs)) = rx.await {
            files.set(fs);
            // 状态消息在 spawn 闭包里产生：i18n::tt 读全局原子语言（i18n.rs 注释）
            status.set(i18n::tt(i18n::k::BATCH_STATUS_SELECTED).to_string());
        }
    });
}

/// 选择文件夹（J2 #2，spawn 线程跑 rfd pick_folder + 递归收集）→
/// **追加**进文件列表（与"选择文件"的多选结果混排，不排序不去重）。
/// 超 [`FOLDER_SCAN_CAP`] 截断时状态行提示。
fn pick_folder_into(mut files: Signal<Vec<String>>, mut status: Signal<String>) {
    spawn(async move {
        let rx = crate::ocr_flow::run_blocking(|| {
            rfd::FileDialog::new()
                .pick_folder()
                .map(|dir| collect_files_recursive(&dir))
        });
        if let Ok(Some((fs, truncated))) = rx.await {
            let mut all = files();
            all.extend(fs);
            files.set(all);
            if truncated {
                status.set(i18n::filln(
                    i18n::tt(i18n::k::BATCH_FOLDER_TRUNCATED),
                    &[&FOLDER_SCAN_CAP],
                ));
            } else {
                status.set(i18n::tt(i18n::k::BATCH_FOLDER_SELECTED).to_string());
            }
        }
    });
}

/// 批量页组件。`busy` 与识别页共享：批量进行中禁用两边的触发。
#[component]
pub fn BatchPage(mut busy: Signal<bool>, lang: Signal<String>) -> Element {
    let lang_v = lang();
    let t = |k: &'static str| i18n::ts(&lang_v, k);
    let mut files = use_signal(Vec::<String>::new);
    let mut results = use_signal(|| Option::<Vec<BatchFileOut>>::None);
    let mut status = use_signal(|| String::new());
    let mut with_qr = use_signal(|| false);
    // P2 #13 二维码工具卡
    let mut qr_open = use_signal(|| false);
    let mut qr_text = use_signal(String::new);
    let mut qr_scale = use_signal(|| "8".to_string());
    let mut qr_png = use_signal(|| Option::<String>::None);
    let busy_v = busy();
    let files_empty = files().is_empty();
    let results_some = results().is_some();
    // 双层 PDF 导出可用性：至少一个文件识别成功，且不在进行中
    let has_ok = results().is_some_and(|rs| rs.iter().any(|o| o.error.is_none()));
    let qr_open_v = qr_open();
    let qr_scale_v = qr_scale();
    let qr_text_v = qr_text();
    // 进行中按钮文字反馈（P0 #5）
    let btn_start = if busy_v { "识别中…".to_string() } else { t(i18n::k::BATCH_START).to_string() };

    rsx! {
        div { class: "page",
            div { class: "page-head",
                span { class: "page-title", "{t(i18n::k::PAGE_BATCH)}" }
            }

            // O 波 #5：工具行分两组——左侧按钮组（选择/开始/导出），右侧选项组
            // （顺带扫二维码 + 二维码工具卡折叠入口），space-between 对齐，
            // 按钮与 checkbox 不再挤作一团（checkbox 对齐统一走 .chk 组合）
            div { class: "row", style: "justify-content: space-between; align-items: center;",
                div { class: "row",
                    button {
                        disabled: busy_v,
                        onclick: move |_| pick_into(files, status),
                        "{t(i18n::k::BATCH_PICK)}"
                    }
                    // J2 #2：文件夹递归收集（png/jpg/jpeg/webp/pdf；软链不跟、
                    // 隐藏目录跳过、上限 FOLDER_SCAN_CAP 截断并提示）
                    button {
                        disabled: busy_v,
                        onclick: move |_| pick_folder_into(files, status),
                        "{t(i18n::k::BATCH_PICK_FOLDER)}"
                    }
                    button { class: "primary",
                        disabled: busy_v || files_empty,
                        onclick: move |_| {
                            if busy() { return; }
                            busy.set(true);
                            results.set(None);
                            status.set(i18n::tt(i18n::k::BATCH_STATUS_RUNNING).to_string());
                            let fs: Vec<String> = files();
                            let qr = with_qr();
                            let mut st = status;
                            spawn(async move {
                                let rx = crate::ocr_flow::run_blocking(move || {
                                    render_batch(&fs, qr, |done, total| {
                                        // 进度仅作线程侧观测钩子；UI 进度由 status 汇总
                                        let _ = (done, total);
                                    })
                                });
                                match rx.await {
                                    Ok(outs) => {
                                        let done = outs.iter().filter(|o| o.error.is_none()).count();
                                        st.set(i18n::filln(
                                            i18n::tt(i18n::k::BATCH_STATUS_DONE),
                                            &[&done, &outs.len()],
                                        ));
                                        results.set(Some(outs));
                                    }
                                    Err(_) => st.set(i18n::tt(i18n::k::BATCH_THREAD_LOST).to_string()),
                                }
                                busy.set(false);
                            });
                        },
                        "{btn_start}"
                    }
                    if results_some {
                        button {
                            onclick: move |_| {
                                if let Some(outs) = results() {
                                    let text = export_txt(&outs);
                                    let default = format!("rumi_batch_{}.txt", timestamp());
                                    let mut st = status;
                                    spawn(async move {
                                        let rx = crate::ocr_flow::run_blocking(move || save_as(&default));
                                        if let Ok(Some(path)) = rx.await {
                                            match std::fs::write(&path, text) {
                                                Ok(()) => st.set(i18n::fill(i18n::tt(i18n::k::EXPORT_OK), path)),
                                                Err(e) => st.set(i18n::fill(i18n::tt(i18n::k::EXPORT_FAIL), e)),
                                            }
                                        }
                                    });
                                }
                            },
                            "{t(i18n::k::BATCH_EXPORT_TXT)}"
                        }
                        button {
                            onclick: move |_| {
                                if let Some(outs) = results() {
                                    let text = export_md(&outs);
                                    let default = format!("rumi_batch_{}.md", timestamp());
                                    let mut st = status;
                                    spawn(async move {
                                        let rx = crate::ocr_flow::run_blocking(move || save_as(&default));
                                        if let Ok(Some(path)) = rx.await {
                                            match std::fs::write(&path, text) {
                                                Ok(()) => st.set(i18n::fill(i18n::tt(i18n::k::EXPORT_OK), path)),
                                                Err(e) => st.set(i18n::fill(i18n::tt(i18n::k::EXPORT_FAIL), e)),
                                            }
                                        }
                                    });
                                }
                            },
                            "{t(i18n::k::BATCH_EXPORT_MD)}"
                        }
                    }
                    // P0：双层可搜索 PDF 导出（视觉层整页图 + 隐形文字层）。
                    // 无成功结果或批量进行中时禁用；rfd 选目录与 PDF 生成都经 run_blocking。
                    // 设置里是插件引擎时同样可导出：插件无坐标 → 全零占位框，
                    // 隐形文字层定位失真（见 process_one / recognize_raw 注释）。
                    button {
                        class: "primary",
                        disabled: busy_v || !has_ok,
                        onclick: move |_| {
                            if busy() { return; }
                            let Some(outs) = results() else { return };
                            let outs: Vec<BatchFileOut> =
                                outs.into_iter().filter(|o| o.error.is_none()).collect();
                            if outs.is_empty() { return; }
                            let mut st = status;
                            spawn(async move {
                                let dir = crate::ocr_flow::run_blocking(pick_export_dir).await;
                                if let Ok(Some(dir)) = dir {
                                    let rx = crate::ocr_flow::run_blocking(move || {
                                        export_dual_pdfs(&outs, &dir)
                                    });
                                    match rx.await {
                                        Ok(summary) => st.set(summary),
                                        Err(_) => st.set("❌ 导出线程意外退出".to_string()),
                                    }
                                }
                            });
                        },
                        "导出双层 PDF"
                    }
                }
                // 右侧选项组：顺带扫二维码（.chk 组合统一 checkbox 对齐，O 波 #3）
                div { class: "row",
                    label { class: "chip chk", style: "cursor: pointer; gap: 6px;",
                        input {
                            r#type: "checkbox",
                            checked: with_qr(),
                            onchange: move |e| with_qr.set(e.checked()),
                        }
                        "{t(i18n::k::BATCH_WITH_QR)}"
                    }
                    // P2 #13：折叠展开按钮（与二维码开关同组靠右）
                    button { class: "btn-sm",
                        onclick: move |_| qr_open.toggle(),
                        if qr_open_v { "▾ " } else { "▸ " }
                        "{t(i18n::k::BATCH_QR_TITLE)}"
                    }
                }
            }

            div { class: "status", "{status}" }

            if files_empty {
                // 空状态占位（P0 #5）：居中虚线框 + 引导
                div { class: "empty-state",
                    crate::NavIcon { name: crate::NavIconName::Batch, size: 34 }
                    div { class: "empty-title", "{t(i18n::k::BATCH_EMPTY_TITLE)}" }
                    div { class: "empty-sub", "{t(i18n::k::BATCH_EMPTY_SUB)}" }
                    button { class: "primary", onclick: move |_| pick_into(files, status),
                        "{t(i18n::k::BATCH_PICK)}" }
                }
            } else {
                div { class: "file-list",
                    for (i, f) in files().iter().enumerate() {
                        div { class: "file-row",
                            span { "{f}" }
                            button { class: "danger btn-sm",
                                onclick: move |_| {
                                    let mut v = files();
                                    v.remove(i);
                                    files.set(v);
                                },
                                "{t(i18n::k::BATCH_REMOVE)}" }
                        }
                    }
                }
            }

            if let Some(outs) = results() {
                for o in &outs {
                    div { class: "batch-out",
                        if let Some(e) = &o.error {
                            span { class: "err", "❌ {e}" }
                        } else {
                            for t in o.all_text() {
                                div { "{t}" }
                            }
                            for q in &o.qr {
                                div { span { class: "qr-tag", "[QR]" } " {q}" }
                            }
                        }
                    }
                }
            }

            // ── P2 #13：生成二维码折叠卡 ────────────────────────────
            if qr_open_v {
                div { class: "card", style: "margin-top: var(--sp2);",
                    div { class: "schema-head", "{t(i18n::k::BATCH_QR_TITLE)}" }
                    div { class: "formrow",
                        label { "{t(i18n::k::BATCH_QR_TEXT)}" }
                        div { class: "field",
                            textarea {
                                class: "line-edit",
                                rows: "3",
                                placeholder: "{t(i18n::k::BATCH_QR_PLACEHOLDER)}",
                                value: "{qr_text_v}",
                                oninput: move |e| qr_text.set(e.value()),
                            }
                        }
                    }
                    div { class: "formrow",
                        label { title: "每个模块的像素边长（1-32）", "{t(i18n::k::BATCH_QR_SCALE)}" }
                        div { class: "field",
                            input {
                                r#type: "number", min: "1", max: "32",
                                value: "{qr_scale_v}",
                                oninput: move |e| qr_scale.set(e.value()),
                            }
                        }
                    }
                    div { class: "row",
                        button { class: "primary",
                            disabled: busy_v,
                            onclick: move |_| {
                                let text = qr_text().clone();
                                if text.is_empty() {
                                    status.set(i18n::tt(i18n::k::QR_ERR_EMPTY).to_string());
                                    return;
                                }
                                let Ok(scale) = qr_scale().trim().parse::<u8>() else {
                                    status.set(i18n::tt(i18n::k::QR_ERR_SCALE_PARSE).to_string());
                                    return;
                                };
                                if scale == 0 || scale > 32 {
                                    status.set(i18n::tt(i18n::k::QR_ERR_SCALE_RANGE).to_string());
                                    return;
                                }
                                let mut st = status;
                                spawn(async move {
                                    let rx = crate::ocr_flow::run_blocking(move || {
                                        rumi_ocr::qr::make_qr_png(&text, scale)
                                    });
                                    match rx.await {
                                        Ok(Ok(png)) => {
                                            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                                            qr_png.set(Some(format!("data:image/png;base64,{b64}")));
                                            st.set(i18n::tt(i18n::k::QR_GEN_OK).to_string());
                                        }
                                        Ok(Err(e)) => st.set(i18n::fill(i18n::tt(i18n::k::QR_GEN_FAIL), e)),
                                        Err(_) => st.set(i18n::tt(i18n::k::QR_THREAD_LOST).to_string()),
                                    }
                                });
                            },
                            "{t(i18n::k::BATCH_QR_GENERATE)}"
                        }
                        if qr_png().is_some() {
                            button {
                                onclick: move |_| {
                                    let default = format!("rumi_qr_{}.png", timestamp());
                                    let mut st = status;
                                    spawn(async move {
                                        let rx = crate::ocr_flow::run_blocking(move || save_as(&default));
                                        if let Ok(Some(path)) = rx.await {
                                            match qr_png() {
                                                Some(url) => {
                                                    // data-url → 字节（本卡片内的 png 一定是 base64 png）
                                                    match data_url_bytes(&url) {
                                                        Some(bytes) => match std::fs::write(&path, bytes) {
                                                            Ok(()) => st.set(i18n::fill(i18n::tt(i18n::k::QR_SAVE_OK), path)),
                                                            Err(e) => st.set(i18n::fill(i18n::tt(i18n::k::QR_SAVE_FAIL), e)),
                                                        },
                                                        None => st.set(i18n::tt(i18n::k::QR_SAVE_BAD).to_string()),
                                                    }
                                                }
                                                None => st.set(i18n::tt(i18n::k::QR_SAVE_STALE).to_string()),
                                            }
                                        }
                                    });
                                },
                                "{t(i18n::k::BATCH_QR_SAVE)}"
                            }
                        }
                    }
                    if let Some(url) = qr_png() {
                        div { style: "margin-top: var(--sp1);",
                            img { src: "{url}", style: "width: 220px; height: auto; border: 1px solid var(--border); border-radius: var(--radius-s); background: var(--img-bg);" }
                        }
                    }
                }
            }
        }
    }
}

/// `data:image/png;base64,…` → 原始字节（仅处理本组件自己生成的 data-url）。
fn data_url_bytes(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("data:image/png;base64,")?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(rest).ok()
}
