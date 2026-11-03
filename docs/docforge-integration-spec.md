# 规格 I：rumi PDF 解析切换 docforge（git 依赖，2026-11-03）

> 用户指令：文件解析改用 https://github.com/dwdcth/docforge（git 依赖）。
> 监工已核对 docforge main@58494928 接口（`PdfOptions{mode,dpi,engine,intra_page,
> on_progress,keep_raster_pages}`、`Block.quad`、`ocr::OcrEngine` trait、
> `Document/Page/Block{content,bbox,confidence,source,quad,kind}`、
> `backends::pdf::register` + `DocumentEngine::parse`）。
> 关键事实：docforge 的 pdf 后端构建于 pdf-inspector 之上（行粒度同一函数
> `group_into_lines_preserving_all_text`）；其 image 依赖 0.25 与 rumi-ocr 同一
> 实例（`&image::RgbaImage` 零拷贝互通）；文字层块 = PDF pt 左下原点 y 向上，
> OCR 块 = 渲染像素左上原点（`Page.coord_units` 显式标注）。

## 工人 I 文件集

只动：`ocr/Cargo.toml`、`ocr/src/pdf_route.rs`（重写）、`ocr/src/bin/rumi_ocr_cli.rs`
（route 调用适配）、删除 `ocr/examples/pdf_text_probe.rs`（阶段一探针，使命完成，
删除并在回报注明）、`Cargo.lock`（自动）。排查但按需最小改：`ocr/src/lib.rs`
（若 load_pdf_pages 仅剩 route 使用则保留不动——server/其他处可能仍用）。
**禁止动**：translate/ ui/src/translate_ui.rs server/src/。

## 1. Cargo.toml

删 `pdf-inspector = "1"`；加：

```toml
# 文档解析（PDF 页级路由：文字层直提 vs 渲染+OCR，引擎注入式）。
# git 依赖为分发渠道（上游暂不发布 crates.io）；跟 main 滚动，
# 更新用 cargo update -p docforge。pdf feature 自带 pdf-inspector（传递依赖）
# 与 image 0.25（与本 crate 同实例）。
docforge = { git = "https://github.com/dwdcth/docforge", default-features = false, features = ["pdf"] }
```

## 2. pdf_route.rs 重写

```rust
//! pdf_route —— PDF 智能路由（docforge 端到端）。
//! 文字层页直提行（pt、左下原点、y 向上，score=1.0）；扫描页 docforge 内部
//! pdftoppm 渲染 + 注入的 rumi 引擎识别（像素、左上原点，带 confidence/quad）。
//! 坐标系按页区分（与旧实现契约一致，消费者按 source/坐标系分派）。

use crate::{OcrError, OcrLine};
use std::path::Path;
use std::sync::Arc;

/// 路由选项。
pub struct RouteOptions<'a> {
    /// rumi OCR 引擎（注入 docforge，扫描页识别用；避免模型双重加载）
    pub engine: &'a crate::OcrEngine,
    /// 页级进度回调透传：(已处理页数, 总页数) -> 是否继续（false 中止，
    /// 已产出页保留，余页空占位——docforge Ok 语义）
    pub on_progress: Option<Arc<dyn Fn(u32, u32) -> bool + Send + Sync>>,
    /// 扫描页渲染 DPI（默认 200，对齐 docforge DEFAULT_DPI）
    pub dpi: u32,
}
impl Default for RouteOptions<'_> { /* engine 无默认——用手写构造函数 */
}
// 提供 RouteOptions::new(engine) -> Self（on_progress None、dpi 200）。

/// 单页路由结果。Raster 变体已删除：渲染+OCR 在 docforge 内完成，
/// OCR 页的行同样出现在 Text 里（source 区分见 BlockSource）。
#[derive(Debug)]
pub enum PdfPage {
    Text(Vec<OcrLine>),
}

/// rumi 引擎 → docforge OcrEngine 适配器（零拷贝：image 0.25 同实例）。
struct RumiOcrAdapter<'a> { engine: &'a crate::OcrEngine }
impl docforge::ocr::OcrEngine for RumiOcrAdapter<'_> {
    fn name(&self) -> &str { "rumi-oar" }
    fn recognize(&self, image: &image::RgbaImage)
        -> std::result::Result<Vec<docforge::ocr::RecognizedLine>, String> {
        // engine.recognize(image) 的 OcrLine{text, box, score} → RecognizedLine
        // {text, score, quad}（box 四点直通）；Err(e) → Err(e.to_string())
    }
}

pub fn route_pdf(path: &Path, opts: &RouteOptions<'_>) -> Result<Vec<PdfPage>, OcrError> {
    // 预检保持文案"文件不存在：{path}"（server http 测试据此断言分支；
    // docforge 预检同文案，但 rumi 侧先查可省一次装配）
    // 装配：Registry::new() + docforge::backends::pdf::register(&mut r, PdfOptions {
    //   mode: OcrMode::Auto, dpi: opts.dpi,
    //   engine: Some(Arc::new(RumiOcrAdapter{engine: opts.engine})),
    //   intra_page: false, on_progress: opts.on_progress.clone(),
    //   keep_raster_pages: false }) + DocumentEngine::new(r).parse(path)
    // DocError → OcrError::Pdf(e.message)（attempts 丢弃）
    // Document.pages 逐页 → PdfPage::Text(blocks→OcrLine)：
    //   - source==TextLayer：score=1.0；box 四点从 bbox(pt, y 向上) 构造，
    //     顺序沿旧契约"左上→右上→右下→左下"（上 = PDF y 更大侧）：
    //     [(x0,y1),(x1,y1),(x1,y0),(x0,y0)]
    //   - source==Ocr：score=confidence；quad Some → box=quad 直通；
    //     quad None（理论不发生，防御）→ [(x0,y0),(x1,y0),(x1,y1),(x0,y1)]
    //   - content.trim() 空的块跳过（等价旧 text_line_to_ocr 空文本过滤）
    //   - 其他 source（Native 等，pdf 不产生）防御性同 TextLayer 处理 score=1.0
    // Document.warnings：丢弃（eprintln 一行即可；rumi 现状无 warnings 概念）
}
```

注意：`PdfPage` 的旧 `Raster(RgbaImage)` 变体删除；`text_line_to_ocr`、
pdf_inspector 相关 import 全部清掉。

## 3. CLI 适配（bin/rumi_ocr_cli.rs 195-235 区域）

- `route_pdf(pdf)` → `route_pdf(pdf, &RouteOptions::new(&engine))`（engine 为
  CLI 已有实例；进度回调 None）。
- 原 Raster 收集 + recognize_many 分支删除：全部页都是 `PdfPage::Text(lines)`，
  行直接进既有输出管线。
- 输出行为（行序/文本/页数）必须与替换前一致（见验收 2）。

## 4. 排查其余调用点

`rg -n "route_pdf|PdfPage|load_pdf_pages" --type rust`：逐一核对。ui 批量若经
server/CLI 之外的入口消费 PDF，按同一模式适配（engine 实例从该入口已有引擎取）。
`load_pdf_pages` 本体保留（可能仍有直接渲染消费）。

## 5. 测试与验收

1. `cargo test -p rumi-ocr` 基线 30+2 全绿（合成文字层 PDF 金测即行为等价守门）。
2. **切换前后对拍**：改前先跑 `cargo run -p rumi-ocr-cli -- file <合成PDF>` 存档
   输出（ocr/tests 的金测 fixture 或 examples 造的 PDF），切换后同命令 diff 一致
   （行文本/行数/页数；坐标若有微差逐字段说明原因）。
3. `rg -n "pdf_inspector" ocr/src ocr/examples ocr/tests` → 0 命中（源码零直接引用）。
4. `cargo test --workspace` 全绿（ui/server/translate 不回归）。
5. mock 引擎单测：RumiOcrAdapter 转发正确性（构造 OcrLine 断言 RecognizedLine
   字段映射；不必跑真模型）。

## 6. 回报格式

改动文件清单；切换前后对拍结果（一致或差异逐条）；调用点排查清单；
cargo test 关键行；歧义点清单。
