# 纯 Rust SentencePiece 对拍验证报告

**日期**：2026-11-03
**目的**：验证「纯 Rust SentencePiece 实现」与「官方 C++ 实现」的分词等价性，判定能否替换 rumi 中的 C++ `sentencepiece-sys`（ct2rs 的 `sentencepiece` feature），从而根治与 ORT（内嵌 protobuf 3.14）的双 protobuf 链接冲突。
**方法**：golden 由 Python 官方版（C++ 绑定 wheel）离线生成；Rust 侧为独立对拍程序，**不引入任何 C++ sentencepiece 依赖**（避免把双 protobuf 冲突带进测试环境）。

---

## 0. 结论速览

| 判定 | 内容 |
|---|---|
| **最终判定** | **✅ 可直接换**：推荐 `sentencepiece-rs` 0.2.2（纯 Rust，直接加载 .spm，三模型 ids/pieces/decode 全量 100% 一致） |
| 备选 | `sentencepiece-rust` 0.1.1（ids/decode 100%，但 API 不暴露 unk 表面 piece，不适合 ct2rs 的字符串 piece 接口）；HF `tokenizers` + tokenizer.json 离线转换路线（ids 100%，但需转换步骤且 decode 约定不同） |
| 排除 | `splintr`（.spm 不可直载；tokenizer.json 路线有 1 条空白处理差异）；C++ 绑定 `sentencepiece` 0.14（即 danieldk/sentencepiece，内嵌 protobuf，本就要替换的对象） |

**一句话**：`sentencepiece-rs` 在 2 套语料（29 条固定 + 219 条压力）× 3 条模型轨道 × Unigram/BPE 双模型类型上，ids 序列、pieces 序列、ids-decode、pieces-decode 四项全部逐条全等，含 unk 表面文本、中文标点、emoji ZWJ、全角/NFKC、控制字符、纯空白、超长文本等边角；可以按第 5 节方案接入 rumi。

---

## 1. 选型调研

### 1.1 候选清单（crates.io 检索 + 指定评估项）

| crate | 版本 | 最后发布/commit | 下载量 | 性质 |
|---|---|---|---|---|
| `sentencepiece`（danieldk） | 0.14.0 | 2026-07-26 | 985k | **C++ 绑定**（sentencepiece-sys，内嵌 protobuf-lite）——排除对象本体 |
| **`sentencepiece-rs`**（Rayato159/sentencepiece-rs） | 0.2.2 | 2026-05-20 | 57k（近期 46k） | 纯 Rust 运行时重实现 |
| **`sentencepiece-rust`**（VoiceLessQ/sentencepiece-rust） | 0.1.1 | 2026-06-25 | 11k（近期 9.4k） | 纯 Rust 从零移植，自称与官方差分验证过 |
| **`splintr`**（ml-rust/splintr） | 0.22.0 | 2026-10-12（repo 最后 push 2026-11-01） | 42k | 纯 Rust 多后端 tokenizer（BPE/SPM/WordPiece），63 stars，最活跃 |
| **`tokenizers`**（huggingface） | 0.23.2 | 2026-10-19 | 18.6M | HF 官方，原生吃 tokenizer.json |
| `kitoken` | 0.11.1 | 2026-10-26 | 49k | 纯 Rust，可读 .spm，但 README 自述两处**故意偏差**（nmt_nfkc 的 `～` 规则、Unigram 同分合并顺序向 HF Tokenizers 看齐而非 SP）——对拍目标就是逐位一致，故不选 |
| `sentencepiece-model` | 0.1.4 | 2024-10-08 | 819k | 仅 proto 解析器，无 encode——不满足 |

**谱系查证**（协调方指定核查项）：
- `vgarleanu/sentencepiece-rs` 仓库已 **404**（删除/消失），无法验证继任关系。
- crates.io 上 `sentencepiece`（C++ 绑定）的 repository 指向 `danieldk/sentencepiece`，与 vgarleanu 无关。
- `splintr` 出自 ml-rust 组织（rust-tokenizers 一系），其 ".spm" 支持是**自有文本行格式**（每行 `base64(piece) score type`），**不是** SentencePiece protobuf——见 1.2。它不是 `sentencepiece-rs`（Rayato159）的继任：Rayato159 与 VoiceLessQ 两个 crate 均为 2026 年独立新作（VoiceLessQ 的 README 明确声明与 sentencepiece-rs 无衍生关系）。

### 1.2 支持矩阵（全部经实际编译/加载验证，非仅看 README）

| 能力 | sentencepiece-rs 0.2.2 | sentencepiece-rust 0.1.1 | splintr 0.22.0 | tokenizers 0.23.2 |
|---|---|---|---|---|
| .spm（protobuf）直接加载 | ✅ `SentencePieceProcessor::open(path)` | ✅ 同名 API | ❌ 只吃自有文本格式 / GGUF / tokenizer.json / tiktoken | ❌ 只吃 tokenizer.json |
| Unigram | ✅（en_zh 模型实测） | ✅ | ✅（经 tokenizer.json） | ✅（经 tokenizer.json） |
| BPE | ✅（nllb 模型实测） | ✅ | ✅（经 tokenizer.json，转 HF BPE） | ✅（经 tokenizer.json，转 HF BPE） |
| precompiled_charsmap normalizer | ✅（内置 Darts trie 重放） | ✅（同左） | ✅（NormOp::Precompiled，经 tokenizer.json） | ✅（normalizers.Precompiled） |
| byte_fallback | API 支持 encode/decode byte piece（本次两个模型 proto 均为 `byte_fallback=false`，**未获真实数据覆盖**，README 声明支持） | 同左 | ✅（tokenizer.json 声明式） | ✅ |
| encode → pieces（含 unk 表面） | ✅ `encode()` | ❌ 只返回 ids，`id_to_piece(unk)` 只给 `"<unk>"` | ❌ Backend 无 id_to_token | ✅ `get_tokens()`（unk 显示为 `"<unk>"` 而非表面文本） |
| encode → ids | ✅ `encode_to_ids()` | ✅ `encode()` | ✅ | ✅ |
| decode(ids) / decode(pieces) | ✅ / ✅ | ✅ / ❌ | ✅（默认 skip_specials）/ ❌ | ✅ / ❌ |
| 零 C++ 依赖 | ✅（无 sentencepiece-sys） | ✅（全 crate 零依赖） | ✅ | ✅ |
| 维护活跃度 | 2026-05 发布，单一维护者，2 stars | 2026-06 发布，单一维护者，1 star | 非常活跃（2026-11-01 仍有 push） | HF 官方，极活跃 |

### 1.3 HF `tokenizers` 路线的转换可行性（指定核查项）

- Rust 端 `tokenizers` crate **没有** `from_sentencepiece` 类 API（只认 tokenizer.json）。
- 离线转换路线**验证成立**：用 Python `transformers` 5.22.1 的 `SpmConverter`（`SentencePieceExtractor` 解析 proto + `Precompiled` charsmap + `Metaspace` pre-tokenizer）生成 tokenizer.json，Unigram 模型（en_zh）走官方 `converted()` 全流程；**BPE 模型（nllb）踩中 transformers 5.22 的 bug**——`SpmConverter.tokenizer()` BPE 分支以旧签名调用 `SentencePieceExtractor.extract()`（传 `vocab_scores` 而非 model 类，`AttributeError: 'list' object has no attribute '__name__'`）。本次按其源码意图手工组装绕过（`extract(models.BPE)` + `generate_merges` + 同套 normalizer/pre_tokenizer/decoder）。转换产物由 Rust `tokenizers` 0.23.2 加载对拍，ids 一致率 100%（见第 3 节）。
- 该路线接入 rumi 的形态：ct2rs 的 `tokenizers` feature（`Translator<ct2rs::tokenizers::hf::Tokenizer>`），或同样自实现 trait。与直载路线的差异：① 需要一次性离线转换产物随模型分发（模型目录多一个 tokenizer.json，且 auto 探测优先级在 spm 之前，见 ct2rs `auto.rs`）；② ct2rs 的 hf 实现不自动追加 `</s>`（sentencepiece 实现会追加），需自理；③ unk 的 decode 渲染为 `"<unk>"` 而非 SP 的 `" ⁇ "`（对翻译出口文本是可见差异，rumi 引擎出口的 ▁ 清理逻辑之外还要再处理 unk 文本）。

### 1.4 推荐

按判据优先级（① 三模型 ids 一致率 → ② .spm 直载免转换 → ③ 维护活跃度）：

1. **`sentencepiece-rs` 0.2.2**：① 100%（唯一连 pieces 都 100% 的候选）② ✅ 直载 ③ 中（新 crate、单一维护者——用锁定版本 + 保留对拍脚本对冲）。
2. `sentencepiece-rust` 0.1.1：① 100% ② ✅ ③ 中；但 encode 只出 ids、无 pieces-decode，**不能直接满足 ct2rs trait 的字符串 piece 契约**（CTranslate2 侧靠 piece 字符串映射词表，unk 表面文本必须保真）。
3. `tokenizers` + 转换：① 100% ② ❌ ③ 高；转换链多一环 + decode 约定差异，作为回退路线。
4. `splintr`：① 99.8%（248 条中 1 条空白处理差异）② ❌ ③ 最高；空白差异属可修的小 bug，但无直载能力，综合不敌 1。

---

## 2. 对拍方法

### 2.1 环境

- Python 3.14.8；venv 内 `sentencepiece==0.2.2`（**cp314 manylinux wheel 存在**，直接安装，无需源码编译）、`transformers==5.22.1`、`tokenizers==0.23.3`、`protobuf`。
- rustc 1.99.0 / cargo 1.99.0；crates 经 USTC 镜像拉取，全部一次编译通过。
- 对拍程序：`/tmp/spm-parity/rs-check/`（独立 bin，依赖四个候选 crate + serde_json，**不含 ct2rs/sentencepiece-sys**）。

### 2.2 语料（`/tmp/spm-parity/corpus.txt`，29 条，一行一条）

a. 中文日常 5（含「」“”《》等中文标点）；b. 英文技术 5（URL、`192.168.1.100:8080`、`/usr/local/...` 路径、驼峰/下划线标识符、`v1.2.3-beta.4`）；c. 日文 3；d. 中英混排 3；e. emoji 3（😀👨‍👩‍👧‍👦🇨🇳）；f. 全角/半角 2（`１２３ＡＢＣ`）；g. NFKC 边角 2（㍿ ㈱ ﹒ ㎡ ㌔㍍）；h. 连续空白 2（多空格 / tab）；i. 空串 1 + 纯空白 1（哨兵行 `[[EMPTY]]`/`[[WHITESPACE]]`→`" \t "`，双端同规则解码）；j. 超长中文 1（>5000 字符）；k. 控制字符 1（`\u0000\u0001\u007f` 原始字节）。

**压力语料 corpus2.txt（219 条）**：上述全部 + 连续标点串（`....` `————` `。。。。` 等，Unigram tie-break 已知弱点）+ 11 种文字系统真句 + 数字/日期/十六进制 + NFC/NFD/连字 + 各式引号货币符 + emoji 肤色/旗帜/keycap/ZWJ + NBSP/U+3000/ZWSP/BOM/首尾空白 + 文字系统边界粘连 + 200 字符长单词 + **词表汤**（从两个模型词表随机采样 piece 拼接，固定 seed=42，覆盖在分布内输入）。

### 2.3 模型（只读 rumi 模型目录）

| 轨道 | 路径 | 类型 | 说明 |
|---|---|---|---|
| en_zh_source | `~/.local/share/rumi/translate/models/en_zh/source.spm` | Unigram（trainer.model_type=1，byte_fallback=false） | 32000 词表 |
| en_zh_target | 同目录 `target.spm` | 同上 | **与 source.spm md5 相同**（`8049d3…`），实为同一文件 |
| nllb_600m_source | `~/.local/share/rumi/translate/models/nllb_600m/source.spm` | BPE（trainer.model_type=2，byte_fallback=false） | 256000 词表，piece score 为频次秩（-196,-195,…） |

> 即实际只有 2 个不同模型（Unigram ×1 + BPE ×1），三条轨道仍全部独立执行。NLLB 的 target.spm 亦与 source 相同（md5 `05c551…`），未重复占用轨道。

### 2.4 golden 与判据口径

- golden：`sp.encode(text, out_type=str)` / `out_type=int` / `sp.decode(ids)` / `sp.decode(pieces)`，另有 `SentencePieceNormalizer.normalize` 诊断字段。默认选项（不加 bos/eos，与 ct2rs C++ 路径行为一致——`</s>` 由 ct2rs 的 trait 实现层追加，见第 5 节）。
- **主判据 = ids 序列全等**（pieces 表示法存在 ⏎/▁ 及 unk 表面约定差异的可能，故单列归类）；decoded round-trip 分 ids-入口与 pieces-入口两栏。

---

## 3. 结果

### 3.1 ids 一致率总表（主判据）

| 候选 | en_zh_source (U) | en_zh_target (U) | nllb_600m_source (BPE) |
|---|---|---|---|
| sentencepiece-rs 0.2.2 | **29/29 · 219/219** | **29/29 · 219/219** | **29/29 · 219/219** |
| sentencepiece-rust 0.1.1 | 29/29 · 219/219 | 29/29 · 219/219 | 29/29 · 219/219 |
| hf-tokenizers 0.23.2（转换路线） | 29/29 · 219/219 | 29/29 · 219/219 | 29/29 · 219/219 |
| splintr 0.22.0（转换路线） | 29/29 · 219/219 | 29/29 · 219/219 | **28/29** · 219/219 |

（格式：固定语料 29 条 · 压力语料 219 条；U=Unigram）

固定语料分类明细：sentencepiece-rs / sentencepiece-rust / hf-tokenizers 在全部 12 个类别（zh/en_tech/ja/mix/emoji/zenkaku/nfkc/spaces/empty/ws/long/ctrl）全部满分；splintr 仅 `ws`（纯空白）类 0/1，其余满分。

### 3.2 不一致样本逐条（全部候选 × 全部语料仅此 1 条）

| 项 | 值 |
|---|---|
| 候选/模型/条目 | splintr / nllb_600m_source / c1[26] |
| 原文 | `" \t "`（空格+TAB+空格） |
| golden | normalize→`"   "`→去空格后为空 → pieces=`[]`，ids=`[]` |
| splintr | ids=`[248058]`，该 id 的 piece 为 `▁`（dummy prefix 本身） |
| 归因 | **空白处理/dummy prefix**：输入归一化后为全空白时，SP 折叠+裁剪后输出空序列；splintr 仍输出一个 `▁`。注：其 charsmap 本身正确（tab→空格的映射生效），混合文本中的空白行（c2 的 12 条空白压力行）全部一致，仅"整行皆空白"这一边角失守。 |

### 3.3 pieces / decoded 差异定性（次要口径）

| 候选 | pieces 差异 | decoded(ids) 差异 | 定性 |
|---|---|---|---|
| sentencepiece-rs | **0**（744/744 全等） | **0** | 无 |
| sentencepiece-rust | 有（c2 en_zh 95/219） | 0 | **对拍程序重建方式的伪差异**：该 crate encode 只返回 ids，pieces 由 `id_to_piece` 重建，unk 位置只能给 `"<unk>"`，而官方在 pieces 里给原始表面文本（如 `"今天天气不错"`，id 同为 0）。全部 585 处 piece 差异均为 unk 表面，0 处其他。 |
| hf-tokenizers | 仅 unk 显示（nllb c1 5 条：`“`→`<unk>` 等，ids 全等） | 有（en_zh 7/29、95/219；nllb 24/29、206/219） | **约定差异**：SP 渲染 unk 为 `" ⁇ "`，HF 渲染为 `"<unk>"` 字面量；124 处 decoded 差异逐条核对 100% 属此因，无其他差异。 |
| splintr | 大量（pieces 由逐 id `decode_lossy` 重建，▁ 还原为空格——程序侧伪差异） | 与 hf-tokenizers **逐条完全相同**（24/24 吻合） | 同 HF 的 unk 约定差异 + 上述 1 条空白差异 |

### 3.4 decode round-trip（含 rumi 真实路径）

| 检查项 | sentencepiece-rs |
|---|---|
| `decode(ids) == golden` | 744/744（29+219）× 3 模型全等 |
| `decode(pieces) == golden`（ct2rs trait 实际调用的形态） | 744/744 × 3 模型全等 |

---

## 4. 残余风险与限制（如实声明）

1. **语料覆盖**：248 行 × 3 轨道全等，含大量边角，但不可能穷尽；未测 `enable_sampling`/nbest/`bos:eos:reverse` extra options（rumi 路径不使用；sentencepiece-rs 声明支持 bos/eos/reverse/unk_piece 选项，未对拍）。
2. **byte_fallback 未获真实覆盖**：两个模型的 proto 均 `byte_fallback=false`。sentencepiece-rs/sentencepiece-rust 均声明支持 byte piece 编解码（后者 README 称经 byte-fallback 模型对拍过），但本次无本地模型可证。rumi 未来若引入 byte_fallback 模型（如某些 LLaMA 系），需补对拍。
3. **sentencepiece-rs 工程成熟度**：2026-05 新 crate、单一维护者、2 stars、无 CI 数据可查。对冲：Cargo.toml 锁定精确版本，`/tmp/spm-parity` 的 golden + 对拍脚本可随时回归（建议随 rumi 仓库存档一份精简版）。
4. en_zh/nllb 的 source 与 target .spm 为同文件（md5 相同），"目标端解码器"轨道实为源端重复；未来若 target 换成不同文件，结论不受影响（decode 路径已被 nllb 与 en_zh 两种词表覆盖）。

---

## 5. 接入 rumi 的改造成本评估（判定 ≥⚠️ 附带，本文为 ✅ 仍附）

现状（`/home/banshan/项目/jt/rumi/translate/Cargo.toml` + `src/engine.rs`）：
- `ct2rs = { version = "0.9", features = ["dnnl", "openmp-runtime-comp", "sentencepiece"] }`
- `translator: ct2rs::Translator<ct2rs::tokenizers::auto::Tokenizer>`，`Translator::new(&model_dir, &cfg)` 自动探测 `source.spm`/`target.spm`。

ct2rs 0.9.22 的 `Tokenizer` trait（`src/tokenizer.rs`）极小，正好落在已验证的能力面上：

```rust
pub trait Tokenizer {
    fn encode(&self, input: &str) -> anyhow::Result<Vec<String>>;  // piece 字符串
    fn decode(&self, tokens: Vec<String>) -> anyhow::Result<String>;
}
```

其官方 sentencepiece 实现只做三件事：`encoder.encode(input)` 取 piece 字符串（**含 unk 表面**，本次已证 sentencepiece-rs 逐位一致）→ 末尾追加 `"</s>"` → decode 走 `decode_pieces`（本次已证 pieces-decode 逐位一致）。

**改造步骤（预计 0.5～1 天，含自测）**：
1. `translate/Cargo.toml`：features 去掉 `"sentencepiece"`，加 `sentencepiece-rs = "0.2.2"`（精确版）；ct2rs 的 `default-features = false` 保持。第二份 protobuf（sentencepiece-sys 内嵌 protobuf-lite 3.21）随之消失，可回退 ORT 侧的动态链 + `--exclude-libs` 止血补丁（另案评估）。
2. 新建 `translate/src/tokenizer.rs`（约 60 行）：包装两个 `sentencepiece_rs::SentencePieceProcessor`（source/target），`impl ct2rs::Tokenizer`：encode = `sp.encode(input)?` + push `"</s>"`；decode = `sp.decode(tokens.as_slice())`。注意按 ct2rs 源码逐字对齐 `"</s>"` 追加行为。
3. `engine.rs`：`Translator<ct2rs::tokenizers::auto::Tokenizer>` 改为 `Translator<RumiSpmTokenizer>`；构造处 `Translator::with_tokenizer(&model_dir, tok, &cfg)`（ct2rs 提供 `with_tokenizer`，其 sentencepiece 模块文档示例即此用法）。模型目录仍只需 `model.bin + source.spm + target.spm`，`is_loadable` 判断不用动。
4. 回归：拿 `/tmp/spm-parity` 的 golden JSON 对新 tokenizer 直接跑 ids 对拍（脚本已就绪），再跑 rumi 翻译冒烟。
5. 引擎出口的 ▁(U+2581) 清理逻辑（engine.rs 注释所述）**保持不变**——decode 输出与 C++ 版逐位一致，无需调整。

**双 feature 轨道建议**：过渡期保留一条 `spm-sys` cfg 开关（`[features] rust-spm = []` / 默认开 rust-spm），两实现共存一两个版本便于线上对比回退；确认稳定后删除 `sentencepiece` feature 与止血补丁。备选回退路线为 1.3 的 tokenizers+tokenizer.json（ids 亦 100%），但需模型分发转换产物并自理 `</s>` 与 unk 渲染，改造成本更高，不作首选。

**性能**：对拍程序单进程跑完 4 候选 × 3 模型 × 219 行（含 5000+ 字符长文）共约 1.7 秒，sentencepiece-rs 无性能疑点（未做严格基准，翻译场景分词占比本来就低）。

---

## 6. 附录

**产物清单**（除本报告外均在 `/tmp/spm-parity/`，不入库）：
- `gen_corpus.py` / `corpus.txt`（29 条固定）/ `corpus.json`；`gen_corpus2.py` / `corpus2.txt`（219 条压力，seed=42 可复现）
- `gen_golden.py` → `golden-{en_zh_source,en_zh_target,nllb_600m_source}[-c2].json`
- `convert_hf.py` → `tokjson-en_zh_source.json` / `tokjson-nllb_600m_source.json`（含 transformers 5.22 BPE 转换 bug 的绕法）
- `rs-check/`（四候选对拍 bin）→ `rs-{sentencepiece-rs,sentencepiece-rust,hf-tokenizers,splintr}-{model}[-c2].json`
- `compare.py`（总表 + 分类归因 + 逐条明细）

**复现**：`venv/bin/python -I gen_golden.py`（两套语料各一次）→ `rs-check/target/release/rs-check <corpus> [suffix]` → `venv/bin/python -I compare.py`。

**版本**：Python sentencepiece 0.2.2（cp314 wheel）；transformers 5.22.1 / tokenizers(py) 0.23.3；Rust sentencepiece-rs 0.2.2、sentencepiece-rust 0.1.1、splintr 0.22.0、tokenizers 0.23.2；rustc 1.99.0；ct2rs 参照 0.9.22（本机 registry 缓存，rumi 在用）。
