# M1 任务书：oar-ocr 集成 + 截图 OCR 闭环

> 输入：M0 完成的 `capture` crate（全屏/区域/窗口捕获）与 `ui` crate（Dioxus 壳）。

## 目标

热键外的一切：截图 → oar-ocr 识别 → 结果面板（逐行复制/全选复制）→ 可再次截图。形成截图 OCR 最小可用闭环。

## 硬事实（主管已实测，直接用）

1. **依赖必须 git 版**（crates.io 0.1.1 是旧版，没有 auto-download）：
   ```toml
   oar-ocr = { git = "https://github.com/GreatV/oar-ocr", features = ["auto-download"] }
   ```
2. 可用 API（探针 /home/banshan/项目/jt/oar-probe/src/main.rs 已跑通，抄它）：
   ```rust
   use oar_ocr::prelude::*;
   let ocr = OAROCRBuilder::pp_ocrv6(PpOcrV6Size::Small).build()?;   // 引擎一次性构建
   let img  = load_image(Path::new(p))?;                              // 文件
   let out  = ocr.predict(vec![img])?;                                // Vec<ImageResult>
   for tr in &out[0].text_regions { tr.text_with_confidence() -> Option<(String, f32)> }
   ```
   首次构建含模型下载 ~2.4s（显示"正在下载模型"状态），之后 68ms/张。
3. `capture` 的 `Frame { rgba, width, height }` 是内存 RGBA——不要落盘再 load_image：
   oar-ocr 的输入图像类型用 `oar_ocr` 的图像构造（读 prelude 里 `load_image` 返回的类型；
   若有 `image::DynamicImage` → oar 图像的 From/转换，直接转，避免临时文件）。
   如果确实只有文件入口，落 tempfile 也可接受（标注 TODO）。
4. 引擎是重资源（ONNX session）：**全局只建一次**（lazy static / OnceLock），放后台线程，
   与 UI 隔离；`predict` 是批量接口，批量页留 M3 复用。

## 交付物

1. workspace 新成员 `ocr`（库）：`OcrEngine` 包装（init/recognize(Frame)->Vec<OcrLine>），
   `OcrLine { text, score, box:[[f32;2];4] }` 统一中间格式（M2 的插件输出将来也转这个）
2. `ui` 接线：
   - 状态行显示引擎状态（下载中/就绪/识别中）
   - 截图按钮（全屏/区域/窗口）→ 捕获 → 识别 → 结果面板：图预览 + 逐行文本（可点选复制）+ 全部复制
   - 结果面板样式对齐 Umi：左图右记录
3. 冒烟：`cargo build` 通过；主管验收时手动跑 region 闭环

## 边界（不要做）

- 不做热键/托盘/HTTP（M3）、不做插件（M2）、不做设置持久化（M3 一起）
- 不动 `capture` crate 的公共 API（M0 已验收）；确有必要改，报告里说明
