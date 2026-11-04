//! 在线翻译 providers（google/microsoft/deepl/deeplx/openai）。
//!
//! 按 `docs/translate-spec-online.md` 实现：每个 provider 拆成 build()/parse()
//! 纯函数 + translate() 薄发送层，单测只测纯函数（不打网，见 tests/online_tests.rs）。
//!
//! 通用约定：
//! - 错误信息统一 `"<provider>: <原因>"`，中文原因；
//! - 语言映射每个 provider 一份 `map()`，未知语言码原样透传，让服务端报错如实上抛；
//! - 纯函数的可见性为 `pub` 而非规格写的 `pub(crate)`——规格 §7 要求在
//!   translate/tests/online_tests.rs（集成测试，只能看到 `pub` 项）里测它们。

pub mod deepl;
pub mod deeplx;
pub mod google;
pub mod microsoft;
pub mod openai;

use std::time::Duration;

#[derive(Debug, Clone)]
pub enum OnlineEngine {
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
        /// 术语表 (原文, 译文) 对（glossary 规格 §4）：非空时注入 system prompt。
        glossary: Vec<(String, String)>,
    },
}

/// 引擎展示名（UI 词条/设置页直接用，不走 i18n）。
pub fn engine_label(e: &OnlineEngine) -> &'static str {
    match e {
        OnlineEngine::Google => "Google",
        OnlineEngine::Microsoft => "Microsoft",
        OnlineEngine::DeepL { .. } => "DeepL",
        OnlineEngine::DeepLX { .. } => "DeepLX",
        OnlineEngine::OpenAI { .. } => "OpenAI 兼容",
    }
}

/// 在线翻译：texts 与返回一一对应；from/to 是内部两位码（provider 内部自行映射）。
pub fn translate_online(
    texts: &[&str],
    from: &str,
    to: &str,
    engine: &OnlineEngine,
) -> Result<Vec<String>, String> {
    // 空输入直接返回，避免无谓请求（也是不打网单测覆盖的分发路径）。
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    match engine {
        OnlineEngine::Google => google::translate(texts, from, to),
        OnlineEngine::Microsoft => microsoft::translate(texts, from, to),
        OnlineEngine::DeepL {
            api_key,
            pro,
        } => deepl::translate(texts, from, to, api_key, *pro),
        OnlineEngine::DeepLX { url } => deeplx::translate(texts, from, to, url),
        OnlineEngine::OpenAI {
            base_url,
            api_key,
            model,
            glossary,
        } => openai::translate(texts, from, to, base_url, api_key, model, glossary),
    }
}

/// 一次 HTTP 请求的完整描述（build() 纯函数的产物，单测直接断言字段）。
pub struct RequestSpec {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// 统一 HTTP 客户端：连接超时 10s、整体超时 30s、最多 5 跳重定向。
///
/// 规格 §0 写的是 `.proxy(Proxy::try_from_system().ok())`，但本地 ureq 2.12.1 里
/// `Proxy::try_from_system()` 是 `pub(crate)`（外部不可调），且 `AgentBuilder::proxy`
/// 只收非 Option 的 `Proxy`。等价能力是 builder 的 `try_proxy_from_env(true)`：
/// build() 内部同样走 `Proxy::try_from_system()`，尊重 ALL_PROXY/HTTPS_PROXY 等
/// 环境变量（rumi 未开 ureq 的 proxy-from-env feature，默认不探测，需显式开启）。
pub(crate) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirects(5)
        .try_proxy_from_env(true)
        .build()
}

/// 薄发送层：执行 RequestSpec，返回 2xx 响应体文本。
///
/// 错误不带 provider 前缀（由各 provider 的 translate() 包一层 `"<provider>: "`）；
/// ureq 2.x 对非 2xx 状态返回 `Err(Error::Status(code, resp))`，响应体可从错误里取出。
pub(crate) fn send(spec: &RequestSpec) -> Result<String, String> {
    let agent = agent();
    let mut req = match spec.method {
        "GET" => agent.get(&spec.url),
        "POST" => agent.post(&spec.url),
        m => return Err(format!("不支持的 HTTP 方法：{m}")),
    };
    for (k, v) in &spec.headers {
        req = req.set(k, v);
    }
    let result = match spec.body.as_deref() {
        Some(b) => req.send_string(b),
        None => req.call(),
    };
    match result {
        Ok(resp) => {
            // 正常路径 Ok 已是 2xx（重定向内部消化）；防御性再查一次状态码。
            let status = resp.status();
            let body = resp.into_string().map_err(|e| format!("读取响应体失败：{e}"))?;
            if !(200..300).contains(&status) {
                return Err(format!("HTTP {status}：{}", snippet(&body, 200)));
            }
            Ok(body)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(format!("HTTP {code}：{}", snippet(&body, 200)))
        }
        Err(e) => Err(format!("请求失败：{e}")),
    }
}

/// 截取前 max 个字符（按字符边界截，避免多字节字符串切片 panic），用于错误信息
/// 里附带响应片段。超过长度时以 "…" 结尾提示有截断。
pub(crate) fn snippet(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(max).collect();
        s.push('…');
        s
    }
}

/// 查询串百分号编码：保留 RFC 3986 unreserved（A-Za-z0-9-_.~），其余字节按
/// UTF-8 编为 %XX 大写十六进制。crate 未声明 url 依赖（不许改 Cargo.toml），手写此小段。
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 逐条请求之间的 150ms 间隔（google 免费端点限速用；其余 provider 不调）。
pub(crate) fn throttle() {
    std::thread::sleep(Duration::from_millis(150));
}
