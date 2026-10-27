# M3 任务书：对齐 Umi 主功能（热键/托盘/批量/PDF/二维码/HTTP/设置）

> 前置：M1（OCR 闭环）与 M2（插件 worker）已完成。
> 本阶段把应用从"能截图识别"补齐到"Umi-OCR 主要功能对齐"。

## 1. 全局热键

- crate `global-hotkey`（跨平台；Linux 走 X11/evdev——Wayland 下 global-hotkey 在 KDE 可用性需实测，
  失败则回退：注册 KDE 快捷键 D-Bus（org.kde.kglobalaccel）或文档说明 Wayland 限制 + 提供"点击托盘菜单触发"）
- 默认热键对齐 Umi：截图 OCR `Alt+Q`（可配置）；截图粘贴/批量窗口各一个
- 热键触发时窗口可隐藏到托盘——捕获/识别流程照常，结果弹小窗（复用主窗口置顶 + 通知）

## 2. 系统托盘

- crate `tray-icon`（wry/tao 生态配套）
- 菜单：打开主窗口 / 截图OCR / 批量OCR / 暂停热键 / 退出
- 状态反馈：引擎就绪/识别中/插件启用数

## 3. 批量 OCR + PDF

- oar-ocr 的 predict 本来就是批量接口；`ocr` crate 的 OcrEngine 加 `recognize_many(&[Frame])`
- 文件入口（M1 只吃 Frame）：图片文件 → image crate 解码 → Frame；PDF → 查 oar-ocr CLI/pdf 支持的库接口
  （读 git checkout 源码确认 PDF 分页 API；若有直接用，没有则用 pdfium-render 或 pdftoppm 子进程兜底——报告里说明选型）
- UI：拖拽/选择多文件 → 队列 → 逐个识别 → 汇总导出（txt/markdown，Umi 支持的格式）

## 4. 二维码

- crate：`rqrr`（纯 Rust qrcode 识别）或 `bardecoder`
- 识别流程：OCR 完成后对同一 Frame 顺带扫二维码（开关控制）

## 5. HTTP API（Umi 兼容子集）

- crate `axum`，绑定 127.0.0.1:1224（Umi 默认端口，可配置）
- Umi 兼容路由（读 Umi-OCR 源码 PySide6/api/* 对照，至少实现）：
  - `GET /` 或 `/api` 状态
  - `POST /api/ocr` body: {base64|path|bytes} → Umi JSON 格式 {code:100,data:[{text,box,score}]}
  - 语言/插件选择参数透传
- 鉴权：可选 token（默认关）

## 6. 设置持久化

- `~/.config/rumi/config.toml`（crate `toml` + `serde`）
- 项：热键、引擎选择（native/plugin:xxx）、插件 local/global options（M2 的 schema 值）、
  HTTP 端口/token、语言（联动 plugin_i18n setLangCode）、主题

## 7. 纪律

- 不重写 M1/M2 的公共 API，只调用；确需改动在报告里单独列出理由
- 每个子功能独立 commit 粒度的提交不做（无 git），但每个功能要在报告里给出冒烟方式与结果
- GUI 运行验收归主管；agent 只保证 build + 单测/CLI 冒烟

## 验收标准

1. 托盘图标出现，菜单可用，截图 OCR 从托盘可触发
2. 热键在 KDE Wayland 实测可用（或给出明确限制说明 + 替代触发路径）
3. 批量 3 张图识别导出 txt 成功；PDF 一份成功
4. `curl 127.0.0.1:1224/api/ocr` base64 图返回 Umi 格式 JSON
5. 配置改热键重启生效
