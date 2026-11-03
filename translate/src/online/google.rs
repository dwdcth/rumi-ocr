//! Google 免费翻译端点（translate.googleapis.com，client=gtx）：单文本协议，
//! 多条逐条请求、间隔 150ms；无 key 但对无 User-Agent 的请求偶发 429，故带固定 UA。
//! 端点国内可达性不保证（见 docs/translate-design.md），默认引擎仍是离线。

use super::{percent_encode, throttle, RequestSpec};
use serde_json::Value;

/// 内部两位码 → Google 语言码（未知码原样透传，让服务端报错如实上抛）。
pub fn map(lang: &str) -> String {
    match lang {
        "zh" => "zh-CN".into(),
        "en" => "en".into(),
        "ja" => "ja".into(),
        "ko" => "ko".into(),
        "fr" => "fr".into(),
        "de" => "de".into(),
        "es" => "es".into(),
        "ru" => "ru".into(),
        "pt" => "pt".into(),
        "it" => "it".into(),
        "vi" => "vi".into(),
        "ar" => "ar".into(),
        "th" => "th".into(),
        other => other.into(),
    }
}

/// 构建单条文本的请求（免费端点是单文本协议，多条由 translate() 外层循环）。
pub fn build(text: &str, from: &str, to: &str) -> RequestSpec {
    let url = format!(
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl={}&tl={}&dt=t&q={}",
        percent_encode(&map(from)),
        percent_encode(&map(to)),
        percent_encode(text),
    );
    RequestSpec {
        method: "GET",
        url,
        headers: vec![("User-Agent".into(), "rumi-translate/0.1".into())],
        body: None,
    }
}

/// 解析 gtx 响应：只取第一层 [0]，逐元素 [0] 拼接为该条译文，返回长度 1 的 Vec。
/// 响应形如 `[[["世界","world",null,null,1],["！","!",null,null,1]],null,"en",...]`。
pub fn parse(body: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    let segments = v.get(0).ok_or("响应缺少译文数组 [0]")?;
    let segments = segments.as_array().ok_or("响应 [0] 不是数组")?;
    let mut out = String::new();
    for seg in segments {
        let piece = seg
            .get(0)
            .ok_or("译文分段缺少 [0]")?
            .as_str()
            .ok_or("译文分段 [0] 不是字符串")?;
        out.push_str(piece);
    }
    Ok(vec![out])
}

/// 翻译一批：逐条请求、间隔 150ms（降低免费端点 429 概率）；任何一条失败整批报错。
pub fn translate(texts: &[&str], from: &str, to: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(texts.len());
    for (i, text) in texts.iter().enumerate() {
        if i > 0 {
            throttle();
        }
        let spec = build(text, from, to);
        let body = super::send(&spec).map_err(|e| format!("google: {e}"))?;
        let mut one = parse(&body).map_err(|e| format!("google: {e}"))?;
        out.push(one.remove(0));
    }
    Ok(out)
}
