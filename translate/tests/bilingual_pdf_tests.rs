//! 双语对照 PDF 出口集成测试（不打网不跑真模型）：走公开入口
//! translate_document（--doc-out x.pdf 的 docmt 分派臂），from==to 直通
//! 路径零引擎零网络。排版纯函数（节标题/引用前缀/换行）的单测在
//! bilingual_pdf.rs 模块内——export_bilingual_pdf 是 pub(crate)，集成
//! 测试不可见。真模型 E2E 为 #[ignore]（en_zh 已装时才跑）。

use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use rumi_translate::docmt::{translate_document, BiStrategy, DocTranslateOptions};
use rumi_translate::Engine;

/// from==to 直通选项（facade 短路原样返回，引擎与缓存都不触达——
/// use_cache: false 与既有测试同口径，不碰用户真实缓存目录）。
fn opts(from: &str, to: &str) -> DocTranslateOptions {
    DocTranslateOptions {
        engine: Engine::Offline,
        from: from.to_string(),
        to: to.to_string(),
        strategy: BiStrategy::Inline,
        polish: false,
        min_chars: 2,
        on_progress: None,
        use_cache: false,
    }
}

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rumi-bilingual-pdf-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录失败");
    d
}

/// 二进制断言三件套：文件存在、非空、%PDF 头（规格写死的最低验收线）。
fn assert_pdf_header(path: &Path) {
    assert!(path.is_file(), "输出文件缺失：{}", path.display());
    let size = path.metadata().expect("读元数据失败").len();
    assert!(size > 0, "输出为空：{}", path.display());
    let mut buf = std::fs::File::open(path).expect("打开输出失败");
    let mut head = [0u8; 5];
    buf.read_exact(&mut head).expect("读头部失败");
    assert_eq!(&head, b"%PDF-", "缺 %PDF 头：{:?}", head);
}

/// pdftotext 是否可用（未装则相关内容断言跳过）。
fn pdftotext_installed() -> bool {
    Command::new("pdftotext")
        .arg("-v")
        .output()
        .map(|o| o.status.success() || !String::from_utf8_lossy(&o.stderr).is_empty())
        .unwrap_or(false)
}

fn run_pdftotext(pdf: &Path) -> String {
    let out = Command::new("pdftotext")
        .arg(pdf)
        .arg("-")
        .output()
        .expect("pdftotext 运行失败");
    assert!(
        out.status.success(),
        "pdftotext 失败：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 去掉全部空白后是否包含（pdftotext 可能改变词间距/换行位置）。
fn nospace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// en→en 直通（无网无模型）：txt → .pdf 真生成，units 全量写入。
#[test]
fn txt_to_pdf_passthrough_writes_real_pdf() {
    let dir = tmp_dir("passthrough");
    let input = dir.join("in.txt");
    std::fs::write(
        &input,
        "The quick brown fox jumps over the lazy dog.\n\n\
         Second paragraph goes into the bilingual PDF export.\n",
    )
    .unwrap();
    let out = dir.join("out.pdf");
    let stats = translate_document(&input, &out, &opts("en", "en")).expect("导出失败");
    assert!(stats.units >= 2, "两段 txt 应得两个单元：{}", stats.units);
    assert!(stats.note.is_none(), "pdf 出口不应携带退化说明：{:?}", stats.note);
    assert_pdf_header(&out);
    let _ = std::fs::remove_dir_all(&dir);
}

/// zh→zh 直通：CJK 原文走 Type0 嵌入字体路径（本机有 Noto CJK 时真嵌入；
/// 无字体候选的环境退化为 Helvetica 跳过 CJK 行——文件仍须生成）。
#[test]
fn zh_txt_to_pdf_via_type0_or_degrades() {
    let dir = tmp_dir("zh");
    let input = dir.join("in.txt");
    std::fs::write(&input, "山有木兮木有枝。\n\n心悦君兮君不知。\n").unwrap();
    let out = dir.join("zh.pdf");
    translate_document(&input, &out, &opts("zh", "zh")).expect("导出失败");
    assert_pdf_header(&out);
    // 嵌入 Noto（~19MB face）的产物应显著大于纯结构 PDF
    let size = out.metadata().unwrap().len();
    eprintln!("zh.pdf 大小：{size} 字节");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 任何输入格式可出 .pdf（设计决策 1）：md 输入不触发同格式校验。
#[test]
fn md_input_exports_pdf_without_same_input_check() {
    let dir = tmp_dir("cross");
    let input = dir.join("in.md");
    std::fs::write(&input, "# Heading One\n\nBody paragraph for cross-format export.\n").unwrap();
    let out = dir.join("out.pdf");
    let stats = translate_document(&input, &out, &opts("en", "en")).expect("跨格式导出失败");
    assert!(stats.units >= 2, "标题 + 段落应得两个单元：{}", stats.units);
    assert_pdf_header(&out);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 写入内容可提取回（pdftotext 已装时）：直通原文 + "> " 引用前缀 + 节标题。
#[test]
fn pdf_text_is_extractable_when_pdftotext_installed() {
    if !pdftotext_installed() {
        eprintln!("跳过：未安装 pdftotext");
        return;
    }
    let dir = tmp_dir("extract");
    let input = dir.join("in.txt");
    std::fs::write(&input, "The quick brown fox jumps over the lazy dog.\n").unwrap();
    let out = dir.join("out.pdf");
    translate_document(&input, &out, &opts("en", "en")).expect("导出失败");
    let text = nospace(&run_pdftotext(&out));
    assert!(text.contains("Thequickbrownfox"), "原文未提取回：{text}");
    // 原文引用块的前缀与节标题（无名页 → 页序 "0"）都应可提取
    assert!(text.contains(">Thequick"), "缺引用前缀：{text}");
    assert!(text.contains("第0节"), "缺节标题：{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// #[ignore] 端到端：en→zh 离线模型已装时 txt → .pdf 全链路（真引擎翻译），
/// pdftotext 已装时进一步断言译文可提取回。
/// 手动跑：cargo test -p rumi-translate --test bilingual_pdf_tests -- --ignored
#[test]
#[ignore]
fn e2e_en_zh_txt_to_pdf_with_offline_engine() {
    let model = rumi_translate::models_dir().join("en_zh").join("model.bin");
    if !model.is_file() {
        eprintln!("跳过：en_zh 语言包未安装（{}）", model.display());
        return;
    }
    let dir = tmp_dir("e2e");
    let input = dir.join("in.txt");
    std::fs::write(&input, "The quick brown fox jumps over the lazy dog.\n\nHello world.\n").unwrap();
    let out = dir.join("e2e.pdf");
    let mut o = opts("en", "zh");
    o.use_cache = false;
    let stats = translate_document(&input, &out, &o).expect("端到端导出失败");
    assert_eq!(stats.units, 2);
    assert_pdf_header(&out);
    eprintln!("e2e.pdf 大小：{} 字节，chars={}", out.metadata().unwrap().len(), stats.chars);
    if pdftotext_installed() {
        let text = nospace(&run_pdftotext(&out));
        // txt 是单页文档：两段同源页 → 只有一个节标题（页名 "0"）
        assert!(text.contains("第0节"), "缺源页节标题：{text}");
        assert!(text.contains(">Thequickbrownfox"), "缺原文引用：{text}");
        assert!(text.contains(">Helloworld"), "缺第二条原文引用：{text}");
        // 译文（CJK）经 Type0 嵌入字体写入且可提取（与原文不同的文字系统）
        assert!(
            text.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)),
            "提取文本无 CJK，译文未写入或不可提取：{text}"
        );
        eprintln!("提取样例：{}", &text.chars().take(200).collect::<String>());
    }
    let _ = std::fs::remove_dir_all(&dir);
}
