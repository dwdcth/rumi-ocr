//! 纯 Rust SentencePiece 分词器（对拍全等，见 docs/spm-parity-report.md）。
//!
//! 替换 ct2rs 自带的 C++ sentencepiece（`sentencepiece` feature）——那份实现内嵌
//! protobuf-lite 3.21，与 ORT 的 protobuf 3.14 链进同一可执行文件曾互劫崩溃；
//! 本实现零 C++ 依赖，直接读 .spm。与 ct2rs 0.9.22 官方
//! `tokenizers::sentencepiece::Tokenizer` 逐字对齐（encode 取 piece 字符串含
//! unk 表面文本、末尾追加 `</s>`；decode 交给分词库处理控制 piece），等价性由
//! Python 官方版离线 golden（tests/spm_parity_tests.rs，29 条 × 2 模型）兜底。
use std::path::Path;

use ct2rs::Tokenizer;

pub struct RumiSpmTokenizer {
    source: sentencepiece_rs::SentencePieceProcessor,
    target: sentencepiece_rs::SentencePieceProcessor,
}

impl RumiSpmTokenizer {
    /// 从模型目录加载 source.spm / target.spm（统一布局保证双文件存在；
    /// en_zh/nllb 实际是同一文件的两份拷贝，各自 open 即可）。
    pub fn open(model_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            source: sentencepiece_rs::SentencePieceProcessor::open(model_dir.join("source.spm"))?,
            target: sentencepiece_rs::SentencePieceProcessor::open(model_dir.join("target.spm"))?,
        })
    }
}

impl Tokenizer for RumiSpmTokenizer {
    /// 与 ct2rs 官方 sentencepiece 实现逐字对齐：encode 取 piece 字符串（含 unk
    /// 表面文本），末尾追加 `</s>`（CT2 解码侧靠它截断，与官方行为一致）。
    /// sentencepiece-rs 的 encode 只返回 piece，无需再经 id→piece 重建。
    fn encode(&self, input: &str) -> anyhow::Result<Vec<String>> {
        let mut pieces = self.source.encode(input)?;
        pieces.push("</s>".to_string());
        Ok(pieces)
    }

    /// 与官方实现一致：decode 走 pieces 入口（ct2rs trait 的真实调用形态）。
    /// `</s>` 等控制 piece 由 sentencepiece-rs 按 proto 的 CONTROL 类型自动剥离
    /// （官方 C++ decode_pieces 同规则），对拍 decode(pieces) 744/744 全等。
    fn decode(&self, tokens: Vec<String>) -> anyhow::Result<String> {
        self.target.decode(&tokens).map_err(Into::into)
    }
}
