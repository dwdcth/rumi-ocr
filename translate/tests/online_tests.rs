//! 在线 provider 纯函数单测（build/parse/map/token_valid/分发），全部不打网。
//!
//! 覆盖 `docs/translate-spec-online.md` §7：
//! - 每个 provider：build() 的 URL/headers/body 断言（含 urlencoded、批量 join、
//!   base_url 拼接）+ parse() 全部规格 fixture + 至少 1 个坏输入；
//! - 映射表每 provider 3 条样例（zh 特化值 + en + 未知码透传）；
//! - microsoft token_valid 有效/过期两态；
//! - translate_online 空 texts → Ok(vec![])（早退路径，不打网）。
//!
//! openai 的 build() 最终入口会调 crate::lang::display（工人 A 并行实现，此刻是
//! todo!() 桩，调用即 panic），故本文件对 openai 走 build_with_names 常量展示名
//! 版本断言——URL/headers/body 覆盖完整，仅 display 组合那一行未被本文件执行。

use rumi_translate::online::{
    deepl, deeplx, google, microsoft, openai, translate_online, OnlineEngine, RequestSpec,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 从 RequestSpec 里按名（大小写不敏感）找 header。
fn header<'a>(spec: &'a RequestSpec, name: &str) -> Option<&'a str> {
    spec.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// body 解析成 Value 再断言：不受 serde_json 对象键序（BTreeMap 排序）影响。
fn body_json(spec: &RequestSpec) -> Value {
    serde_json::from_str(spec.body.as_deref().expect("断言用例的 body 应为 Some"))
        .expect("断言用例的 body 应为合法 JSON")
}

// ---------- google ----------

#[test]
fn google_map_zh_en_unknown() {
    assert_eq!(google::map("zh"), "zh-CN");
    assert_eq!(google::map("en"), "en");
    assert_eq!(google::map("xx"), "xx"); // 未知码透传
}

#[test]
fn google_build_url_headers() {
    let spec = google::build("hello world", "zh", "en");
    assert_eq!(spec.method, "GET");
    assert_eq!(
        spec.url,
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl=zh-CN&tl=en&dt=t&q=hello%20world"
    );
    assert_eq!(header(&spec, "User-Agent"), Some("rumi-translate/0.1"));
    assert!(spec.body.is_none());
}

#[test]
fn google_build_urlencoded_utf8() {
    // "你好" 的 UTF-8 字节 E4 BD A0 E5 A5 BD 逐字节百分号编码。
    let spec = google::build("你好", "en", "zh");
    assert!(spec.url.ends_with("q=%E4%BD%A0%E5%A5%BD"), "url={}", spec.url);
    assert!(spec.url.contains("sl=en&tl=zh-CN&dt=t&"));
}

#[test]
fn google_parse_single_fixture() {
    let out = google::parse(r#"[[["你好","hello",null,null,1]],null,"en",null,null,null,null,[]]"#)
        .expect("fixture 应解析成功");
    assert_eq!(out, vec!["你好"]);
}

#[test]
fn google_parse_multiseg_joined() {
    let out = google::parse(
        r#"[[["世界","world",null,null,1],["！","!",null,null,1]],null,"en",null,null,null,null,[]]"#,
    )
    .expect("fixture 应解析成功");
    assert_eq!(out, vec!["世界！"]);
}

#[test]
fn google_parse_bad_inputs() {
    assert!(google::parse("not json").is_err());
    // 顶层是对象：缺少 [0] 数组。
    assert!(google::parse(r#"{"x":1}"#).is_err());
    // [0] 不是数组。
    assert!(google::parse(r#"[null,null]"#).is_err());
    // 分段元素 [0] 不是字符串。
    assert!(google::parse(r#"[[[1,2]]]"#).is_err());
}

// ---------- microsoft ----------

#[test]
fn microsoft_map_zh_en_unknown() {
    assert_eq!(microsoft::map("zh"), "zh-Hans");
    assert_eq!(microsoft::map("en"), "en");
    assert_eq!(microsoft::map("xx"), "xx"); // 未知码透传
}

#[test]
fn microsoft_build_batch_body_and_headers() {
    let spec = microsoft::build(&["你好", "世界"], "zh", "en", "jwt");
    assert_eq!(spec.method, "POST");
    assert_eq!(
        spec.url,
        "https://api-edge.cognitive.microsofttranslator.com/translate?api-version=3.0&from=zh-Hans&to=en"
    );
    assert_eq!(header(&spec, "Authorization"), Some("Bearer jwt"));
    assert_eq!(header(&spec, "Content-Type"), Some("application/json"));
    assert_eq!(body_json(&spec), json!([{"Text": "你好"}, {"Text": "世界"}]));
}

#[test]
fn microsoft_token_valid_states() {
    assert!(microsoft::token_valid(&Some(("jwt".into(), Instant::now()))));
    // 过期（10 分钟前取得，超过 9 分钟有效期）。
    let expired = Instant::now()
        .checked_sub(Duration::from_secs(600))
        .expect("测试环境的单调时钟应允许回退 10 分钟");
    assert!(!microsoft::token_valid(&Some(("jwt".into(), expired))));
    // 无缓存 / 空 token 都不算有效。
    assert!(!microsoft::token_valid(&None));
    assert!(!microsoft::token_valid(&Some((String::new(), Instant::now()))));
}

#[test]
fn microsoft_parse_fixture() {
    let out = microsoft::parse(
        r#"[{"translations":[{"text":"你好","to":"zh-Hans"}]},{"translations":[{"text":"世界","to":"zh-Hans"}]}]"#,
    )
    .expect("fixture 应解析成功");
    assert_eq!(out, vec!["你好", "世界"]);
}

#[test]
fn microsoft_parse_bad_inputs() {
    assert!(microsoft::parse("not json").is_err());
    // 顶层不是数组。
    assert!(microsoft::parse(r#"{"translations":[]}"#).is_err());
    // 元素缺 translations。
    assert!(microsoft::parse(r#"[{"nope":1}]"#).is_err());
    // translations 为空数组。
    assert!(microsoft::parse(r#"[{"translations":[]}]"#).is_err());
}

// ---------- deepl ----------

#[test]
fn deepl_map_zh_en_passthrough() {
    assert_eq!(deepl::map("zh"), "ZH");
    assert_eq!(deepl::map("en"), "EN");
    assert_eq!(deepl::map("vi"), "vi"); // 表外原码透传，让服务端报错
}

#[test]
fn deepl_build_free_vs_pro() {
    let free = deepl::build(&["Hello"], "en", "zh", "k", false);
    assert_eq!(free.url, "https://api-free.deepl.com/v2/translate");
    let pro = deepl::build(&["Hello"], "en", "zh", "k", true);
    assert_eq!(pro.url, "https://api.deepl.com/v2/translate");

    assert_eq!(header(&free, "Authorization"), Some("DeepL-Auth-Key k"));
    assert_eq!(header(&free, "Content-Type"), Some("application/json"));
    assert_eq!(
        body_json(&free),
        json!({"text": ["Hello"], "target_lang": "ZH", "source_lang": "EN"})
    );
}

#[test]
fn deepl_parse_fixture() {
    let out = deepl::parse(
        r#"{"translations":[{"detected_source_language":"EN","text":"你好"},{"detected_source_language":"EN","text":"世界"}]}"#,
    )
    .expect("fixture 应解析成功");
    assert_eq!(out, vec!["你好", "世界"]);
}

#[test]
fn deepl_parse_bad_inputs() {
    assert!(deepl::parse("not json").is_err());
    // 缺 translations。
    assert!(deepl::parse(r#"{"nope":1}"#).is_err());
    // translations 不是数组。
    assert!(deepl::parse(r#"{"translations":"x"}"#).is_err());
    // 元素缺 text。
    assert!(deepl::parse(r#"{"translations":[{}]}"#).is_err());
}

// ---------- deeplx ----------

#[test]
fn deeplx_map_zh_en_unknown() {
    assert_eq!(deeplx::map("zh"), "ZH");
    assert_eq!(deeplx::map("en"), "EN");
    assert_eq!(deeplx::map("xx"), "xx"); // 未知码透传
}

#[test]
fn deeplx_build_single_text() {
    let spec = deeplx::build("你好", "zh", "en", "http://127.0.0.1:9999/translate");
    assert_eq!(spec.method, "POST");
    assert_eq!(spec.url, "http://127.0.0.1:9999/translate");
    assert_eq!(header(&spec, "Content-Type"), Some("application/json"));
    assert_eq!(
        body_json(&spec),
        json!({"text": "你好", "source_lang": "ZH", "target_lang": "EN"})
    );
}

#[test]
fn deeplx_build_default_url_when_empty() {
    let spec = deeplx::build("hi", "en", "zh", "");
    assert_eq!(spec.url, "http://127.0.0.1:1188/translate");
}

#[test]
fn deeplx_parse_fixture() {
    let out = deeplx::parse(r#"{"code":200,"data":"你好"}"#).expect("fixture 应解析成功");
    assert_eq!(out, vec!["你好"]);
}

#[test]
fn deeplx_parse_error_code_with_snippet() {
    let err = deeplx::parse(r#"{"code":429,"message":"Too Many Requests"}"#)
        .expect_err("code!=200 应报错");
    assert!(err.contains("429"), "错误应含状态码：{err}");
}

#[test]
fn deeplx_parse_bad_inputs() {
    assert!(deeplx::parse("not json").is_err());
    // 缺 code。
    assert!(deeplx::parse(r#"{"data":"x"}"#).is_err());
    // code=200 但缺 data。
    assert!(deeplx::parse(r#"{"code":200}"#).is_err());
}

// ---------- openai ----------

#[test]
fn openai_build_url_join_and_base_url() {
    let spec = openai::build_with_names(&["a", "b"], "中文", "English", "http://127.0.0.1:11434/v1", "", "m1");
    assert_eq!(spec.url, "http://127.0.0.1:11434/v1/chat/completions");

    // base_url 尾部斜杠被清理。
    let spec = openai::build_with_names(&["a"], "中文", "English", "http://x/v1/", "", "m1");
    assert_eq!(spec.url, "http://x/v1/chat/completions");
}

#[test]
fn openai_build_auth_header_only_with_key() {
    // key 为空（本地 ollama 类无鉴权）：不传 Authorization。
    let no_key = openai::build_with_names(&["a"], "中文", "English", "http://x/v1", "", "m");
    assert_eq!(header(&no_key, "Authorization"), None);
    assert_eq!(header(&no_key, "Content-Type"), Some("application/json"));

    let with_key = openai::build_with_names(&["a"], "中文", "English", "http://x/v1", "sk-test", "m");
    assert_eq!(header(&with_key, "Authorization"), Some("Bearer sk-test"));
}

#[test]
fn openai_build_body_prompt_and_join() {
    let spec = openai::build_with_names(&["a", "b"], "中文", "English", "http://x/v1", "", "m1");
    let v = body_json(&spec);
    assert_eq!(v["model"], "m1");
    assert_eq!(v["temperature"], 0.0);
    assert_eq!(
        v["messages"][0],
        json!({
            "role": "system",
            "content": "You are a translation engine. Translate the user text from 中文 to English. Output ONLY the translation with original line breaks preserved. No explanations."
        })
    );
    // texts 以 \n join。
    assert_eq!(v["messages"][1], json!({"role": "user", "content": "a\nb"}));
}

#[test]
fn openai_build_default_model_when_empty() {
    let spec = openai::build_with_names(&["a"], "中文", "English", "http://x/v1", "", "");
    assert_eq!(body_json(&spec)["model"], "gpt-4o-mini");
}

#[test]
fn openai_parse_fixture_two_lines() {
    let out = openai::parse(r#"{"choices":[{"message":{"content":"你好\n世界"}}]}"#, 2)
        .expect("fixture 应解析成功");
    assert_eq!(out, vec!["你好", "世界"]);
}

#[test]
fn openai_parse_extra_lines_dropped() {
    // 多于请求份数：多余行丢弃。
    let out = openai::parse(r#"{"choices":[{"message":{"content":"你好\n世界\n多余"}}]}"#, 2)
        .expect("应解析成功");
    assert_eq!(out, vec!["你好", "世界"]);
}

#[test]
fn openai_parse_fewer_lines_is_err() {
    let err = openai::parse(r#"{"choices":[{"message":{"content":"你好"}}]}"#, 2)
        .expect_err("行数不足应报错");
    assert!(err.contains("返回行数不符（期望 2 得到 1）"), "err={err}");
}

#[test]
fn openai_parse_bad_inputs() {
    assert!(openai::parse("not json", 1).is_err());
    // choices 为空。
    assert!(openai::parse(r#"{"choices":[]}"#, 1).is_err());
    // 缺 message。
    assert!(openai::parse(r#"{"choices":[{}]}"#, 1).is_err());
    // content 不是字符串。
    assert!(openai::parse(r#"{"choices":[{"message":{"content":1}}]}"#, 1).is_err());
}

// ---------- mod 分发 ----------

#[test]
fn dispatch_empty_texts_ok_empty_for_all_engines() {
    // 空 texts 早退，不打网；五种引擎都覆盖。
    for engine in [
        OnlineEngine::Google,
        OnlineEngine::Microsoft,
        OnlineEngine::DeepL { api_key: "k".into(), pro: false },
        OnlineEngine::DeepLX { url: "http://127.0.0.1:1188/translate".into() },
        OnlineEngine::OpenAI { base_url: "http://x/v1".into(), api_key: String::new(), model: String::new() },
    ] {
        let out = translate_online(&[], "zh", "en", &engine).expect("空 texts 应返回 Ok");
        assert!(out.is_empty());
    }
}

#[test]
fn engine_labels() {
    assert_eq!(
        rumi_translate::online::engine_label(&OnlineEngine::Google),
        "Google"
    );
    assert_eq!(
        rumi_translate::online::engine_label(&OnlineEngine::Microsoft),
        "Microsoft"
    );
    assert_eq!(
        rumi_translate::online::engine_label(&OnlineEngine::DeepL {
            api_key: String::new(),
            pro: false
        }),
        "DeepL"
    );
    assert_eq!(
        rumi_translate::online::engine_label(&OnlineEngine::DeepLX { url: String::new() }),
        "DeepLX"
    );
    assert_eq!(
        rumi_translate::online::engine_label(&OnlineEngine::OpenAI {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new()
        }),
        "OpenAI 兼容"
    );
}
