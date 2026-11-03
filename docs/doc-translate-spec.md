# 规格 J/K：文档翻译导出（docforge 双语回写接入，2026-11-03）

> 前置：docforge 已更新至 main@435dcaa（`to_bilingual_markdown(_with)` +
> `BiStrategy::{BilingualInline,TranslationOnly,OriginalPlusAppendix}` +
> `to_translation_units(_with)` + `TranslationUnit{id,page,kind,text,source,
> confidence,lang}` + `ParsePhase` 进度）。v1 回写目标 = Markdown；docx/epub
> 回写上游在写，到位后只加出口不改链路。rumi-ocr 已依赖 docforge（同 git 源
> 统一 checkout），本工作项让 translate crate 也依赖它。

## 工人 J（translate crate + CLI）

只动：`translate/Cargo.toml`、`translate/src/docmt.rs`（新建）、`translate/src/lib.rs`
（仅 `pub mod docmt;` 一行）、`translate/src/bin/rumi_translate_cli.rs`、
`translate/tests/docmt_tests.rs`（新建）。禁止动：engine/online/download/registry/
segment/postfix、ocr/、ui/、server/。

### 1. Cargo.toml 追加

```toml
# 文档翻译（v1：解析→翻译单元→逐单元翻译→双语 Markdown 回写）。
# 与 ocr crate 同一 git 源，cargo 统一 checkout；翻译场景 feature 集
# （文本族随 default 自带）：encoding=中文编码链，lang-detect=源语言检测
# （TranslationUnit.lang，v2 自动检测用），docx/epub/pdf=主流文档格式。
docforge = { git = "https://github.com/dwdcth/docforge", default-features = false, features = ["encoding", "lang-detect", "docx", "epub", "pdf"] }
```

注意：default-features=false 时 docforge 的 pdf feature 需 engine 才能处理扫描页
——文档翻译场景**不注入**引擎：扫描 PDF 会得空页（OcrMode::Auto 无引擎……查
`docforge::parse` 源码确认无引擎时 Auto 行为；若报错则 docmt 层先
`PdfOptions{mode: OcrMode::Auto, engine: None}` 自行装配并接受扫描页空结果，
错误文案转译 "含扫描页且未注入 OCR 引擎（v1 暂不支持扫描 PDF 翻译，请转图片批量 OCR）"）。
以实际源码为准，歧义记回报。

### 2. docmt.rs

```rust
//! 文档翻译：docforge 解析 → 翻译单元 → 引擎翻译 → 双语 Markdown 回写。
use std::path::Path;

/// 双语策略（镜像 docforge::emit::BiStrategy）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BiStrategy { #[default] Inline, Only, Appendix }
impl BiStrategy {
    pub fn parse(s: &str) -> Option<Self> { /* "inline"|"only"|"appendix"，未知 None */ }
}

pub struct DocTranslateOptions {
    pub engine: crate::Engine,
    pub from: String,          // 内部码；单元级不自动检测（unit.lang 留 v2）
    pub to: String,
    pub strategy: BiStrategy,
    pub polish: bool,          // 与 facade 同义（zh 目标时润色）
    pub min_chars: usize,      // 默认 2（docforge 同款）
    pub on_progress: Option<std::sync::Arc<dyn Fn(usize, usize) + Send + Sync>>,
    // (已翻译单元数, 总单元数)；调用方取消语义 v1 不做（返回 false 中止留 v2）
}

pub struct DocStats { pub units: usize, pub chars: usize, pub elapsed_ms: u128 }

/// 主入口：input（任意 docforge 支持格式）→ out_md 双语 Markdown。
/// 流程：parse → to_translation_units_with(min_chars) → 按 16 单元/批调
/// [`crate::translate`]（texts 收集、空串原样、Err 直透传带单元 id 上下文
/// "单元 {id} 翻译失败：{e}"）→ 每批完成回调 on_progress → 回写落盘。
/// units 为空 → Err("文档无可翻译文本单元（空文件或全为非文本块）")。
pub fn translate_document(
    input: &Path, out_md: &Path, opts: &DocTranslateOptions,
) -> Result<DocStats, String>;
```

引擎批量注意：`crate::translate` 是逐条接口——直接循环逐单元调（离线引擎内部
有 Translator 批缓存，量级可接受）；进度按单元计。docforge 的
`to_bilingual_markdown_with(doc, &translated, strategy)` 的 translated =
`Vec<(String, String)>`（id, 译文）；**doc 需要在函数内持有**（parse 返回的
Document 直接用，units 从它出）。

### 3. CLI 追加

```
--doc <path>      文档翻译模式：解析→翻译→双语 Markdown（与 --engine/--from/--to 组合）
--doc-out <path>  输出路径（默认 <输入名去扩展>.bilingual.md，存输入同目录）
--strategy <s>    inline(默认) | only | appendix
```
`--doc` 时忽略位置参数/stdin 文本源；进度 stderr `已翻译 n/total 单元`；
完成打印统计行 `单元 N · 字符 M · 耗时 X s → <out>`。

### 4. 测试（docmt_tests.rs）

不打网不跑真模型：
- BiStrategy::parse 四例（三合法 + 未知 None）。
- 回写骨架：构造 `Document`（手搓 model 类型：两页各一块 Paragraph，块带 id
  锚点口径 p{n}-b{n}——**注意** to_translation_units 从 doc 现算 id，测试直接用
  docforge emit 两侧（units → 假译文 → to_bilingual_markdown_with）断言输出含
  原文+译文交错（Inline）/仅译文（Only）/附录（Appendix）三种形态各一例。
  （即测 docforge 组合用法在 rumi 侧的契约，不重测 docforge 自身。）
- translate_document 的错误分支：文件不存在（Err 含"文件不存在"）——用不存在的
  路径即可，不触发解析。
- 真模型 E2E 一个 `#[ignore]`：en_zh 已装时对 `ocr/src/pdf_route.rs` 的
  sample 生成 PDF 跑 translate_document（from en to zh），断言输出文件存在且
  非空且含中文。sample 生成器在 rumi-ocr（translate tests 不依赖 ocr crate——
  #[ignore] 用例内联用 docforge 能解析的最小 txt 文件替代即可，避免跨 crate 依赖）。

## 工人 K（UI）

只动：`ui/src/translate_ui.rs`、`ui/assets/i18n/{zh-CN,en-US}.json`。
禁止动：settings.rs、main.rs、translate crate、其他。

### 5. 翻译页加「翻译文档…」按钮

- 位置：引擎行右侧或「翻译」按钮旁（btn-sm，不抢主按钮视觉）。
- 点击：std 线程 rfd 选文件（过滤器：pdf/docx/epub/txt/md/html/csv/rtf/srt；
  Linux GTK 死锁教训——rfd 必须 std 线程，模式照抄现有「导入语言包…」）→
  选中后再 rfd 保存对话框（默认名 `<原名>.bilingual.md`，同线程）→ 后台线程
  调 `rumi_translate::docmt::translate_document(input, out, &opts)`（opts：
  engine=engine_from_settings(&st)、from/to=当前下拉值、strategy=Inline、
  polish=st.polish、min_chars=2、on_progress=进度回调）→ 进度复用现有 prog
  信号模式（文案 `已翻译 n/N 单元`）→ 完成状态行显示 `单元 N · 耗时 X s → <文件名>`
  或红字错误。
- 依赖 API（工人 J 并行实现，按此签名假想编码）：
  `rumi_translate::docmt::{translate_document, DocTranslateOptions, BiStrategy, DocStats}`
- 与「翻译」按钮互斥（同一 busy 信号）。

### 6. i18n 词条（zh / en）

| key | zh | en |
| --- | --- | --- |
| translate.doc_button | 翻译文档… | Translate document… |
| translate.doc_running | 文档翻译中… | Translating document… |
| translate.doc_progress | 已翻译 {p1}/{p2} 单元 | {p1}/{p2} units done |
| translate.doc_done | 单元 {p1} · 耗时 {p2} s → {p3} | {p1} units · {p2}s → {p3} |
| translate.doc_failed | 文档翻译失败：{p} | Document failed: {p} |
| translate.doc_filter | 可翻译文档 | Translatable documents |

## 7. 验收

- J：`cargo test -p rumi-translate` 全绿（原 89+ 新增 ≥7）；`cargo check -p
  rumi-translate` 0 warning；CLI 冒烟（真件 txt→md，贴回报输出）。
- K：`cargo check -p rumi-ui` 0 warning；`cargo test -p rumi-ui` 基线不回归 +
  新增 ≥2 纯函数测试（如默认输出名推导 `fn default_doc_out(input)->PathBuf`、
  过滤器串构造）。
- 监工复验：workspace 全测 + CLI 真件端到端 + UI 手动冒烟。
