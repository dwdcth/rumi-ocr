# 规格 F/G：模型删除 + 模型目录 + NLLB 多语言（2026-11-03 增量）

> 用户需求：①模型可删除；②提供可下载模型列表，支持中文↔其他语言。
> 监工已实测验证（2026-11-03）：NLLB ct2rs target-prefix 翻译可行——
> `/tmp/rt-nllb`（JustFrederik/nllb-200-distilled-600M-ct2-int8 解包：model.bin +
> shared_vocabulary.txt + config.json + sentencepiece.bpe.model 复制成 source.spm/
> target.spm），`translate_batch_with_target_prefix(&srcs, &vec![vec!["jpn_Jpan"]], &Default::default(), None)`
> 加载 276ms、zh→ja 译文正确（"デフォルトポートは8080です."）。
> 另实测：官方 argos 索引 index.json 共 100 包，**中文相关仅 zh↔en**；en↔ja/ko/fr/de/ru
> 等包在 argos-net.com 直链可下（links[0] 即直链）。

## 工人 F（translate crate + CLI）：文件集

只动：`translate/src/catalog.rs`（新建）、`registry.rs`、`download.rs`、`engine.rs`、
`src/bin/rumi_translate_cli.rs`、`translate/tests/catalog_tests.rs`（新建）、
`tests/download_tests.rs`（可加用例）。禁止动 lib.rs（除 §1 标注的两行）、online/、ui/、server/。

### 1. registry.rs：新增 NLLB 条目与 ModelFile 别名

```rust
/// 文件落盘别名：下载名 name → 落盘名 as_（None=同名）。NLLB 的
/// sentencepiece.bpe.model 需要→source.spm+target.spm 两份，第二份用 extra 复制表示。
pub struct ModelFile { pub name: &'static str, pub bytes: u64, pub as_: Option<&'static str> }
// 现有 4 处 ModelFile 字面量补 as_: None。
pub struct ModelSpec { /* 现有字段不动 */ pub kind: ModelKind }
pub enum ModelKind { Marian, Nllb }
// 现有 zh_en/en_zh 条目 kind: Marian；新增：
ModelSpec {
    key: "nllb_600m", from: "*", to: "*", kind: ModelKind::Nllb, size_mb: 593,
    sources: &[ModelSource::Hf {
        repo: "JustFrederik/nllb-200-distilled-600M-ct2-int8",
        files: &[
            ModelFile { name: "model.bin", bytes: 622595991, as_: None },
            ModelFile { name: "sentencepiece.bpe.model", bytes: 4852054, as_: Some("source.spm") },
            ModelFile { name: "sentencepiece.bpe.model", bytes: 4852054, as_: Some("target.spm") },
            ModelFile { name: "shared_vocabulary.txt", bytes: 2568098, as_: None },
            ModelFile { name: "config.json", bytes: 159, as_: None },
        ],
    }],
},
// find/from,to 语义不变（"*" 不参与方向匹配；by_key("nllb_600m") 可得）。
// lib.rs 仅加两行：pub use registry::ModelKind;（若 lib.rs 无此 re-export 区则放 registry pub 顶格，不动 lib.rs）
```

### 2. download.rs：remove + NLLB 落盘 + 目录条目安装

```rust
/// 删除已装模型：key 校验 ^[a-z0-9_]+$（防路径穿越），删 dir/<key>/ 整目录
/// （含 .part）。目录不存在 → Ok（幂等）。返回被删目录 PathBuf。
pub fn remove(key: &str, dir: &Path) -> Result<PathBuf, String>;

/// 远程目录条目（来自官方 argos index.json，已在调用方过滤内置两包）。
pub struct CatalogEntry {
    pub key: String,       // 如 "en_ja"（from_to）
    pub from: String, pub to: String,
    pub version: String,   // 如 "1.1"
    pub url: String,       // 完整直链（index.json links[0]，实测即 argos-net）
}
/// 拉官方索引：依次试 mirrors.github_prefixes（空串=直连）拼
/// {p}https://raw.githubusercontent.com/argosopentech/argospm-index/main/index.json，
/// 解析 JSON 数组（字段 package_version/argos_version/from_code/to_code/from_name/
/// to_name/links/code），过滤 key ∈ {"zh_en","en_zh"}（内置重复）与解析失败的条目；
/// 失败换下一前缀；全挂返回 Err。**不打网单测**：解析逻辑抽 pub(crate)
/// fn parse_index(text: &str) -> Vec<CatalogEntry> 纯函数直测（fixture 写死 3 条）。
pub fn fetch_catalog(mirrors: &Mirrors) -> Result<Vec<CatalogEntry>, String>;

/// 安装目录条目（单 zip 直链 → 统一布局，与 install 的 argos 分支同款内部函数复用）。
/// key/from/to 取 entry；manifest source="argos"。
pub fn install_catalog_entry(entry: &CatalogEntry, dir: &Path, mirrors: &Mirrors,
                             progress: ProgressFn<'_>) -> Result<PathBuf, String>;
```

HF 分支改动：落盘名用 `f.as_.unwrap_or(f.name)`（同 repo 同名两份 = 下载一次写两份：
**同 name+bytes 的文件只下载一次**，落盘两份——按 name 去重缓存到内存再复制）。
manifest 写入增加 `"kind"` 字段：Marian→"marian"、Nllb→"nllb"（argos 包恒 "marian"）。
is_complete 对 NLLB 条目同样 manifest 驱动 ✓（files 含 source.spm/target.spm 两行）。

### 3. engine.rs：NLLB 支持 + unload_key

```rust
/// 从缓存摘除指定 key（NLLB 用 "nllb_600m"）；正在翻译中的调用持有 Arc 不受影响。
/// 返回是否确实摘除。
pub fn unload_key(&self, key: &str) -> bool;
```

- `Loaded` 增加 `nllb: bool`（manifest kind=="nllb" 或 key=="nllb_600m"）。
- **resolve 顺序**：registry 精确方向（含导入包）→ 都无 → `registry::by_key("nllb_600m")`
  且 is_complete 且 `nllb::code(to).is_some()` → `Resolution::NllbFound`（新增变体）；
  否则 NoBuiltinModel。from==to 已在方法入口挡掉。
- `translate`：nllb 分支用
  `translate_batch_with_target_prefix(&sources, &vec![vec![nllb_code]; n], &Default::default(), None)`
  （每段同一目标语言码）。新增模块级：
  ```rust
  /// 内部两位码 → NLLB 语言码（Flores-200）。None = NLLB 不支持该目标语言。
  pub fn nllb_code(lang: &str) -> Option<&'static str>;
  // zh→zho_Hans, en→eng_Latn, ja→jpn_Jpan, ko→kor_Hang, fr→fra_Latn, de→deu_Latn,
  // es→spa_Latn, ru→rus_Cyrl, pt→por_Latn, it→ita_Latn, vi→vie_Latn, ar→arb_Arab, th→tha_Thai
  ```
  （放 engine.rs 顶部，pub 供 UI 提示"NLLB 不支持该语言"。）
- NLLB 的段分隔：to=="zh" 用 ""，其余（含日/韩）用 " "（现有 seg_sep 逻辑已如此，确认即可）。
- `installed()`：NLLB 条目 key/from/to 用 "*"/"*"→from/to 显示字段填 "zh↔多语言"？**不改结构**：
  InstalledInfo.from="*"、to="*"，UI 自行渲染；bytes 照 manifest。

### 4. CLI

- `--remove <key>`：download::remove + 提示已删（与 --install 对称的管理子命令）。
- `--catalog`：拉 fetch_catalog 列表（key/方向/版本），失败报错退出 1；纯列表不装。
- `--list-models`：NLLB 行显示 `nllb_600m 多语言(zh↔200语言) 约593MB 已安装/未安装`。

### 5. 测试（catalog_tests.rs + 增补 download_tests.rs）

- parse_index fixture（3 条：正常/缺 links 跳过/zh_en 过滤）。
- remove：正常删（tempdir 摆目录）、key 穿越（"../x" → Err）、不存在（Ok 幂等）。
- nllb_code：zh/en/ja 各一 + 未知 None。
- ModelFile 别名落盘：在现有 HF 安装测试思路不可打网——抽 `fn hf_dest_name(f: &ModelFile) -> &str` 纯函数 + NLLB spec by_key 断言（5 文件、两 spm 行）。
- e2e（#[ignore]）：install nllb_600m（HF 622MB）→ zh→ja 翻译含 "8080"。

## 工人 G（UI）：文件集

只动：`ui/src/translate_ui.rs`、`ui/src/main.rs`（仅错误文案一处，见 §7）、
`ui/assets/i18n/{zh-CN,en-US}.json`。禁止动 settings.rs/其他 crate。

### 6. 模型块改造（translate_ui.rs）

1. 内置行渲染：`registry::MODELS` 遍历——kind==Nllb 的行显示
   `多语言（中文↔200 语言）`，from/to 显示 `*` 时按此文案（key=nllb_600m）。
2. **每行已安装 → [删除] btn-sm**（红色文字，i18n `translate.models.delete`）：
   std 线程调 `engine::shared().unload_key(key)` + `download::remove(key, &models_dir())`
   （先 unload 再 remove），完成后 refresh_installed；失败红字（`translate.models.delete_failed {p}`）。
   下载中（busy_key 匹配）的行禁删。
3. **[更多语言包…] btn-sm**（放在「离线语言包」小节头右侧、原「导入语言包…」旁）：
   展开/收起一个目录子区：
   - 首次展开 std 线程 `download::fetch_catalog(&mirrors_from_settings(&st))` → 列表
     （每行：`display(from)→display(to)`、版本、[下载]；已安装（key 在 installed 列表）的行显示
     已安装 ✓ 无按钮）。方向 display 用 `lang::display`（未知码原样，如 "eo"→"eo"）。
   - 拉取中显示 `translate.models.fetching`；失败显示 `translate.models.fetch_failed {p}` + [重试]。
   - [下载] 走 `install_catalog_entry`，进度条复用现有 prog 信号（busy_key 用 entry.key）。
   - 目录行数可能上百：子区 max-height 200px overflow-y:auto（内联样式即可）。
4. 语言下拉已有 13 语种不动。NLLB 不支持的目标语言（如用户后续扩展码）由引擎层报错，
   UI 不前置过滤（from/to 候选 = lang::UI_LANGS 全集）。

### 7. 词条（zh / en，追加两个 json）

| key | zh | en |
| --- | --- | --- |
| translate.models.delete | 删除 | Delete |
| translate.models.deleting | 删除中… | Deleting… |
| translate.models.delete_failed | 删除失败：{p} | Delete failed: {p} |
| translate.models.nllb_label | 多语言（中文↔200 语言） | Multilingual (Chinese ↔ 200 langs) |
| translate.models.more | 更多语言包… | More packs… |
| translate.models.fetching | 正在获取目录… | Fetching catalog… |
| translate.models.fetch_failed | 目录获取失败：{p} | Catalog failed: {p} |
| translate.models.retry | 重试 | Retry |
| translate.models.version | 版本 {p} | Version {p} |

（{p} 插值沿用现有 replace 方案。）

### 8. 验收

- F：`cargo test -p rumi-translate` 全绿（原 72+ 新增 ≥8）；`cargo check -p rumi-translate`。
- G：`cargo check -p rumi-ui`、`cargo test -p rumi-ui`（67+1 不回归 + 新纯函数测试：
  目录行渲染数据组装 `fn catalog_rows(entries, installed) -> Vec<...>` 抽纯函数测 3 条）。
- 监工复验：UI 删除 en_zh → 重新下载；更多语言包展开 → 下载 en_ja → 翻译 en→ja；
  NLLB 安装 → zh→ja 翻译。
