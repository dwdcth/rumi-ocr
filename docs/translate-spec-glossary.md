# 规格 N：术语表（glossary，#2，2026-11-04）

> 需求：长文档同一术语前后译法不一致（"thread pool"→线程池/执行绪混用）。
> v1 范围（写死）：**文本域存储 + 精确匹配替换 + LLM prompt 注入**；
> 术语自动提取、模糊替换、翻译页文本框应用 = v2 不做。

## 设计

### 1. 存储（设置页，textarea 最低成本形态）

`TranslateSettings` 加 `pub glossary: String`（默认空）。格式：一行一条
`原文 = 译文`（`=` 两侧空格容忍；`#` 开头行 = 注释；空行忽略；解析失败的
行静默跳过并在 doc 注释说明——编辑中态不该炸应用）。

### 2. translate 侧新模块 glossary.rs

```rust
//! 术语表：设置文本域 → 结构化条目 + 应用策略。
pub struct GlossaryTerm { pub src: String, pub dst: String }
pub struct Glossary { pub terms: Vec<GlossaryTerm> }
impl Glossary {
    /// 解析设置文本域（"原文 = 译文" 行式；# 注释/空行/无 = 的行跳过）。
    pub fn parse(text: &str) -> Self;
    /// 单元文本精确匹配某术语（trim 相等）→ 直接用术语译文（跳过引擎）。
    pub fn exact_match(&self, unit_text: &str) -> Option<&str>;
    /// 非空且有条目。
    pub fn is_empty(&self) -> bool;
    /// LLM system prompt 注入段（openai 引擎用）：
    /// "\n\n术语表（翻译时必须遵循以下固定译法）：\n- src → dst\n…"
    pub fn prompt_block(&self) -> String;
}
```

### 3. docmt 应用（顺序插入，改循环的判定链）

现有链：防幻觉 → running memo → 缓存 → 引擎。**术语精确匹配插在防幻觉之后**
（术语命中跳过一切成本）：

```rust
let text = if !unit.has_translatable_text() {
    unit.text.clone()
} else if let Some(dst) = glossary.exact_match(&unit.text) {
    glossary_hits += 1;
    dst.to_string()
} else if unit.running && running_memo.contains_key(...) { ...
```

- `glossary: Glossary` 由 opts 传入：`DocTranslateOptions` 加
  `pub glossary: Glossary`（Glossary: Clone；默认 `Glossary::default()` 空）。
  **构造点影响**：tests 的 use_cache:false 构造点要补字段——全部补
  `glossary: Glossary::default()`；bin/cli 与 ui 的构造点由监工补（同 L 轮
  先例），回报注明。
- DocStats 加 `pub glossary_hits: usize`。
- 缓存交互：精确命中不进缓存不改缓存（术语表变化时不留脏缓存——因为
  精确匹配在缓存查询之前，改术语立即生效 ✓ 这个顺序别改）。

### 4. LLM prompt 注入（特批：允许动 translate/src/online/openai.rs）

`openai.rs` 的 build（或 build_with_names）加术语段：system prompt 末尾拼
`glossary.prompt_block()`（非空时）。参数传递路径：`docmt` 的循环里
`Engine::OpenAI{..}` 分支拿不到 opts 之外的东西——**facade 不动**，docmt 在
调用 crate::translate 前对 OpenAI 引擎特判？不优雅。**正确做法**：
`crate::Engine` 枚举不动；`online::OnlineEngine::OpenAI` 结构体加
`glossary: Vec<(String,String)>` 字段（默认空）+ facade `Engine::OpenAI`
加 `glossary: Vec<(String, String)>` 字段（默认构造点补 vec![]）——
字段扩散两处构造点（ui settings/engine_from_settings、cli），监工补。
**docmt 循环**：构造 facade Engine 时若是 OpenAI 且 glossary 非空，
克隆引擎枚举并填 glossary（一次性，循环外）。
- google/microsoft/deepl 不注入（无 prompt 通道），精确匹配已覆盖关键场景。

### 5. UI（设置页翻译小节）

- `settings.rs`：字段 + Default（空串）。
- `main.rs` 设置小节：textarea（label=术语表，placeholder=
  `一行一条：原文 = 译文（# 注释）`，6 行高，onblur 存——与镜像输入框同款）。
- 词条：settings.translate.glossary（术语表）/ glossary_ph（placeholder）。
- 翻译页文档翻译无需改动（glossary 从 settings 自动带入 docmt opts——
  start_doc_translate 构造点补 `glossary: Glossary::parse(&st.glossary)`）。

### 6. 测试

- glossary.rs 单测 ≥8：parse（正常/注释/空行/无=/多空格/exact 大小写敏感
  trim 相等）、exact_match、prompt_block 格式、empty。
- docmt 集成 1 条：from==to 直通 + glossary 精确命中
  （text="thread pool"，glossary 给 "thread pool = 线程池"，断言输出该单元
  译文=="线程池"且 glossary_hits==1——from==to 直通路径 glossary 也在
  防幻觉之后先命中？直通是 facade 层 from==to 短路……docmt 循环里直通不
  短路（facade translate 处理），glossary 精确匹配在其之前 ✓ 可测）。
- 既有测试构造点补字段后全绿。

## 文件集

工人 N 动：translate/src/glossary.rs（新）、docmt.rs（判定链+stats）、
lib.rs（mod + Engine::OpenAI 字段）、online/openai.rs（prompt 段）、
tests/{glossary_tests.rs 新, docmt_tests.rs 补字段}、ui/settings.rs、
ui/src/main.rs（设置小节 textarea）、ui/src/translate_ui.rs（start_doc_translate
构造点）、ui/assets/i18n/*（2 词条）。
监工后补：bin/cli 的两处构造点。

## 验收

cargo test -p rumi-translate 全绿（+≥9）；ui 基线不回归；CLI 冒烟
（glossary 生效：txt 含术语 → .md 输出含固定译法）留监工。
