# 规格 A：translate crate 骨架 + 核心模块（lang/segment/postfix/registry/download/lib.rs）

> 工人 A。总设计见 `translate-design.md`。**本规格与实现冲突时以本规格为准。**
> 你只创建/修改本规格 §0 列出的文件；`engine.rs`、`online/`、`bin/` 只放**带签名桩**（由工人 C/B 覆写）。
> 依赖其假想 API 已存在进行编码；规格未覆盖的分歧点：保守实现并在最终回报中列出。

## 0. 文件集（只许动这些）

```
translate/Cargo.toml                        （新建，内容见 §1）
translate/src/lib.rs                        （新建，见 §7）
translate/src/lang.rs                       （新建，见 §2）
translate/src/segment.rs                    （新建，见 §3）
translate/src/postfix.rs                    （新建，见 §4）
translate/src/registry.rs                   （新建，见 §5）
translate/src/download.rs                   （新建，见 §6）
translate/src/engine.rs                     （新建桩，见 §8——只有签名，body todo!()）
translate/src/online/mod.rs                 （新建桩，见 §9——只有签名，body todo!()）
translate/tests/segment_postfix_tests.rs    （新建，单测，见 §10）
Cargo.toml                                  （工作区根：members 加 "translate"）
```

**禁止**：动 ui/ server/ cli/ ocr/ plugins-worker/ 的任何文件；动 engine.rs/online 的最终实现（那是别人覆写的）。

## 1. translate/Cargo.toml（逐字）

```toml
[package]
name = "rumi-translate"
version = "0.1.0"
edition = "2024"
license = "MulanPSL-2.0"

[lib]
name = "rumi_translate"
path = "src/lib.rs"

[[bin]]
name = "rumi-translate-cli"
path = "src/bin/rumi_translate_cli.rs"

[dependencies]
# 推理引擎：oneDNN 后端（vendored oneDNN v3.14.1，Intel+AMD Zen 优化）+ sentencepiece 分词。
# 关闭默认 ruy：int8 GEMM 需要 oneDNN。openmp-runtime-comp=GNU OpenMP 运行时。
ct2rs = { version = "0.9", default-features = false, features = ["dnnl", "openmp-runtime-comp", "sentencepiece"] }
# HTTP（在线翻译 + 模型下载）。tls=rustls 不引 openssl；Proxy::try_from_system 尊重代理环境变量
ureq = { version = "2", default-features = false, features = ["tls", "json", "gzip"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
dirs = "6"
# Argos .argosmodel（zip）导入解包
zip = { version = "2", default-features = false, features = ["deflate"] }

[dev-dependencies]
tempfile = "3"
```

工作区根 Cargo.toml：`members = ["capture", "cli", "ui", "plugins-worker", "ocr", "server"]` 末尾追加 `"translate"`（保持字母序可乱，追加即可），并照现有风格加一行注释 `# - translate: 离线(CTranslate2)+在线翻译核心（rumi-translate；ct2rs 重编译只在此 crate）`。

## 2. lang.rs

```rust
//! 语言代码规范化与展示名。

/// 规范化为 rumi 内部两位码（ISO 639-1 子集）。
/// zh-CN / zh_CN / zh-Hans / zhs / chs / chinese / 中文 → "zh"
/// zh-TW / zh-Hant / zht / cht → "zh"（v1 简体模型，繁体输入仍可译，不特殊处理）
/// en-US / en_GB / eng / english / 英文 → "en"
/// ja/jpn/japanese/日语 → "ja"；ko/kor → "ko"；fr/fra → "fr"；de/deu → "de"；
/// es/spa → "es"；ru/rus → "ru"；pt → "pt"；it → "it"；vi → "vi"；ar → "ar"；th → "th"
/// 其余：取小写后前两位（"JA"→"ja"），未知原样返回小写。
pub fn normalize(lang: &str) -> String;

/// 展示名（UI 下拉用）："zh"→"中文"，"en"→"英语"，"ja"→"日语"，"ko"→"韩语"，
/// "fr"→"法语"，"de"→"德语"，"es"→"西班牙语"，"ru"→"俄语"，"pt"→"葡萄牙语"，
/// "it"→"意大利语"，"vi"→"越南语"，"ar"→"阿拉伯语"，"th"→"泰语"，其他→原码
pub fn display(lang: &str) -> String;

/// v1 翻译页语言下拉的候选（写死顺序）：
pub const UI_LANGS: &[&str] = &["zh", "en", "ja", "ko", "fr", "de", "es", "ru", "pt", "it", "vi", "ar", "th"];
```

单测并入 segment_postfix_tests.rs：normalize 至少 12 条（含大小写、分隔符变体、中文写法）、display 3 条。

## 3. segment.rs（分句器，规则写死）

```rust
//! 规则分句：不引入 Stanza（其初始化需联网，见 docs/translate-design.md 选型）。

/// 把文本切成可独立翻译的段。
/// 规则（按序应用）：
/// 1. 先按 '\n' 切行（行是第一层单元，行结构由调用方另行保留）。
/// 2. 行内按终结符切：出现在 `。！？!?…；;` 之后的边界处断开，终结符留在前段。
///    连续终结符（如 "！！" 或 "？！"）并入前段。
/// 3. 若某段长度 > 300 chars，再按次级标点 `，,、:：` 就近二分（在 >150 chars
///    处找最近的次级标点，标点留前段），递归直到每段 ≤300 或无标点可切。
/// 4. 不产生空段；整行空白则该行输出空段由调用方处理（segment 本身输入非空文本）。
pub fn segment(text: &str) -> Vec<String>;

/// 便捷封装：多行文本 → Vec<Vec<String>>（外层=行，内层=该行的段）。
pub fn segment_lines(text: &str) -> Vec<Vec<String>>;
```

实现提示：用 `char_indices` 扫描；判断 CJK 终结符用 chars().next()。**单测用例逐条写死**（期望值不许改）：

| 输入 | 期望 segment 输出 |
| --- | --- |
| `"默认端口为8080。请先设置环境变量。"` | `["默认端口为8080。", "请先设置环境变量。"]` |
| `"Set X. Then start it!"` | `["Set X.", " Then start it!"]`（注意第二段带前导空格，保留原样） |
| `"第一句！！第二句？"` | `["第一句！！", "第二句？"]` |
| `"one,two,three"` | `["one,two,three"]`（无终结符不切） |
| 600 字中文逗号串（构造：`"甲，乙，丙，"`×100） | 全部段 ≤300 chars 且拼回原文等长 |
| `"你好"` | `["你好"]` |
| `""` | `[]` |
| 多行 `"第一行。\nsecond line\n"` | segment_lines → `[["第一行。"],["second line"],[""]]`（末尾空行产出空 Vec 内含一个空串——**修正**：空/纯空白行产出 `[""]` 一个空串段） |

> 上表最后一行以这个为准：`segment_lines` 对空行产出 `vec![String::new()]`，非空行产出该行分段结果。

## 4. postfix.rs（zh 译文润色，保守规则）

```rust
//! zh 译文的保守标点润色。文章实测痛点：半角句点/逗号、": "插空格、标点前空格。

/// to == "zh" 时由调用方启用。规则按序：
/// R1 '.'→'。'：当且仅当前一字符与后一字符【不同时】是 ASCII 字母或数字。
///    例："容器。"✓  "8080."（后是 EOL，前是数字→前数字后EOL：**不转**，
///    数字打头保护的判据是：前是数字【或】后是数字即跳过）。
///    修正判据（以此为准）：跳过当 前一字符是 [0-9A-Za-z] 且 后一字符是 [0-9A-Za-z]；
///    或 前一字符是 [0-9]；或 后一字符是 [0-9]。其余替换。
///    （"v1.2"前后字母数字→跳过；"8080."前数字→跳过；"Docker."后EOL→转；"end. Next"后空格→转）
/// R2 ','→'，'：跳过当前一或后一字符是 [0-9]（"1,000" 保护）。
/// R3 "N: M"数字冒号修复：正则性地——前一字符 [0-9] 且后一字符 [0-9] 的 ": " 去空格→"8: 15"→"8:15"。
/// R4 标点前空格删除：`。！？，、：；）】》"` 前的空格删除（"秒 。"→"秒。"）。
/// R5 句末标点后跟 CJK 字符时，其间空格删除（"。 下一步"→"。下一步"；仅当后是 CJK）。
/// 其余空格一律保留（拉丁词间、中西文之间不动）。
pub fn polish_zh(text: &str) -> String;
```

单测逐条（写死）：R1 四例（"8080." 不变、"v1.2.3" 不变、"Docker."→"Docker。"、"Done. OK"→"Done。 OK"）；R2（"1,000" 不变、"a, b"→"a， b"）；R3（"8: 15"→"8:15"、"127.0.0.1: 8080"→"127.0.0.1:8080"、注意 R1 已保住 IP 的点）；R4（"30 秒 。"→"30 秒。"）；R5（"。 下一步"→"。下一步"、"。 Next" 不变）。

## 5. registry.rs（内置模型注册表，数值写死）

```rust
//! 内置离线模型注册表。每个方向一个 spec，sources 按优先级排列（argos 官方包
//! 直链为主源、HF opus-mt 多文件为备源），安装后统一目录布局（见 §6）。

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(tag = "kind")]
pub enum ModelSource {
    /// Argos 官方 .argosmodel 直链（单 zip 文件，下载后解包安装）。
    /// URL = mirrors.argos_base + "/" + file；bytes 为直链 content-length（2026-11-03 实测）。
    Argos { file: &'static str, bytes: u64 },
    /// HuggingFace 仓库多文件。URL = https://{host}/{repo}/resolve/main/{file}。
    Hf { repo: &'static str, files: &'static [ModelFile] },
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ModelFile { pub name: &'static str, pub bytes: u64 }

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ModelSpec {
    pub key: &'static str,      // "zh_en" / "en_zh"
    pub from: &'static str,
    pub to: &'static str,
    pub sources: &'static [ModelSource],
    pub size_mb: u64,           // 展示用约数（按主源）
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        key: "zh_en", from: "zh", to: "en", size_mb: 71,
        sources: &[
            ModelSource::Argos {
                file: "translate-zh_en-1_9.argosmodel",
                bytes: 74481402,   // argos-net.com 实测 content-length
            },
            ModelSource::Hf {
                repo: "ooeoeo/opus-mt-zh-en-ct2-float16",
                files: &[
                    ModelFile { name: "model.bin", bytes: 155502615 },
                    ModelFile { name: "shared_vocabulary.json", bytes: 1303998 },
                    ModelFile { name: "source.spm", bytes: 804677 },
                    ModelFile { name: "target.spm", bytes: 806530 },
                ],
            },
        ],
    },
    ModelSpec {
        key: "en_zh", from: "en", to: "zh", size_mb: 68,
        sources: &[
            ModelSource::Argos {
                file: "translate-en_zh-1_9.argosmodel",
                bytes: 70743021,
            },
            ModelSource::Hf {
                repo: "ooeoeo/opus-mt-en-zh-ct2-float16",
                files: &[
                    ModelFile { name: "model.bin", bytes: 155502615 },
                    ModelFile { name: "shared_vocabulary.json", bytes: 1303887 },
                    ModelFile { name: "source.spm", bytes: 806435 },
                    ModelFile { name: "target.spm", bytes: 804600 },
                ],
            },
        ],
    },
];

/// 按方向找内置模型。
pub fn find(from: &str, to: &str) -> Option<&'static ModelSpec>;
/// 按 key 找。
pub fn by_key(key: &str) -> Option<&'static ModelSpec>;
```

## 6. download.rs（多源下载器 + 统一安装布局 + Argos 包导入）

```rust
//! 模型下载与安装：argos-net 直链主源、HF（含镜像）备源；断点续传；
//! argos zip 与 HF 多文件统一安装为同一目录布局，engine 不感知来源。

pub struct Progress {
    pub file: &'static str,     // 正在下的文件名（argos 源为 .argosmodel 文件名）
    pub downloaded: u64,
    pub total: Option<u64>,
}
pub type ProgressFn<'a> = &'a (dyn Fn(Progress) + Send + Sync);

/// 镜像配置（UI 设置可注入；Default 为内置国内友好默认）。
#[derive(Debug, Clone)]
pub struct Mirrors {
    /// Argos 直链 base（尾不带 /）。默认 "https://argos-net.com/v1"
    pub argos_base: String,
    /// HF host 顺序。默认 ["huggingface.co", "hf-mirror.com"]
    pub hf_hosts: Vec<String>,
    /// GitHub 前缀顺序（当前版本供扩展资源用；空串元素 = 直连）。
    /// 默认 [""（直连), "https://ghfast.top", "https://gh-proxy.com", "https://ghproxy.net"]
    pub github_prefixes: Vec<String>,
}
impl Default for Mirrors {
    fn default() -> Self { /* 上述默认值写死 */ }
}

/// 安装 spec 到 `dir/<key>/`，统一目录布局（argos 与 HF 殊途同归）：
///   <key>/model.bin
///   <key>/shared_vocabulary.json
///   <key>/config.json          （argos 源有：CT2 加载配置 add_source_bos 等，必须保留）
///   <key>/source.spm           （argos 源：从包内 sentencepiece.model 复制）
///   <key>/target.spm
///   <key>/manifest.json
///
/// manifest.json 字段：{"key":..,"from":..,"to":..,"source":"argos"|"hf",
///                     "files":[{"name":..,"bytes":..},...],"installed_bytes":N}
/// （from/to 恒取 spec；导入包见 import_argosmodel。）
///
/// 流程：
/// 1. is_complete → 直接返回；
/// 2. 依次尝试 spec.sources（主源 argos）：
///    a. argos 源：单文件下载（URL = mirrors.argos_base + "/" + file，
///       校验 content-length == 声明 bytes，不符换下一源）到 `dir/<file>.part`
///       （Range 续传同下）；下载完成后 zip 解包：
///       - 在 zip 全树按文件名定位 model.bin / sentencepiece.model /
///         shared_vocabulary.* / config.json（实测布局：顶层 translate-xx-1_9/ 下，
///         但按名搜不假设层级）；
///       - 落盘为统一布局；model.bin 同目录的 config.json/shared_vocabulary 一并放；
///       - sentencepiece.model 写两份：source.spm 与 target.spm；
///       - stanza/、README、metadata.json 不落盘；
///       - zip 里的 model.bin 字节数记入 manifest；
///       - 校验：model.bin 或 vocabulary 缺失 → 报错换下一源。
///    b. HF 源：逐文件（model.bin/shared_vocabulary.json/source.spm/target.spm）
///       按 mirrors.hf_hosts 顺序下载（单文件级换源），URL =
///       https://{host}/{repo}/resolve/main/{file}；先比 content-length 与声明 bytes
///       （302 跳转后以最终响应为准），不符换 host/源。
/// 3. 所有文件就绪 → 写 manifest.json → 返回 dir/<key>。
/// 下载公共逻辑：`.part` 文件 + Range: bytes={len}- 续传（206 续传/200 重写），
/// 完成后 rename；每源连接超时 30s；progress 每 256KB 回调一次；
/// agent 配 `Proxy::try_from_system()`（失败忽略）+ redirects(5) + UA "rumi-translate/0.1"。
pub fn install(spec: &ModelSpec, dir: &std::path::Path, mirrors: &Mirrors,
               progress: ProgressFn<'_>) -> Result<std::path::PathBuf, String>;

/// manifest 存在 + files 各文件字节数相符。
pub fn is_complete(spec: &ModelSpec, dir: &std::path::Path) -> bool;

/// 导入本地 .argosmodel 包到 dir/<pkg>/（pkg 取 zip 内顶层目录名或文件名去扩展）。
/// 解包逻辑与 install 的 argos 分支完全一致（复用同一内部函数），
/// from/to 从包内 metadata.json 的 from_code/to_code 读（读不到置空串），
/// manifest 的 source 字段为 "argos"。返回 dir/<pkg>。
pub fn import_argosmodel(pkg: &std::path::Path, dir: &std::path::Path)
    -> Result<std::path::PathBuf, String>;
```

网络实现约束：`ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).redirects(5)`
  + `Proxy::try_from_system()`（失败忽略代理）。HF resolve 会 302 到 CDN，redirects=5 必须。
下载循环用 `resp.into_reader()` + 8KB buf 读。

单测（tests/download_tests.rs，需要 tempfile）：
- `is_complete`：临时目录摆好 manifest+文件/摆坏（缺文件、字节错）两态。
- **argos 解包**：测试里用 zip crate 现场构造一个最小 argosmodel 包（复刻真实布局：
  `translate-en_zh-1_9/metadata.json`（from_code:"en",to_code:"zh"）、
  `translate-en_zh-1_9/sentencepiece.model`（16B 假数据）、
  `translate-en_zh-1_9/model/model.bin`（16B）、`translate-en_zh-1_9/model/shared_vocabulary.json`、
  `translate-en_zh-1_9/model/config.json`、`translate-en_zh-1_9/stanza/resources.json`（应被忽略）），
  分别测 install 的 argos 分支（**用 file:// 假服务器不可行——改为把解包逻辑抽成
  `pub(crate) fn unpack_argos(zip_path:&Path, out:&Path) -> Result<(String /*from*/, String /*to*/), String>`
  纯本地函数直接测**）与 import_argosmodel（构造包落盘后走完整导入）。
  断言：统一布局五文件齐全、manifest from/to 正确、stanza 未落盘。

## 7. lib.rs（crate 门面 + 统一入口）

```rust
//! rumi-translate：离线（CTranslate2/ct2rs）+ 在线翻译。
pub mod download;
pub mod engine;
pub mod lang;
pub mod online;
pub mod postfix;
pub mod registry;
pub mod segment;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// 模型根目录：环境变量 RUMI_TRANSLATE_MODELS_DIR 优先，否则
/// dirs::data_dir()/rumi/translate/models（Linux: ~/.local/share/rumi/translate/models）。
pub fn models_dir() -> PathBuf;

/// 计算精度（离线引擎）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compute { Int8, Float32 }

/// 引擎选择（facade 层；在线子集与 online::OnlineEngine 一一对应）。
#[derive(Debug, Clone)]
pub enum Engine {
    Offline,
    Google, Microsoft,
    DeepL { api_key: String, pro: bool },
    DeepLX { url: String },
    OpenAI { base_url: String, api_key: String, model: String },
}

/// 统一翻译入口（UI/server/CLI 共用；from==to 直接原样返回；
/// texts 与返回值一一对应；空串原样返回空串）。
/// 离线：engine::shared() 懒加载模型（未安装报 "语言包未安装：<from>→<to>，请先在翻译页下载"）。
/// 在线：每段文本逐条调 online（多段由内部 join 逻辑决定，见 online 规格）。
/// polish=true 且 to=="zh" 时对每条结果过 postfix::polish_zh。
pub fn translate(texts: &[String], from: &str, to: &str,
                 engine: &Engine, polish: bool) -> Result<Vec<String>, String>;

pub use engine::shared as shared_engine; // 进程级单例（见 engine 规格）
```

`translate()` 的实现全部编码为「调用 engine/online 假想 API」；两模块此时是桩，编译在合并后通过（你本地 `cargo check -p rumi-translate` **允许因桩 body todo!() 而不通过**——只要错误全部位于 engine.rs / online/mod.rs 两个文件内，且你保证签名与规格逐字一致；core 自身模块 lang/segment/postfix/registry/download/lib 的 translate() 函数体必须语法完整（它引用的符号在桩里有声明即可）。**桩文件的 todo!() 会让 check 报 E0308? 不会——todo!() 是合法表达式**，因此整个 crate 应能 `cargo check` 通过：桩 body 用 `todo!()` 并 `#[allow(unreachable_code)]`。验证基准：`cargo check -p rumi-translate` 通过（含桩），`cargo test -p rumi-translate --test segment_postfix_tests --test download_tests` 全绿（不依赖 ct2rs 重编译的部分——若 ct2rs 编译太久（首次 10-20 分钟属正常），`cargo test -p rumi-translate --lib lang` 可先验证纯逻辑；最终门禁以全量为准）。

## 8. engine.rs 桩（你只写这些，工人 C 覆写）

```rust
//! 离线引擎（工人 C 实现）。此文件为接口桩。
use crate::Compute;
use std::path::PathBuf;
use std::time::Duration;

pub enum EngineError {
    ModelNotInstalled { from: String, to: String },
    NoBuiltinModel { from: String, to: String },
    Load(String),
    Infer(String),
}
impl std::fmt::Display for EngineError { /* todo!() */ }
impl std::error::Error for EngineError {}

pub struct OfflineEngine { _p: () }

impl OfflineEngine {
    pub fn new(models_dir: PathBuf, compute: Compute) -> OfflineEngine { todo!() }
    pub fn installed(&self) -> Vec<InstalledInfo> { todo!() }
    pub fn is_installed(&self, from: &str, to: &str) -> bool { todo!() }
    pub fn translate(&self, text: &str, from: &str, to: &str) -> Result<String, EngineError> { todo!() }
    pub fn unload_idle(&self, idle: Duration) { todo!() }
}

pub struct InstalledInfo { pub key: String, pub from: String, pub to: String, pub bytes: u64 }

/// 进程级单例：首次调用按 models_dir() + Compute::Int8 构造。
pub fn shared() -> &'static OfflineEngine { todo!() }
```

（Display impl 的桩 body 也用 todo!()； InstalledInfo derive Debug, Clone。）

## 9. online/mod.rs 桩（你只写这些，工人 B 覆写）

```rust
//! 在线翻译 providers（工人 B 实现）。此文件为接口桩。
pub mod deepl;
pub mod deeplx;
pub mod google;
pub mod microsoft;
pub mod openai;

#[derive(Debug, Clone)]
pub enum OnlineEngine {
    Google, Microsoft,
    DeepL { api_key: String, pro: bool },
    DeepLX { url: String },
    OpenAI { base_url: String, api_key: String, model: String },
}

/// 在线翻译：texts 与返回一一对应；from/to 是内部两位码（provider 内部自行映射）。
pub fn translate_online(texts: &[&str], from: &str, to: &str,
                        engine: &OnlineEngine) -> Result<Vec<String>, String> { todo!() }
```

（子模块文件 deepl.rs 等你**不创建**——B 创建；桩 mod.rs 声明了不存在的子模块会导致编译错误！
**修正**：桩阶段的 online/mod.rs 把 5 个 `pub mod` 行写成注释 `// pub mod deepl; ...`，由 B 解开。）

## 10. 测试

`translate/tests/segment_postfix_tests.rs`：§2/§3/§4 列出的全部用例（写死期望值）。
`translate/tests/download_tests.rs`：§6 的 is_complete 与 import_argosmodel 构造包用例。

## 11. 回报格式

最终回复列出：改动文件清单、cargo check/test 结果（原样粘贴关键行）、构造的 argosmodel 测试包结构、
歧义点清单（规格未覆盖处你做了什么保守选择）。
