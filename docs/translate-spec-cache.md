# 规格 L：文档翻译缓存/断点续翻（2026-11-04）

> 需求 #3：大文档翻译中断/重跑全部重来 = 浪费；PDF 页眉页脚同文本重复翻 N 遍。
> 核心设计：**缓存键 = 文本 hash（非单元 id）**——同文本跨单元/跨文档/跨页眉页脚
> 只翻一次，一石二鸟。docforge 侧的页眉页脚标记（用户在做）是解析层优化，
> 与本缓存互补不冲突。

## 工人 L 文件集

只动：`translate/src/docmt.rs`、`translate/src/cache.rs`（新建）、
`translate/src/lib.rs`（仅 `pub mod cache;` 一行）、`translate/tests/cache_tests.rs`（新建）。
禁止动：engine/online/download/segment/postfix/registry、bin/、ui/、server/。

## 1. cache.rs（纯逻辑层，全部可单测）

```rust
//! 文档翻译缓存：文本 hash 键值存储（JSON 落盘，LRU 条数上限）。

/// 缓存键：blake3/sha 不引新依赖——std DefaultHasher（SipHash）足够
/// （缓存是性能优化非安全边界，碰撞最坏一次坏译文重翻即可）。
/// key = hash64(text) 的 hex + "|" + from + "|" + to + "|" + engine_tag + "|" + polish
/// engine_tag：Engine 的判别标识（"offline" / "google" / "deepl:<keyhash8>" /
/// "deeplx:<urlhash8>" / "openai:<base><model><keyhash8>"）——配置变则 miss，
/// 保守正确。pub(crate) fn engine_tag(e: &crate::Engine) -> String。
pub struct DocCache {
    entries: std::collections::HashMap<String, String>, // key -> 译文
    path: std::path::PathBuf,
}

impl DocCache {
    /// 缓存文件路径：`~/.local/share/rumi/translate/doc-cache.json`
    /// （RUMI_TRANSLATE_MODELS_DIR 同源规则不适用——缓存独立于模型，用
    /// 环境变量 RUMI_TRANSLATE_CACHE 覆盖，缺省数据目录）。
    pub fn default_path() -> std::path::PathBuf;

    /// 加载（文件不存在/损坏 JSON → 空缓存，不报错——缓存损坏不该阻断翻译）。
    pub fn load() -> Self;

    /// 查询。命中返回译文克隆。
    pub fn get(&self, text: &str, from: &str, to: &str, engine: &crate::Engine, polish: bool) -> Option<String>;

    /// 写入（同时维护 LRU：插入后若 entries.len() > MAX_ENTRIES(5000)，
    /// 按插入序淘汰最旧 25%——HashMap 无序，需另持 Vec<key> 插入序或用
    /// 简单策略：超限时全量重建保留后 3750 条（再简单点：直接清空重建，
    /// 注明取舍——**采用清空策略**，代码最简且缓存语义无损）。
    pub fn insert(&mut self, text: &str, from: &str, to: &str, engine: &crate::Engine, polish: bool, translation: &str);

    /// 落盘（原子写：先写 doc-cache.json.part 再 rename）。目录不存在则建。
    /// 序列化失败静默忽略（缓存尽力而为）。
    pub fn save(&self);
}
```

JSON 形态：`{"version": 1, "entries": {"<key>": "<译文>", ...}}`。

## 2. docmt.rs 集成

```rust
pub struct DocTranslateOptions {
    // ...现有字段不动
    /// 翻译缓存（默认 true）：文本级命中跳过引擎；进度照常推进。
    /// from==to 的直通路径不经过缓存（无引擎成本，无缓存意义）。
    pub use_cache: bool,
}
// 所有现有构造点（CLI/UI/测试）补 use_cache: true —— 调用方文件你不动，
// 故 DocTranslateOptions 提供不带 use_cache 的场景怎么办：**给字段加
// #[doc(hidden)] 不行**——直接要求：本 crate 内 bin/tests 的构造点你也不改
// （禁止动 bin/），所以把默认值问题解决在类型层：
// 改为 pub use_cache: bool 不行会破编译。方案：**新增构造函数**
// `DocTranslateOptions::with_defaults(engine, from, to, strategy, polish) -> Self`
// （use_cache: true, min_chars: 2, on_progress: None），现有字段 pub 保持——
// bin/tests 用结构体字面量会缺 use_cache 编译错！矛盾。
// **最终方案**：字段带默认值不可行（struct 无默认），改为在
// translate_document 内部读 opts.use_cache —— 字段必须存在。
// 你允许的文件里 tests/docmt_tests.rs 大量构造点需要补字段——新增
// `..docmt::doc_opts_default(engine, from, to)` 辅助或直接让测试构造点补
// `use_cache: false`（测试不落盘缓存，见下）。
```

**决策（写死，工人不再纠结）**：`DocTranslateOptions` 加 `pub use_cache: bool`；
本 crate 的 `tests/docmt_tests.rs` 既有构造点全部补 `use_cache: false`（测试不碰
用户缓存目录）；`bin/rumi_translate_cli.rs` 你禁动——它会在编译时缺字段报错，
**这是预期**：本项由监工在合并后补 CLI 一行（`use_cache: true`），工人在回报
里注明"CLI 待监工补"即可。UI 的 start_doc_translate 同理（监工补）。

translate_document 循环改动：

```rust
let mut cache = if opts.use_cache { Some(cache::DocCache::load()) } else { None };
let mut cache_hits = 0usize;
for (done, unit) in units.iter().enumerate() {
    let text = if !unit.has_translatable_text() {
        unit.text.clone()                       // 防幻觉直通（不进缓存）
    } else if let Some(hit) = cache.as_ref()
        .and_then(|c| c.get(&unit.text, &opts.from, &opts.to, &opts.engine, opts.polish)) {
        cache_hits += 1;
        hit                                      // 命中
    } else {
        let t = crate::translate(...)?;
        if let Some(c) = cache.as_mut() {
            c.insert(&unit.text, &opts.from, &opts.to, &opts.engine, opts.polish, &t);
            // 断点续翻：每批边界落盘一次（崩溃最多丢一批）
            if done % UNITS_PER_BATCH == 0 { c.save(); }
        }
        t
    };
    // ...进度回调照常（命中也推进——进度是"处理进度"不是"翻译成本"）
}
if let Some(c) = cache.as_ref() { c.save(); }   // 收尾落盘
```

DocStats 加 `pub cache_hits: usize`（UI/CLI 展示"缓存命中 n/N"待后续，字段先记）。

## 3. 测试（cache_tests.rs）

- 纯逻辑（用 RUMI_TRANSLATE_CACHE 指到 tempdir——**env var 在测试并行下有竞态**：
  DocCache::load() 读默认路径不接 env 的话测试会碰真实用户缓存！**修正**：
  `DocCache::load_at(path)` / `save` 公开，`load()` = `load_at(default_path())`；
  测试全用 load_at(tempdir)。env var 不引入。）
  - load_at 不存在文件 → 空；损坏 JSON → 空
  - get/insert roundtrip：同文本+同参数命中；任一参数不同（from/to/engine/polish）miss
  - engine_tag：offline 稳定；deepl key 不同 → tag 不同
  - 清空策略：MAX_ENTRIES 用测试钩子（`#[cfg(test)] const 可调` 或插入
    MAX+1 条断言 len 回到 0——按实现取简）
  - save→load roundtrip 持久
- docmt 集成（use_cache: false 的既有用例不受影响 + 一个 true 的新用例）：
  from==to 直通 + use_cache:true + RUMI... 不适用（load() 默认路径！）——
  **集成层不测 true 路径的落盘**（会写用户真实缓存目录，测试不可以）；
  改为 translate_document 加 `#[doc(hidden)] pub cache_path: Option<PathBuf>`
  字段？字段膨胀。**最简**：translate_document 里 `DocCache::load()` 换成
  `DocCache::load_at(cache_path_override().unwrap_or_else(DocCache::default_path))`，
  `fn cache_path_override() -> Option<PathBuf>` 读 env RUMI_TRANSLATE_CACHE
  （运行时读，无 unsafe set_var；测试用 `std::process::Command` 起子进程太重——
  **集成用例标 #[ignore] 注明手动跑**，或用 Command 起同 crate bin 不可行。
  决策：集成命中用例 #[ignore]，纯逻辑层覆盖足够）。

## 4. 验收

- cargo test -p rumi-translate 全绿（cache_tests 新增 ≥7；docmt_tests 既有全过）
- cargo check -p rumi-translate 0 warning
- 回报注明：CLI/UI 待监工补的构造点清单

## 5. 回报格式

文件清单、MAX_ENTRIES 清空策略的最终实现、测试关键行、歧义点。
