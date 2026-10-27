//! 阶段一探针：pdf-inspector 中文文字层提取质量验证（快路径集成门槛）。
//!
//! 步骤：
//! 1. 生成含中文文字层的样例 PDF（两页，每页 2-3 行中文 + 1 行英文数字；
//!    生成逻辑在 `rumi_ocr::pdf_route::sample`，含 .ttc 抽面，输出 target/pdf-probe/sample.pdf）；
//! 2. pdf-inspector 分类（PdfType / 置信度 / 需 OCR 页）；
//! 3. pdf-inspector 提取文字 + 坐标，与嵌入原文逐行比对，输出整体相似度。
//!
//! 退出码：0 = 相似度 ≥98%（过关）；1 = 不过关；2 = 样例生成失败（无法进入比对）。
//!
//! 运行：cargo run -p rumi-ocr --example pdf_text_probe

use rumi_ocr::pdf_route::sample;

fn main() {
    let dir = std::path::Path::new("target/pdf-probe");
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("无法创建输出目录 {dir:?}：{e}");
        std::process::exit(2);
    }
    let sample_pdf = dir.join("sample.pdf");

    // ---- 步骤 1：生成样例 ----
    let font = match sample::generate_text_pdf(&sample_pdf) {
        Ok(font) => {
            println!("[生成] {}（嵌入字体：{font}）", sample_pdf.display());
            font
        }
        Err(e) => {
            eprintln!("[生成失败] {e}");
            std::process::exit(2);
        }
    };

    // ---- 步骤 2：pdf-inspector 分类（路由骨架依赖的 API 通路）----
    match pdf_inspector::detect_pdf_type(&sample_pdf) {
        Ok(info) => {
            println!(
                "[分类] {:?}  置信度 {:.2}  页数 {}  有文字算子页 {} / 采样 {}",
                info.pdf_type, info.confidence, info.page_count, info.pages_with_text, info.pages_sampled
            );
            println!("[分类] 需 OCR 页：{:?}", info.pages_needing_ocr);
        }
        Err(e) => {
            eprintln!("[分类失败] {e}");
            std::process::exit(1);
        }
    }

    // ---- 步骤 3：提取 + 逐行比对 ----
    let expected_pages = sample::SAMPLE_PAGES;
    let expected_lines: Vec<String> =
        expected_pages.iter().flat_map(|p| p.iter().map(|s| s.to_string())).collect();
    let extracted = match pdf_inspector::extract_text_with_positions(&sample_pdf) {
        Ok(items) => {
            let lines = pdf_inspector::extractor::group_into_lines_preserving_all_text(items);
            lines
                .iter()
                .map(|l| {
                    let first = l.items.first();
                    let (x, y, w, h) = first
                        .map(|i| (i.x, i.y, i.width, i.height))
                        .unwrap_or((0.0, 0.0, 0.0, 0.0));
                    println!(
                        "[提取] 第{}页 y={y:.1} x={x:.1} w={w:.1} h={h:.1} | {}",
                        l.page,
                        l.text()
                    );
                    l.text()
                })
                .collect::<Vec<_>>()
        }
        Err(e) => {
            eprintln!("[提取失败] {e}");
            std::process::exit(1);
        }
    };

    // 整体相似度：期望全文 vs 提取全文 的归一化编辑距离相似度
    let expect_all: String = expected_lines.join("\n");
    let got_all: String = extracted.join("\n");
    let sim = similarity(&expect_all, &got_all);
    println!(
        "\n[比对] 期望 {} 字符，提取 {} 字符",
        expect_all.chars().count(),
        got_all.chars().count()
    );
    println!("[比对] 整体相似度 {sim:.4}（门槛 0.98）");

    // 逐行找最接近的提取行，展示差异
    for (pi, page) in expected_pages.iter().enumerate() {
        for line in *page {
            let best = extracted
                .iter()
                .map(|e| (similarity(line, e), e))
                .max_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((s, e)) = best {
                let mark = if s >= 0.999 { "==" } else { "≈" };
                println!("[行比] 第{}页 {mark}{s:.3}  期望:{line:?}  最近:{e:?}", pi + 1);
            }
        }
    }

    if sim >= 0.98 {
        println!("\n[结论] 过关：相似度 ≥98%，快路径可集成（嵌入字体 {font}）");
    } else {
        println!("\n[结论] 不过关：相似度 <98%，不集成默认路由");
        std::process::exit(1);
    }

    // ---- 附加：混合样例的分类与逐页路由验证 ----
    let mixed = dir.join("mixed.pdf");
    match sample::generate_mixed_pdf(&mixed) {
        Ok(_) => match pdf_inspector::detect_pdf_type(&mixed) {
            Ok(info) => println!(
                "\n[混合样例] {mixed:?} → {:?}（置信度 {:.2}），需 OCR 页 {:?}",
                info.pdf_type, info.confidence, info.pages_needing_ocr
            ),
            Err(e) => eprintln!("[混合样例分类失败] {e}"),
        },
        Err(e) => eprintln!("[混合样例生成失败] {e}"),
    }
}

/// 归一化编辑距离相似度（1 - lev/max_len），按 Unicode 标量计数。
fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let max = a.len().max(b.len());
    if max == 0 {
        return 1.0;
    }
    // 常规 Levenshtein（滚动数组）
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - (prev[b.len()] as f64) / (max as f64)
}
