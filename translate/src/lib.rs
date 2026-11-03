//! rumi-translate：离线（CTranslate2/ct2rs）+ 在线翻译核心。
//!
//! 设计与选型见 `docs/translate-design.md`；实现规格见 `docs/translate-spec-*.md`。
//! 全同步 API：UI 沿用「std 线程 + oneshot 回 dioxus spawn」，server 用 spawn_blocking。

pub mod docmt;
pub mod download;
pub mod engine;
pub mod lang;
pub mod online;
pub mod postfix;
pub mod registry;
pub mod segment;
// 纯 Rust 分词器仅默认分支编译；spm-sys 回退分支由 ct2rs 自带的
// C++ sentencepiece 承担（engine.rs 按 feature 分支选型）
#[cfg(not(feature = "spm-sys"))]
pub mod tokenizer;

use std::path::PathBuf;

/// 模型根目录：环境变量 RUMI_TRANSLATE_MODELS_DIR 优先，否则
/// `dirs::data_dir()/rumi/translate/models`（Linux: ~/.local/share/rumi/translate/models）。
pub fn models_dir() -> PathBuf {
    if let Ok(d) = std::env::var("RUMI_TRANSLATE_MODELS_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rumi")
        .join("translate")
        .join("models")
}

/// 离线计算精度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compute {
    Int8,
    Float32,
}

/// 引擎选择（facade 层；在线子集与 online::OnlineEngine 一一对应）。
#[derive(Debug, Clone)]
pub enum Engine {
    Offline,
    Google,
    Microsoft,
    DeepL {
        api_key: String,
        pro: bool,
    },
    DeepLX {
        url: String,
    },
    OpenAI {
        base_url: String,
        api_key: String,
        model: String,
    },
}

/// 统一翻译入口（UI/server/CLI 共用）。
///
/// - from==to 直接原样返回；texts 与返回值一一对应；空串原样返回空串。
/// - 离线：`engine::shared()` 懒加载模型（未安装报"语言包未安装：<from>→<to>，请先在翻译页下载"）。
/// - 在线：逐条调 `online::translate_online`。
/// - polish=true 且 to=="zh" 时对每条结果过 `postfix::polish_zh`。
pub fn translate(
    texts: &[String],
    from: &str,
    to: &str,
    engine: &Engine,
    polish: bool,
) -> Result<Vec<String>, String> {
    let from = lang::normalize(from);
    let to = lang::normalize(to);
    if from == to {
        return Ok(texts.to_vec());
    }
    let mut out = match engine {
        Engine::Offline => {
            let eng = engine::shared();
            let mut res = Vec::with_capacity(texts.len());
            for t in texts {
                if t.trim().is_empty() {
                    res.push(String::new());
                    continue;
                }
                res.push(
                    eng.translate(t, &from, &to)
                        .map_err(|e| e.to_string())?,
                );
            }
            res
        }
        online_engine => {
            let oe = match online_engine {
                Engine::Google => online::OnlineEngine::Google,
                Engine::Microsoft => online::OnlineEngine::Microsoft,
                Engine::DeepL { api_key, pro } => online::OnlineEngine::DeepL {
                    api_key: api_key.clone(),
                    pro: *pro,
                },
                Engine::DeepLX { url } => online::OnlineEngine::DeepLX { url: url.clone() },
                Engine::OpenAI {
                    base_url,
                    api_key,
                    model,
                } => online::OnlineEngine::OpenAI {
                    base_url: base_url.clone(),
                    api_key: api_key.clone(),
                    model: model.clone(),
                },
                Engine::Offline => unreachable!(),
            };
            let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            online::translate_online(&refs, &from, &to, &oe)?
        }
    };
    if polish && to == "zh" {
        for t in out.iter_mut() {
            *t = postfix::polish_zh(t);
        }
    }
    // SentencePiece 分词标记清理（对所有目标语言生效——"润色"才分语言，
    // "清理"不分）：ct2rs 反解不处理句首等位置的 ▁(U+2581)，实测任何方向的
    // 离线译文都可能带出（如 "▁The▁default▁port…"）。
    for t in out.iter_mut() {
        *t = strip_sp_marker(t);
    }
    Ok(out)
}

/// 把 SentencePiece 空格标记 ▁(U+2581) 还原为空格并去掉首尾空白。
/// 与 postfix::polish_zh 的 R0 同一规则（那边保留幂等重复无害）。
pub fn strip_sp_marker(text: &str) -> String {
    text.replace('\u{2581}', " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_sp_marker;

    #[test]
    fn strip_sp_marker_all_positions() {
        assert_eq!(strip_sp_marker("▁The default port"), "The default port");
        assert_eq!(strip_sp_marker("默认▁端口"), "默认 端口");
        assert_eq!(strip_sp_marker("  plain  "), "plain");
        assert_eq!(strip_sp_marker(""), "");
    }
}
