//! DeepLX 自建服务：单文本协议（{"text":...}），多条逐条请求、无间隔。
//! 错误响应带上返回原文片段（前 200 字）便于排查自建服务问题。

use super::{snippet, RequestSpec};
use serde_json::json;

/// 用户未填 URL 时的默认地址（DeepLX 项目默认监听端口）。
const DEFAULT_URL: &str = "http://127.0.0.1:1188/translate";

/// 内部两位码 → DeepLX 大写码（与 deepl 同表；未知码原样透传）。
pub fn map(lang: &str) -> String {
    match lang {
        "zh" => "ZH".into(),
        "en" => "EN".into(),
        "ja" => "JA".into(),
        "ko" => "KO".into(),
        "fr" => "FR".into(),
        "de" => "DE".into(),
        "es" => "ES".into(),
        "ru" => "RU".into(),
        "pt" => "PT".into(),
        "it" => "IT".into(),
        other => other.into(),
    }
}

/// 构建单条文本的请求；url 为空串时回退默认地址（单条协议，多条由 translate() 循环）。
pub fn build(text: &str, from: &str, to: &str, url: &str) -> RequestSpec {
    let url = if url.is_empty() { DEFAULT_URL } else { url };
    let body = json!({
        "text": text,
        "source_lang": map(from),
        "target_lang": map(to),
    })
    .to_string();
    RequestSpec {
        method: "POST",
        url: url.to_string(),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: Some(body),
    }
}

/// 解析 DeepLX 响应：`{"code":200,"data":"..."}` → data。
/// code != 200 或缺 data → 报错并附返回原文片段（前 200 字）。
pub fn parse(body: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    let code = v.get("code").and_then(|c| c.as_i64()).ok_or("响应缺少 code")?;
    if code != 200 {
        let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
        return Err(format!("code={code} {msg}；响应片段：{}", snippet(body, 200)));
    }
    let data = v
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| format!("code=200 但响应缺少 data；响应片段：{}", snippet(body, 200)))?;
    Ok(vec![data.to_string()])
}

/// 翻译一批：逐条请求（单文本协议），无间隔；任何一条失败整批报错。
pub fn translate(texts: &[&str], from: &str, to: &str, url: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(texts.len());
    for text in texts {
        let spec = build(text, from, to, url);
        let body = super::send(&spec).map_err(|e| format!("deeplx: {e}"))?;
        let mut one = parse(&body).map_err(|e| format!("deeplx: {e}"))?;
        out.push(one.remove(0));
    }
    Ok(out)
}
