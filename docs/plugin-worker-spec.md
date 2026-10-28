# Rumi 插件系统规范（M2）—— Umi-OCR 协议兼容层

> 所有协议细节均已从 Umi-OCR 源码核实（2026-10，main 分支）。实现方不需要重新考古。

## 1. 目录布局

```
rumi/
├── plugins-worker/            # Rust crate：宿主侧（spawn python、IPC、venv 管理）
├── pyshim/                    # 随应用分发的 Python shim（纯文件，无需安装）
│   ├── umi_worker.py          # 入口：JSON-RPC 循环（见 §3）
│   ├── plugin_i18n.py         # 宿主模块复刻（见 §4.1）
│   └── umi_log.py             # 宿主模块复刻：logger（见 §4.2）
└── plugins/                   # 用户投入 Umi 插件的目录（与 Umi 兼容：UmiOCR-data/plugins）
    └── demo_AbaOCR/
        ├── __init__.py        # 导出 PluginInfo
        ├── aba_ocr.py
        ├── aba_ocr_config.py
        └── i18n.csv
```

## 2. Umi 插件契约（源码实证，不得更改）

`plugins/<名字>/__init__.py` 必须导出：

```python
PluginInfo = {
    "group": "ocr",                  # Umi controller 中 PLUGINS_GROUPS = ["ocr"]，唯一组
    "global_options": dict | None,   # 声明式配置 schema
    "local_options": dict | None,
    "api_class": Api,
}
```

`Api` 类：

```python
class Api:
    def __init__(self, globalArgd: dict)          # 全局配置
    def start(self, argd: dict) -> str            # "" 成功；"[Error] xxx" 失败
    def stop(self)
    def runPath(self, imgPath: str) -> dict
    def runBytes(self, imageBytes: bytes) -> dict
    def runBase64(self, imageBase64: str) -> dict
# 成功: {"code": 100, "data": [{"text": str, "box": [[x,y]×4], "score": float}]}
# 失败: {"code": 102, "data": "错误文本"}
```

配置 schema 形态（aba_ocr_config.py 实证）：

```python
globalOptions = {
    "title": tr("阿巴阿巴OCR"),     # tr 在 import 时已求值 → 读到时已是 str
    "type": "group",
    "api_key": {"title": tr("Api密钥"), "default": "", "toolTip": tr("...")},
}
localOptions = {
    "title": tr("..."), "type": "group",
    "language": {"title": tr("语言"), "optionsList": [["zh_CN","简体中文"], ...]},
}
```

节点类型：`group`（容器）、叶子含 `default`（文本/数字类）、`optionsList`（下拉）。**UI 按此 schema 动态渲染配置页。**

## 3. worker 协议（stdio，按行分隔的 JSON-RPC）

宿主启动：`<python> umi_worker.py <plugin_dir> [--lang zh_CN]`
- `<python>` = 托管 venv 的解释器（见 §5），无 venv 时回退系统 `python3`

消息（每行一个 JSON 对象，双向）：

```
host→worker  {"id":1,"method":"info"}
worker→host  {"id":1,"ok":true,"result":{"name":"demo_AbaOCR","group":"ocr",
             "global_options":{...},"local_options":{...}}}     # schema 已转纯 JSON

host→worker  {"id":2,"method":"init","global":{...}}             # 实例化 Api(globalArgd)
host→worker  {"id":3,"method":"start","local":{...}}             # 返回 {"ok":bool,"error":str}
host→worker  {"id":4,"method":"runPath","path":"/tmp/x.png"}
host→worker  {"id":5,"method":"runBytes","b64":"<base64>"}       # bytes 经 base64 过 JSON
host→worker  {"id":6,"method":"runBase64","b64":"..."}
worker→host  {"id":4,"ok":true,"result":{"code":100,"data":[...]}}  # 原样转发插件返回值

host→worker  {"id":7,"method":"stop"}
任意 stderr   → 原样转发到宿主日志（插件 print 走这里）
worker 崩溃  → 宿主读 EOF，报 Error::WorkerDied，可自动重启（重启后须重走 init+start）
```

- id 严格递增；宿主不做管线复用（一问一答即可，引擎插件可能长耗时，调用期间 UI 显示忙碌）
- runBytes 的 bytes → base64 string 装载（JSON 无 bytes）

## 4. shim 宿主模块（插件 `import` 的是这些名字，必须在 worker 的 sys.path 上）

### 4.1 plugin_i18n.py（语义照抄 Umi 源码）

```python
LangCode = ""
def setLangCode(langCode): global LangCode; LangCode = langCode

class Translator:
    def __init__(self, file="", name=""):
        # 读 os.path.dirname(file)/name 的 CSV；header 是语言码列
        # 找 LangCode 列；找不到时 zh_* → 第0列，否则第1列
        # self._trDict[row[0]] = row[index]
    def __call__(self, original):
        return self._trDict.get(original, original)
```

CSV 格式：首行 `原文列,语言1,语言2,...`（Umi 实际表头含 `zh_CN,en_US,...`）。

### 4.2 umi_log.py（plugin_i18n 依赖它；插件也可能直接用）

```python
import logging
logger = logging.getLogger("rumi-plugin")   # worker 里 basicCallback 到 stderr 即可
```

**注意**：若社区插件 import 了其他 `umi_*` 宿主模块（如 umi_log、Utils），worker 捕获 ImportError 并上报宿主，宿主在插件详情里显示"缺少宿主模块 xxx"，逐个补齐——不要一次猜全集。

## 5. 托管 venv 策略（用户已拍板）

- 数据目录：`$XDG_DATA_HOME/rumi/venv`（默认 `~/.local/share/rumi/venv`）
- 首次启用插件时：无 venv → `python3 -m venv <dir>`；然后查 §6 映射表 `pip install`
- 用户可在设置里手动追加 pip 包
- 探测顺序：venv 存在用 venv python，否则系统 python3（并提示依赖自负）

### 6. 已知社区插件依赖映射（初始表，可被设置文件扩展）

| 插件 | pip 依赖 | 说明 |
|---|---|---|
| demo_AbaOCR | （无） | 纯 demo |
| wechat_ocr（本仓库内置） | protobuf（零拷贝通路另需 Pillow） | 微信 4.x 本地引擎，库随插件 vendored（`plugins/wechat_ocr/wechat_ocr/`，上游 dwdcth/wechat_ocr MIT）；runBytes 走零拷贝 Shared 通路（RGBA+attach，request_id=10018，无 Pillow 回退 tmpfs 临时文件，见开发记录 2026-10-28）；可识别二维码(type=19)/小程序码(type=22)，条码走文字行 |
| win7_x64_RapidOCR-json | （无，附 C++ 可执行） | 引擎走子进程 stdio JSON |
| win_linux_PaddleOCR-json | （无，附可执行） | 同上 |
| win7_x64_Pix2Text | pix2text | pip 依赖较重 |
| MistralOCR | requests | 在线 API |

## 7. Rust 侧 crate 接口（plugins-worker）

```rust
pub struct PluginEntry { pub name: String, pub global_options: serde_json::Value, pub local_options: serde_json::Value }
pub struct PluginProcess { /* child + stdin/stdout 行读写 */ }

impl PluginProcess {
    pub fn spawn(python: &Path, plugin_dir: &Path, lang: &str) -> Result<Self>;
    pub async fn info/start/run_path/run_bytes/run_base64/stop(&mut self, ...) -> Result<serde_json::Value>;
}
pub fn scan_plugins(dir: &Path) -> Vec<PathBuf>;   // 含 __init__.py 的一级子目录
```

- run 返回的 `{"code":100,"data":[...]}` 转成统一 `OcrLine{text, box:[[f32;4];4]? , score}` 中间格式（box 是 4 点 × xy）
- 与 oar-ocr 的 `text_regions` 输出共用同一个 `OcrResult` 枚举：`OcrResult::Native(...)` / `OcrResult::Plugin(...)`

## 8. 验收标准（M2）

1. `plugins/demo_AbaOCR`（从 Umi-OCR_plugins 仓库取）投入 plugins/ 后：扫描 → info → init → start → runPath 返回 `code:100, text:"阿巴阿巴阿巴"`（lang=zh_CN 时）
2. 配置 schema 能在 UI 渲染成表单（group/下拉/默认值/tooltip）
3. worker 被杀死后宿主能感知并重启恢复
4. i18n.csv 翻译生效（切 en_US 时 title 变英文）
