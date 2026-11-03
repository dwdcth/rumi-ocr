//! golden 回归闸门（docs/translate-spec-spm.md §4）：纯 Rust 分词（tokenizer.rs）
// spm-sys 回退分支走 ct2rs 自带 C++ sentencepiece，纯 Rust tokenizer 模块不编译
// （见 lib.rs），本闸门对其无意义——整个测试 crate 置空，保证该 feature 下
// cargo test 也能编译。
#![cfg(not(feature = "spm-sys"))]
//! vs Python 官方 sentencepiece 离线 golden（docs/spm-parity-report.md，全量
//! 744/744 零差异）。每模型一份固定语料 29 条，入库于 tests/spm_golden/
//! （由 /tmp/spm-parity/gen_golden.py 产物裁剪而来，只留断言所需字段）。
//!
//! 模型未安装则整测 eprintln! + Ok 跳过（不打网、不失败）；本机装了就真跑，
//! 逐条断言 encode / decode 与官方逐位一致。升级 sentencepiece-rs 版本必须
//! 重跑本闸门（Cargo.toml 锁版注释同此约）。

use std::path::Path;

use ct2rs::Tokenizer as _;
use rumi_translate::tokenizer::RumiSpmTokenizer;
use serde::Deserialize;

/// golden 文档裁剪版结构（完整字段口径见 docs/spm-parity-report.md §2.4）
#[derive(Deserialize)]
struct GoldenDoc {
    entries: Vec<GoldenEntry>,
}

#[derive(Deserialize)]
struct GoldenEntry {
    idx: usize,
    text: String,
    /// Python 官方 sp.encode(text, out_type=str)，不含 </s>——那是 ct2rs
    /// trait 实现层追加的
    pieces: Vec<String>,
    /// Python 官方 sp.decode(pieces)，即 ct2rs trait 的真实调用形态
    decoded_from_pieces: String,
}

/// 单模型闸门：模型缺 source.spm 则跳过；存在则对 29 条逐条断言。
fn run_gate(model_key: &str, golden_file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let models_dir = rumi_translate::models_dir();
    let spm = models_dir.join(model_key).join("source.spm");
    if !spm.is_file() {
        eprintln!("skip golden gate: {spm:?} 不存在（模型未安装），跳过");
        return Ok(());
    }
    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("spm_golden")
        .join(golden_file);
    let doc: GoldenDoc = serde_json::from_str(&std::fs::read_to_string(&golden)?)?;
    let tok = RumiSpmTokenizer::open(&models_dir.join(model_key))?;
    for e in &doc.entries {
        // encode：trait 层尾部必追加 </s>；剥掉后须与官方 pieces 逐位一致
        let mut pieces = tok.encode(&e.text)?;
        assert_eq!(
            pieces.pop().as_deref(),
            Some("</s>"),
            "encode 未追加 </s> @{} idx={}",
            golden_file,
            e.idx
        );
        assert_eq!(pieces, e.pieces, "encode pieces 不一致 @{} idx={}", golden_file, e.idx);
        // decode：与官方 sp.decode(pieces) 逐位一致
        let decoded = tok.decode(e.pieces.clone())?;
        assert_eq!(
            decoded, e.decoded_from_pieces,
            "decode(pieces) 不一致 @{} idx={}",
            golden_file, e.idx
        );
        // 运行时形态：CT2 输出序列带控制 </s>，decode 须按 CONTROL 类型剥离
        // 之（结果与不带时一致）；该剥离行为是 sentencepiece-rs 承接官方
        // C++ decode_pieces 的关键契约，闸门盯住
        let mut with_eos = e.pieces.clone();
        with_eos.push("</s>".to_string());
        let decoded_eos = tok.decode(with_eos)?;
        assert_eq!(
            decoded_eos, e.decoded_from_pieces,
            "decode(pieces+</s>) 未剥离控制符 @{} idx={}",
            golden_file, e.idx
        );
    }
    eprintln!("golden gate passed: {golden_file} ({} entries)", doc.entries.len());
    Ok(())
}

#[test]
fn golden_en_zh_source() -> Result<(), Box<dyn std::error::Error>> {
    run_gate("en_zh", "golden-en_zh_source.json")
}

#[test]
fn golden_nllb_600m_source() -> Result<(), Box<dyn std::error::Error>> {
    run_gate("nllb_600m", "golden-nllb_600m_source.json")
}
