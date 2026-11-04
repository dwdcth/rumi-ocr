# 规格 M：双语 PDF 出口（#1，2026-11-04 备用，待工人 L 缓存合入后开工）

> 材料（监工已核实）：
> - docforge `writeback::appendix`：`to_appendix_pages(units, translated) ->
>   Vec<AppendixPage{source_page, entries: Vec<AppendixEntry{unit_id, original,
>   translation}>}>`（按源页分组、文档序、原/译对照完整）
> - rumi ocr/dual_pdf.rs 的 CJK 字体经验：系统候选探测 + TTC 头剥离重建
>   sfnt + oxidize-pdf Type0 整体嵌入（load_embeddable_font，当前私有）

## 设计决策（写死）

1. **出口语义**：`.pdf` 输出 = **任何输入格式都可**（docforge IR → 生成式双语
   PDF），不要求同格式输入（与 docx/epub 等回写不同——PDF 出口是纯生成）。
   needs_same_input 校验列表**不加** pdf。
2. **不合并原 PDF 页**（v1）：原/译对照都排在生成页里——每源页一节
   （AppendixPage 分组），条目 = 原文引用块（灰色小字）+ 译文正文（黑色）。
   输入恰为 PDF 时即 PageAlt 的 PDF 形态；其他输入是"译文对照集"。
   （原页级复制合并 oxidize-pdf 不支持，v2 再议。）
3. **字体**：`load_embeddable_font` 从 ocr/dual_pdf.rs **复制**进
   `translate/src/bilingual_pdf.rs`（~60 行，注明出处；不加 translate→ocr
   依赖，保持 translate 轻依赖）。探测失败退化 Helvetica（仅 latin 可写，
   CJK 行跳过计入 stats——与 dual_pdf 同款降级语义）。
4. 排版参数：A4、边距 50pt、原文引用 9pt 灰、译文 11pt 黑、
   每源页节标题 13pt（"第 {page} 节"，source_page None → 按序号）；
   行高 1.5 倍；内容溢出自动分页（oxidize-pdf 的换行/分页 API 以实际为准）。

## 工人 M 文件集

只动：`translate/src/bilingual_pdf.rs`（新建）、`translate/src/docmt.rs`
（仅输出分派加 "pdf" 臂）、`translate/src/lib.rs`（mod 声明一行）、
`translate/tests/bilingual_pdf_tests.rs`（新建）。
注意与工人 L 的缓存改动合并冲突：docmt.rs 只加分派臂，不碰翻译循环。

## API

```rust
/// units + translated → 双语对照 PDF。返回写入的条目数与跳过数
/// （字体覆盖不足时 CJK 行 skipped，与 dual_pdf 降级语义一致）。
pub(crate) fn export_bilingual_pdf(
    units: &[docforge::emit::TranslationUnit],
    translated: &[(String, String)],
    out: &std::path::Path,
) -> Result<(usize, usize), String>; // (written, skipped)
```

## 测试

- 无字体依赖：export 到 tempdir 断言文件存在且非空且 %PDF 头（真生成，
  无网无模型——from==to 直通 translated 由调用方构造）。
- 排版纯函数（节标题/引用块前缀构造）单测 ≥2。
- #[ignore] E2E：en_zh 已装时 txt → .pdf 端到端，pdftotext（若装）或二进制
  头+大小断言。

## 验收

cargo test -p rumi-translate 全绿；CLI `--doc x.txt --doc-out x.pdf` 冒烟贴输出。
