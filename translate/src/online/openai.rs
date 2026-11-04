//! OpenAI 兼容 /chat/completions：多段文本按 \n join 一次请求，回包按 \n 拆回。
//! 任何 OpenAI 兼容服务（含本地 ollama 类无鉴权服务）都可用：key 为空则不传
//! Authorization 头；base_url 由用户保证已含 /v1。

use super::RequestSpec;
use serde_json::json;

/// model 未配置（空串）时的兜底。
const DEFAULT_MODEL: &str = "gpt-4o-mini";

/// 系统提示词（from_name/to_name 为展示名，调用方用 crate::lang::display 提供）。
fn system_prompt(from_name: &str, to_name: &str) -> String {
    format!(
        "You are a translation engine. Translate the user text from {from_name} to {to_name}. \
         Output ONLY the translation with original line breaks preserved. No explanations."
    )
}

/// 系统提示词 + 术语表注入段（glossary 规格 §4）：末尾拼 prompt_block 的
/// 规格写死格式——复用 [`crate::glossary::Glossary::prompt_block`] 避免两处
/// 格式漂移；空术语表原样返回（零开销）。
fn system_prompt_with_glossary(
    from_name: &str,
    to_name: &str,
    glossary: &[(String, String)],
) -> String {
    let mut prompt = system_prompt(from_name, to_name);
    if !glossary.is_empty() {
        // 引擎链路里术语表是 (原文, 译文) 对（Engine 枚举不带自定义类型），
        // 这里回装成 Glossary 复用统一的 prompt_block 渲染
        let g = crate::glossary::Glossary {
            terms: glossary
                .iter()
                .map(|(src, dst)| crate::glossary::GlossaryTerm {
                    src: src.clone(),
                    dst: dst.clone(),
                })
                .collect(),
        };
        prompt.push_str(&g.prompt_block());
    }
    prompt
}

/// 构建请求的实质逻辑（入参已是展示名）。
///
/// 独立出来并 pub 是为了单测绕开 `crate::lang::display`——工人 A 并行实现中该函数
/// 此刻是 todo!() 桩（调用即 panic），tests/online_tests.rs 用常量展示名测本函数，
/// URL/headers/body 断言完整性不受影响（见规格 §7 与最终回报）。
pub fn build_with_names(
    texts: &[&str],
    from_name: &str,
    to_name: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
    glossary: &[(String, String)],
) -> RequestSpec {
    // base_url 已含 /v1 由用户负责，这里只做尾部斜杠清理后拼路径。
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let model = if model.is_empty() { DEFAULT_MODEL } else { model };
    let body = json!({
        "model": model,
        "temperature": 0.0,
        "messages": [
            {
                "role": "system",
                "content": system_prompt_with_glossary(from_name, to_name, glossary),
            },
            {
                "role": "user",
                "content": texts.join("\n"),
            },
        ],
    })
    .to_string();
    // 本地 ollama 类服务无鉴权：key 为空就不传 Authorization 头。
    let mut headers = Vec::new();
    if !api_key.is_empty() {
        headers.push(("Authorization".into(), format!("Bearer {api_key}")));
    }
    headers.push(("Content-Type".into(), "application/json".into()));
    RequestSpec {
        method: "POST",
        url,
        headers,
        body: Some(body),
    }
}

/// 构建请求：展示名由 crate::lang::display 提供。
pub fn build(
    texts: &[&str],
    from: &str,
    to: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
    glossary: &[(String, String)],
) -> RequestSpec {
    build_with_names(
        texts,
        &crate::lang::display(from),
        &crate::lang::display(to),
        base_url,
        api_key,
        model,
        glossary,
    )
}

/// 解析响应：取 choices[0].message.content 按 \n 拆回 expected 份。
/// 份数不强行对齐：少于请求份数报错；多于丢弃多余行（content 尾随换行的空行即属此类）。
pub fn parse(body: &str, expected: usize) -> Result<Vec<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    let content = v
        .get("choices")
        .ok_or("响应缺少 choices")?
        .get(0)
        .ok_or("choices 为空")?
        .get("message")
        .ok_or("choices[0] 缺少 message")?
        .get("content")
        .ok_or("message 缺少 content")?
        .as_str()
        .ok_or("content 不是字符串")?;
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.len() < expected {
        return Err(format!("返回行数不符（期望 {expected} 得到 {}）", lines.len()));
    }
    Ok(lines
        .into_iter()
        .take(expected)
        .map(str::to_string)
        .collect())
}

/// 翻译一批：单请求（文本按 \n join），结果按行拆回。
pub fn translate(
    texts: &[&str],
    from: &str,
    to: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
    glossary: &[(String, String)],
) -> Result<Vec<String>, String> {
    let spec = build(texts, from, to, base_url, api_key, model, glossary);
    let body = super::send(&spec).map_err(|e| format!("openai: {e}"))?;
    parse(&body, texts.len()).map_err(|e| format!("openai: {e}"))
}
