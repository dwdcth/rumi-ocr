<div align="center">

# Rumi OCR

**免费、开源、跨平台的离线截图 OCR 文字识别工具**

Rust 复刻自 [Umi-OCR](https://github.com/hiroi-sora/Umi-OCR)，兼容 Umi 插件协议。

[功能](#-特性) · [下载构建](#-下载与构建) · [使用](#-使用) · [插件](#-插件) · [HTTP-API](#-http-api) · [开发](#-开发)

</div>

---

## ✨ 特性

![主界面（浅色）](docs/img/main-light.png)

- **免费开源，离线运行**：识别全程本地完成，无需联网，不上传任何数据
- **截图识别**：全屏 / 区域 / 活动窗口，一键截图即出文字；支持窗口内粘贴图片识别
- **原生引擎**：[oar-ocr](https://github.com/GreatV/oar-ocr)（PP-OCRv6，纯 Rust 实现，模型自动下载缓存）
- **公式识别**：开关一打，行内公式直接输出 LaTeX
- **批量识别**：多图 / 文件夹递归（500 上限）批量转文字或导出
- **PDF 智能处理**：pdf-inspector 逐页分类——文字层页直接提取（零失真），扫描页才
  渲染 + OCR；支持导出**双层可搜索 PDF**（页面原图 + 隐形文字层，CJK 字体子集化）
- **排版解析**：按行 / 段落合并 / 自然分段三种策略，识别后**免重新识别即时切换**
- **文字定位**：点结果行行号，预览图上高亮对应位置
- **划选复制 / 忽略区域 / 忽略规则**：预览图拖选裁剪复制；红框防水印（识别前像素置白）；
  正则行过滤——三重实用工具
- **二维码**：批量页扫码工具卡
- **系统托盘 + 全局热键**：默认 `Alt+Q` 区域截图 OCR；双击托盘图标呼出主窗口
- **Umi 插件兼容**：[Umi-OCR 插件](https://github.com/hiroi-sora/Umi-OCR_plugins)解压到
  `plugins/` 即用（系统 Python，不内置解释器）
- **HTTP API**：Umi 兼容子集，`127.0.0.1:1224`
- **多语言界面**：简体中文 / English，词条外置 JSON，易于补充翻译
- **深浅双主题** + 无边框自定义标题栏（最大化 / 最小化 / 关闭行为可配置）
- **运行日志**：文件落盘 + 设置页查看 / 复制 / 导出，排障不求人

| 平台 | 截图后端 | 状态 |
|---|---|---|
| Linux · Wayland | KDE Spectacle D-Bus / xdg-desktop-portal | ✅ 真机主力开发环境 |
| Linux · X11 | xcap + 内置冻结帧选择器 | ✅ Xephyr 实测 |
| Windows / macOS | xcap（真机验收待做） | 🚧 |

## 📥 下载与构建

依赖：Rust（edition 2024）；Linux 需 `webkit2gtk-4.1`、`poppler`（PDF）、`python3`（插件）。

```bash
git clone <本仓库地址>
cd rumi
cargo build --workspace          # 全量构建
cargo run -p rumi-ui             # GUI
cargo test -p rumi-ui -p rumi-ocr   # 测试（基线 57+1 / 30+2）
```

命令行工具：

```bash
./target/debug/rumi-capture region out.png    # 截图：full / region / window
cargo run -p rumi-cli -- ocr --full --copy    # 截图→识别→stdout/剪贴板 一条龙
cargo run -p rumi-ocr-cli -- file a.png       # 图片 / --pdf doc.pdf / --qr qr.png
cargo run -p rumi-server                      # HTTP API（前台）
```

## 📖 使用

1. 启动后常驻托盘；`Alt+Q`（可配置）或托盘菜单触发**区域截图 OCR**，
   也可在主窗口点 全屏 / 区域 / 活动窗口 / 粘贴识别
2. 结果区逐行可编辑、逐行复制 / 全部复制；点**行首数字**在预览图上定位该行
3. 预览图上拖选 = 裁剪复制；编辑"忽略区域"拖红框可永久防水印
4. 设置页：主题 / 语言 / 热键 / 端口 / 关闭按钮行为（**最小化到托盘**默认 / 直接退出）、
   通知 / 提示音 / 忽略规则；出问题可在**日志**卡片查看 / 导出运行日志发给开发者

## 🔌 插件

把 Umi-OCR 插件解压到 `plugins/<名字>/`（含 `__init__.py`），在 UI「插件」页选择并配置。协议兼容点：

- `PluginInfo{group:"ocr", global_options, local_options, api_class}`
- `Api`: `__init__(globalArgd)` / `start(argd)` / `stop()` / `runPath` / `runBytes` / `runBase64`
- 宿主模块 `plugin_i18n`（i18n.csv 翻译）、`umi_log` 由随应用分发的 pyshim 提供
- 有 pip 依赖的插件：自动用系统 python3 建托管 venv（`~/.local/share/rumi/venv`）并安装

内置插件 `plugins/wechat_ocr`：调用**微信 4.x 本地 OCR 引擎**（离线、中文效果好），
Win/Linux/macOS 通用，需系统安装微信 + python3 `protobuf`。可识别**普通二维码与
微信小程序码**（原生引擎独有能力，小程序码返回原始载荷）；bytes 识别走**零拷贝
Shared 通路**（RGBA + mmmojo attach 缓冲区，微信自身截图同款，逆向结论见
[`docs/开发记录.md`](docs/开发记录.md) 2026-10-28 条目）。

## 🌐 HTTP API

```
GET  /            # 状态
POST /api/ocr     # {"base64":"..."} 或 {"path":"..."} → {"code":100,"data":[{text,box,score}]}
                  # 引擎由 config.toml 的 engine 决定（"native" | "plugin:<名>"），改配置热生效
```

## ⚠️ 已知限制

- **Linux 托盘双击不可用**：上游 tray-icon 的 GTK 后端无图标点击事件（源码级确认）。
  Linux 上单击/右键出菜单，菜单首项"打开主窗口"；**双击唤出主窗口在 Windows/macOS 正常**
- **Wayland 全局热键**：global-hotkey 在 Linux 是 X11 grab，纯 Wayland 焦点下不触发——
  托盘菜单是等价入口；彻底方案待接 org.kde.kglobalaccel
- **portal 区域截图**：KDE 后端未实现规范 v3 的 target=Area，故区域截图走 Spectacle；
  GNOME 后端实现后 portal 路径自动可用
- 桌面通知暂仅 Linux（Windows/macOS 待接系统通知）
- 插件若用 `sys.__stdout__` 直写会污染协议（Umi 同样不防御）

## 🛠 开发

架构与数据流、按日期的开发日志、踩坑记录与验证工作流，见
[`docs/开发记录.md`](docs/开发记录.md)。任务队列见 `docs/task-queue.md`。

## 📄 许可

本仓库代码以 [木兰宽松许可证，第2版（Mulan PSL v2）](LICENSE) 授权。

致谢与依赖：本项目设计参考 [Umi-OCR](https://github.com/hiroi-sora/Umi-OCR)（MIT），
识别引擎 [oar-ocr](https://github.com/GreatV/oar-ocr)、UI 框架 [Dioxus](https://dioxuslabs.com/)
等依赖依其自身许可证授权；`plugins/demo_AbaOCR` 为 Umi-OCR 官方 demo 插件原样收录，
版权归原项目。OCR 模型文件由 oar-ocr 运行时自动下载，模型许可见其项目页。

## 🙏 致谢

- [Umi-OCR](https://github.com/hiroi-sora/Umi-OCR) —— 产品设计与插件协议的蓝本
- [oar-ocr](https://github.com/GreatV/oar-ocr) —— 纯 Rust 的 PP-OCR 推理
- [Dioxus](https://dioxuslabs.com/) —— Rust 全栈 UI 框架
