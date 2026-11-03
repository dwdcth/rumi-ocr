# 规格 H：ct2rs 分词器替换为纯 Rust sentencepiece-rs（2026-11-03 定案）

> 背景：对拍报告 `docs/spm-parity-report.md`（744/744 全量零差异，判定 ✅ 可直接换）。
> 用户决策：主路 sentencepiece-rs 0.2.2 直载、锁死版本、上游烂尾则 fork 自维护；不做备胎转换器。
> **定位红线**：本工作项只动分词层。ORT 侧配置（ort-sys pkg-config 动态链、
> `.cargo/config.toml` 的 `--exclude-libs,ALL`）**一律保留不动**——回静态 ORT 是
> 另案（cpuinfo 撞名风险，见对拍报告 §5）。替换后收益：sentencepiece-sys 及其内嵌
> protobuf-lite 3.21 从依赖树消失，双 protobuf 冲突从根上不存在。

## 工人 H 文件集

只动：`translate/Cargo.toml`、`translate/src/tokenizer.rs`（新建）、
`translate/src/engine.rs`、`translate/src/lib.rs`（仅 mod 声明一行）、
`translate/tests/spm_parity_tests.rs`（新建）、`translate/tests/spm_golden/`（新建目录，
golden 数据）、`Cargo.lock`（自动）。禁止动：download/registry/catalog/online/bin、
ui/、server/。

## 1. Cargo.toml

```toml
# 分词：纯 Rust 直载 .spm（对拍 744/744 全等，见 docs/spm-parity-report.md）。
# 锁死精确版本：上游为 2026-05 新 crate（单人维护），升级必须重跑
# tests/spm_parity_tests.rs golden 回归；上游烂尾则 fork（用户已拍板自维护）。
sentencepiece-rs = "=0.2.2"
```

ct2rs 依赖行：`features = ["dnnl", "openmp-runtime-comp", "sentencepiece"]`
→ **去掉 `"sentencepiece"`**，改为 `features = ["dnnl", "openmp-runtime-comp"]`。

feature 双轨（过渡期回退开关）：

```toml
[features]
# 过渡开关：启用则回退 ct2rs 自带 C++ sentencepiece（含 protobuf-lite，会重新引入
# 双 protobuf 风险，仅排查分词问题时用）；默认纯 Rust 路线。稳定 1-2 个版本后删除。
spm-sys = ["ct2rs/sentencepiece"]
default = []
```

lib.rs 按 feature 分支声明 tokenizer 模块：`#[cfg(not(feature = "spm-sys"))] pub mod tokenizer;`
（spm-sys 时 engine 继续用 ct2rs 的 auto tokenizer，tokenizer.rs 不编译——两分支都要能编译）。

## 2. tokenizer.rs（新建，~60 行，与 ct2rs 官方实现逐字对齐）

**先读本机 ct2rs 0.9.22 源码**（`~/.cargo/registry/src/*/ct2rs-0.9.22/src/tokenizers/sentencepiece.rs`
与 `src/tokenizer.rs` 的 trait 定义），本节契约以源码为准：

```rust
//! 纯 Rust SentencePiece 分词器（对拍全等，见 docs/spm-parity-report.md）。
use ct2rs::Tokenizer;

pub struct RumiSpmTokenizer {
    source: sentencepiece_rs::SentencePieceProcessor,
    target: sentencepiece_rs::SentencePieceProcessor,
}

impl RumiSpmTokenizer {
    /// 从模型目录加载 source.spm / target.spm（统一布局保证双文件存在；
    /// en_zh/nllb 实际是同一文件的两份拷贝，各自 open 即可）。
    pub fn open(model_dir: &Path) -> anyhow::Result<Self>;
}

impl Tokenizer for RumiSpmTokenizer {
    /// 与 ct2rs 官方 sentencepiece 实现逐字对齐：encode 取 piece 字符串
    ///（含 unk 表面文本），末尾追加 "</s>"（若 ct2rs 源码还有其他处理，
    /// 如空输入行为，一并照抄）。
    fn encode(&self, input: &str) -> anyhow::Result<Vec<String>>;
    /// decode(pieces)：与官方实现一致（ct2rs trait 的真实调用形态，
    /// 对拍 decode(pieces) 744/744 全等）。"</s>" 等控制 piece 的剥离若
    /// ct2rs 官方实现在 decode 侧做了，照抄；sentencepiece-rs 的 decode
    /// 自带控制符剥离则依赖之——以对拍 golden 测试兜底。
    fn decode(&self, tokens: Vec<String>) -> anyhow::Result<String>;
}
```

sentencepiece-rs 0.2.2 的 API 名（`SentencePieceProcessor::open` / `encode` /
`decode`）以 crate 文档与源码为准，差异记回报。

## 3. engine.rs

- `Loaded.translator` 类型：`ct2rs::Translator<RumiSpmTokenizer>`
  （spm-sys 分支保持 `Translator<ct2rs::tokenizers::auto::Tokenizer>`——cfg 分支）。
- 构造：`Translator::with_tokenizer(&model_dir, RumiSpmTokenizer::open(&model_dir)?, &cfg)`。
- 其余（加载/缓存/unload/segment/NLLB target-prefix/strip_sp_marker）不动。

## 4. golden 回归闸门（tests/spm_parity_tests.rs + tests/spm_golden/）

- 从 `/tmp/spm-parity/golden-{en_zh_source,nllb_600m_source}.json` 取**固定语料 29 条**
  子集入库（每模型一份；仓库体积可控）。若 /tmp 已被清理，用报告附录脚本重新生成
  （venv 在 /tmp/spm-parity/venv；脚本 `gen_corpus.py`/`gen_golden.py`）。
- 测试逻辑：读 `~/.local/share/rumi/translate/models/{en_zh,nllb_600m}/source.spm`
  （`RUMI_TRANSLATE_MODELS_DIR` 优先）；**模型缺失则 `eprintln! + return Ok` 跳过**
  （不打网、不失败）；存在则对 29 条逐条断言：
  - `tokenizer.encode(text)`（去掉末尾 `</s>` 后）== golden pieces
  - `tokenizer.decode(pieces)` == golden decoded
- 常规 `cargo test -p rumi-translate` 里它是活跃用例（本机有模型即真跑）。

## 5. 验收（工人自验 + 监工复验）

1. `cargo check -p rumi-translate` 0 warning；`cargo test -p rumi-translate` 全绿
   （原有用例不回归 + golden 闸门通过）。
2. `cargo tree -i sentencepiece-sys` → **"package not found"**（protobuf-lite 依赖树消失）。
3. 冒烟并贴回报：
   `cargo run -q -p rumi-translate -- --models-dir ~/.local/share/rumi/translate/models --to zh "The default port is 8080. Set the environment variable first."`
   译文与替换前一致（替换前基准："默认端口为8080. 在启动 Docker 容器前设定环境变量。"——
   分词变了译文可能微变，**允许不同但必须语义完整**；ids 等价性由 golden 闸门保证）。
   再翻一句中文→日语（NLLB 路径）确认 BPE 轨道。
4. `cargo build -p rumi-ui && nm -D --defined-only target/debug/rumi-ui | c++filt | grep -c "google::protobuf"`
   → 0（替换前就是 0，替换后必须仍为 0；这是回归基线）。
5. `--features spm-sys` 的回退分支 `cargo check -p rumi-translate --features spm-sys` 通过。

## 6. 回报格式

改动文件清单；ct2rs Tokenizer trait 与 sentencepiece-rs API 的实际签名（与本文差异）；
golden 闸门结果；冒烟译文（替换前后对比）；`cargo tree -i sentencepiece-sys` 输出；歧义点清单。
