# Rumi OCR

Umi-OCR 的 Rust 复刻：跨平台截图 OCR 工具，原生引擎 + Umi 插件协议兼容。

> 工作代号 Rumi（Rust + Umi）。方案与决策记录见 [`../PROPOSAL-umi-clone.md`](../PROPOSAL-umi-clone.md)。

## 功能

| 功能 | 状态 | 说明 |
|---|---|---|
| 截图 OCR（全屏/区域/窗口） | ✅ | Wayland 原生选区（KDE 走 Spectacle）、portal 通用路径；X11/Win/macOS 走 xcap + 内置冻结帧选择器；识别结果逐行复制 |
| 原生 OCR 引擎 | ✅ | [oar-ocr](https://github.com/GreatV/oar-ocr)（PP-OCRv6，纯 Rust，模型自动下载） |
| Umi 插件兼容 | ✅ | Umi 协议插件丢进 `plugins/` 即用（系统 Python，无需内置） |
| 托盘 / 全局热键 | ✅ | dioxus 内置；Wayland 热键限制见下 |
| 批量图片 / PDF | ✅ | **PDF 智能路由**：pdf-inspector 逐页分类，文字层页直接提取（实测 1.3s→0.0s 且零失真），扫描页才渲染+OCR；混合 PDF 逐页路由；`--force-raster` 保底；**文件夹递归**（500 上限） |
| 忽略区域 / 忽略规则 | ✅ | 双保险防水印：预览图拖**红框区域**（识别前像素置白）+ 正则行过滤（设置页，HTTP 亦支持）；双层 PDF 文字层同步过滤 |
| 公式识别 | ✅ | 识别页"公式"开关：oar Structure+FormulaNet 管线，行内 LaTeX（首次下载约 230MB 模型） |
| 提示音 | ✅ | 合成双音（无资产文件），默认关 |
| HTTP API | ✅ | Umi 兼容子集，`127.0.0.1:1224`，可切引擎；`rule` 参数别名 / `bad_regex` 上报 |
| CLI 一条龙 | ✅ | `rumi-capture ocr [--full] [--copy]`：截图→识别→stdout/剪贴板 |
| 设置持久化 | ✅ | `~/.config/rumi/config.toml` |
| 双层可搜索 PDF 导出 | ✅ | oxidize-pdf：页面原图 + 隐形文字层（Tr 3），CJK 字体 TTC 抽面嵌入并自动子集化，pdftotext 验证可搜索 |

## 架构

```
rumi/
├── capture          # 跨平台截图：Wayland portal / KDE Spectacle D-Bus / xcap（X11/Win/macOS）
├── cli              # rumi-capture CLI（组合 crate：链接 capture+ocr，ocr 一条龙进程内直调）
├── ocr              # oar-ocr 封装：OcrEngine 单例、recognize(_many)、图片/PDF 装载、QR
├── plugins-worker   # Umi 插件宿主：Python worker 的 JSON-RPC 客户端、venv 管理
├── pyshim           # 随应用分发的 Python shim：umi_worker.py / plugin_i18n / umi_log
├── server           # axum HTTP API（引擎热切换、token 可选）
├── ui               # Dioxus 0.7 前端：识别/插件/批量/设置 四页 + 托盘 + 热键
└── plugins/         # 用户插件目录（demo_AbaOCR 为官方 demo，可删）
```

数据流：`capture` 出 RGBA `Frame`（经 `into_rgba_image` 转 `image::RgbaImage`，ocr crate 不依赖
capture）→ `ocr::OcrEngine`（native）或 `plugins-worker`（Umi 插件，`runBytes` 直传 PNG 字节）
→ 统一 `OcrLine{text,score,box}` → UI / HTTP。

## 构建与运行

依赖：Rust（edition 2024）、webkit2gtk-4.1（Linux UI）、poppler（PDF）、python3（插件）。

```bash
cargo build --workspace
cargo run -p rumi-ui        # GUI
cargo run -p rumi-server    # HTTP API（前台）
cargo run -p rumi-ocr-cli -- file a.png          # CLI：图片
cargo run -p rumi-ocr-cli -- --pdf doc.pdf       # CLI：PDF
cargo run -p rumi-ocr-cli -- --qr qr.png         # CLI：二维码
./target/debug/rumi-capture region out.png       # CLI：截图（三种模式）
```

## 插件

把 [Umi-OCR 插件](https://github.com/hiroi-sora/Umi-OCR_plugins)解压到 `plugins/<名字>/`（含 `__init__.py`），
在 UI「插件」页选择并配置即可。协议兼容点：

- `PluginInfo{group:"ocr", global_options, local_options, api_class}`
- `Api`: `__init__(globalArgd)` / `start(argd)` / `stop()` / `runPath` / `runBytes` / `runBase64`
- 宿主模块 `plugin_i18n`（i18n.csv 翻译）、`umi_log` 由 pyshim 提供
- 有 pip 依赖的插件：设置页会用系统 python3 建托管 venv（`~/.local/share/rumi/venv`）并按映射表安装

## HTTP API

```
GET  /                     # 状态
POST /api/ocr              # {"base64":"..."} 或 {"path":"..."} → {"code":100,"data":[{text,box,score}]}
                           # 引擎由 config.toml 的 engine 决定（"native" | "plugin:<名>"），改配置热生效
```

## 已知限制

- **Wayland 全局热键**：global-hotkey 在 Linux 是 X11 grab，纯 Wayland 焦点下不触发——托盘菜单是等价入口；
  彻底方案待接 org.kde.kglobalaccel（KDE）
- **portal 区域截图**：KDE 后端未实现规范 v3 的 target=Area（AvailableTargets=0），故区域截图走 Spectacle；
  GNOME 后端未来实现后 portal 路径自动可用
- Spectacle D-Bus 方法为 no-reply 模式，结果经 ScreenshotTaken/ScreenshotFailed 信号返回（capture 已按此实现）
- 插件若用 `sys.__stdout__` 直写会污染协议（Umi 同样不防御）

## 路线

- ~~M4：X11 / Windows / macOS 截图后端~~ → 已由 M 波 xcap 承担（X11 已 Xephyr 实测；Win/mac 真机验收延后），选区交互由内置冻结帧选择器补齐
- kglobalaccel 热键、公式/表格版面输出页、Umi 词条级 i18z 完整迁移、打包（AppImage/MSI/DMG）
