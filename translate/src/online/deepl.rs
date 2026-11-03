//! DeepL 官方 API v2：/v2/translate，text 数组原生批量；免费/专业版仅域名不同。
//! vi/ar/th 不在映射表内，原码透传，让服务端报错如实上抛。

use super::RequestSpec;
use serde_json::json;

/// 免费版/专业版端点（DeepL 按 key 类别区分域名，用错域名服务端会 403）。
fn endpoint(pro: bool) -> &'static str {
    if pro {
        "https://api.deepl.com/v2/translate"
    } else {
        "https://api-free.deepl.com/v2/translate"
    }
}

/// 内部两位码 → DeepL 大写码（vi/ar/th 无官方码，原样透传）。
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

/// 构建翻译请求：text 数组一次带全部 texts。
pub fn build(texts: &[&str], from: &str, to: &str, api_key: &str, pro: bool) -> RequestSpec {
    let body = json!({
        "text": texts,
        "target_lang": map(to),
        "source_lang": map(from),
    })
    .to_string();
    RequestSpec {
        method: "POST",
        url: endpoint(pro).to_string(),
        headers: vec![
            ("Authorization".into(), format!("DeepL-Auth-Key {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ],
        body: Some(body),
    }
}

/// 解析翻译响应：按序取每元素 text。
/// 响应形如 `{"translations":[{"detected_source_language":"EN","text":"你好"},...]}`。
pub fn parse(body: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    let arr = v
        .get("translations")
        .ok_or("响应缺少 translations")?
        .as_array()
        .ok_or("translations 不是数组")?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let text = item
            .get("text")
            .ok_or("translations 元素缺少 text")?
            .as_str()
            .ok_or("translations 元素 text 不是字符串")?;
        out.push(text.to_string());
    }
    Ok(out)
}

/// 翻译一批：单请求批量。
pub fn translate(
    texts: &[&str],
    from: &str,
    to: &str,
    api_key: &str,
    pro: bool,
) -> Result<Vec<String>, String> {
    let spec = build(texts, from, to, api_key, pro);
    let body = super::send(&spec).map_err(|e| format!("deepl: {e}"))?;
    parse(&body).map_err(|e| format!("deepl: {e}"))
}
