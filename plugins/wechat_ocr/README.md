# wechat_ocr —— 微信本地 OCR 引擎插件

调用微信（4.x）自带的本地 OCR 引擎识别文字，离线运行、识别快、中文效果好。
跨平台支持 Windows / Linux / macOS，需系统安装微信 4.x（Linux 为
`/opt/wechat-universal` 等 wechat-universal 发行版，macOS 为 WeChat.app，
Windows 为微信 4.x 安装目录）。

## 依赖

- Python 3.12+ 与 `protobuf`（`pip install protobuf`，或放进 rumi 托管 venv）
- 微信 4.x（插件不改动微信本体，仅调用其随附的 OCR 组件）

## 配置

- **全局 · 微信安装目录**：留空自动探测；也可用环境变量 `WECHAT_OCR_DIR`
  或 `~/.config/wechat_ocr/config.json`（详见库 README 的路径解析顺序）
- **会话 · 识别超时（秒）**：默认 30

## 行为说明

- **runBytes / runBase64 优先零拷贝通路**（微信自身截图 OCR 同款，逆向自
  微信主程序发送方）：RGBA 像素 + 元数据走 mmmojo attach 缓冲区
  （method=kMMShared，request_id=10018），**全程不落盘**；需 python3 `Pillow`
  解码，无 Pillow 时自动回退「tmpfs 临时文件 → 路径」版
- runPath 走 10010 路径版（该通路的 `pic_data` 字段是死字段，引擎只读
  `pic_path`；err=2/4/5 = pb 解析失败/文件打不开/短读）
- 结果映射：`ocrResult` 的 `location`（矩形）转 Umi 四点 `box`，`rate` → `score`
- **二维码 / 小程序码**：微信引擎基于改造版 zxing（含自有 `vqrcode` 扩展），
  可识别普通二维码（type=19，返回解码内容）与**微信小程序码/菊花码**
  （type=22，返回原始载荷——跳转信息需微信服务端解析，微信自己扫码拿到的
  也是这个串）。会话选项「二维码/小程序码」控制输出时机：仅当图中无文字时
  返回（默认）/ 总是附加在文字后 / 不返回；小程序码行带 `[小程序码]` 前缀
- 条形码（EAN-13 等）数字由引擎作为普通文字行返回

## 来源与许可

引擎封装库 `wechat_ocr/` vendored 自
[dwdcth/wechat_ocr](https://github.com/dwdcth/wechat_ocr)（MIT License），
其调用方式翻译自 [EEEEhex/QQImpl](https://github.com/EEEEhex/QQImpl)，
Linux/macOS 协议参考 [swigger/wechat-ocr](https://github.com/swigger/wechat-ocr)。
本目录内对该库的改动仅为新增 `DoOCRTaskData`（字节流识别入口）。
