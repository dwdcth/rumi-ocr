//! Microsoft Edge 免费翻译端点：先向 authenticate 换 JWT（token 在**响应头**
//! Authorization 里，不在 body），再带 Bearer 调 translate；token 进程内缓存 9 分钟。

use super::RequestSpec;
use serde_json::json;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const AUTH_URL: &str =
    "https://edge.api.cognitive.microsoft.com/authenticate?translatorTextScope=translate";
const TRANSLATE_URL: &str = "https://api-edge.cognitive.microsofttranslator.com/translate";

/// token 有效期：Edge 端点 JWT 实际约 10 分钟，留 1 分钟余量，过期重取。
const TOKEN_TTL: Duration = Duration::from_secs(9 * 60);

/// 进程内 token 缓存：(JWT, 取得时刻)。
static TOKEN: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// 内部两位码 → Microsoft 语言码（未知码原样透传，让服务端报错如实上抛）。
pub fn map(lang: &str) -> String {
    match lang {
        "zh" => "zh-Hans".into(),
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

/// token 缓存有效性（纯函数）：有非空 JWT 且未过 9 分钟才算有效。
pub fn token_valid(t: &Option<(String, Instant)>) -> bool {
    match t {
        Some((jwt, at)) => !jwt.is_empty() && at.elapsed() < TOKEN_TTL,
        None => false,
    }
}

/// 构建翻译请求：一次带全部 texts（body 为 JSON 数组，单请求批量）。
pub fn build(texts: &[&str], from: &str, to: &str, token: &str) -> RequestSpec {
    let url = format!("{TRANSLATE_URL}?api-version=3.0&from={}&to={}", map(from), map(to));
    let body = json!(texts
        .iter()
        .map(|t| json!({ "Text": t }))
        .collect::<Vec<_>>())
    .to_string();
    RequestSpec {
        method: "POST",
        url,
        headers: vec![
            ("Authorization".into(), format!("Bearer {token}")),
            ("Content-Type".into(), "application/json".into()),
        ],
        body: Some(body),
    }
}

/// 解析翻译响应：按序取每元素 translations[0].text。
/// 响应形如 `[{"translations":[{"text":"你好","to":"zh-Hans"}]},...]`。
pub fn parse(body: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    let arr = v.as_array().ok_or("响应不是 JSON 数组")?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let text = item
            .get("translations")
            .ok_or("响应元素缺少 translations")?
            .get(0)
            .ok_or("translations 为空")?
            .get("text")
            .ok_or("translations[0] 缺少 text")?
            .as_str()
            .ok_or("translations[0].text 不是字符串")?;
        out.push(text.to_string());
    }
    Ok(out)
}

/// 向 authenticate 端点换取 JWT。真实端点的响应头是裸 JWT；个别代理可能补成
/// "Bearer <jwt>"，这里统一剥前缀取裸 token，build() 再按协议加回 Bearer。
fn fetch_token() -> Result<String, String> {
    let resp = match super::agent().get(AUTH_URL).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            return Err(format!("authenticate 返回 HTTP {code}：{}", super::snippet(&body, 200)));
        }
        Err(e) => return Err(format!("请求 authenticate 失败：{e}")),
    };
    let auth = resp
        .header("Authorization")
        .ok_or("authenticate 响应缺少 Authorization 头")?;
    let jwt = auth.strip_prefix("Bearer ").unwrap_or(auth).trim();
    if jwt.is_empty() {
        return Err("authenticate 的 Authorization 头为空".into());
    }
    Ok(jwt.to_string())
}

/// 取缓存 token，过期/缺失则重取并刷新缓存。
fn cached_token() -> Result<String, String> {
    // 锁中毒说明持锁线程 panic 过，取回内部数据继续（缓存丢失只是多换一次 token）。
    let mut guard = TOKEN.lock().unwrap_or_else(PoisonError::into_inner);
    if token_valid(&guard) {
        // token_valid 为真时必然是 Some。
        return Ok(guard.as_ref().expect("token_valid 已保证 Some").0.clone());
    }
    let jwt = fetch_token()?;
    *guard = Some((jwt.clone(), Instant::now()));
    Ok(jwt)
}

/// 翻译一批：单请求带全部 texts（token 缓存命中时不请求 authenticate）。
pub fn translate(texts: &[&str], from: &str, to: &str) -> Result<Vec<String>, String> {
    let token = cached_token().map_err(|e| format!("microsoft: {e}"))?;
    let spec = build(texts, from, to, &token);
    let body = super::send(&spec).map_err(|e| format!("microsoft: {e}"))?;
    parse(&body).map_err(|e| format!("microsoft: {e}"))
}
