# 规格 B：在线翻译 providers（online/）

> 工人 B。总设计 `translate-design.md`；crate 契约见 `translate-spec-core.md` §9
> （`online/mod.rs` 桩由你**整文件覆写**，公开签名逐字保留）。
> 你只动：`translate/src/online/` 目录（mod.rs 覆写 + deepl.rs deeplx.rs google.rs microsoft.rs openai.rs 新建）
> 和 `translate/tests/online_tests.rs`（新建）。**禁止**动 crate 其他文件/其他 crate。
> 依赖已由工人 A 声明（ureq/serde/serde_json），你不需要改 Cargo.toml。

## 0. 通用约束

- HTTP：统一 `fn agent() -> ureq::Agent`：
  `ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout(Duration::from_secs(30)).redirects(5).proxy(ureq::Proxy::try_from_system().ok())`
  （字段名以本地 ureq 2.x API 为准：`timeout_connect`/`timeout` 或 `timeout_connect`→`timeout`；
  读 ~/.cargo 里 ureq 源码核对，差异在回报注明。）
- **每个 provider 拆成纯函数 + 薄发送层**（为不打网单测）：
  ```rust
  pub(crate) struct RequestSpec { pub method: &'static str, pub url: String,
                                  pub headers: Vec<(String, String)>, pub body: Option<String> }
  pub(crate) fn build(texts: &[&str], from: &str, to: &str, <engine 参数>) -> RequestSpec;   // 纯
  pub(crate) fn parse(text: &str) -> Result<Vec<String>, String>;                            // 纯
  pub fn translate(texts, from, to, 参数) -> Result<Vec<String>, String>;                    // build→send→parse
  ```
- 请求间 sleep 150ms（google 逐条请求时；其余 provider 单请求不需要）。
- 错误信息统一 `"<provider>: <原因>"`，中文原因。
- 语言映射：`pub(crate) fn map(lang: &str) -> String`，**每个 provider 一份映射表**（见各节），
  未知语言码直接传原码并继续。

## 1. google.rs（免费端点，逐条请求）

- 端点：`GET https://translate.googleapis.com/translate_a/single?client=gtx&sl={sl}&tl={tl}&dt=t&q={urlencoded}`
- 映射：zh→zh-CN，en→en，ja→ja，ko→ko，fr→fr，de→de，es→es，ru→ru，pt→pt，it→it，vi→vi，ar→ar，th→th。
- 响应：嵌套数组 `[ [ ["译","原",null,null,x], ... ], ... ]`——**只取第一层 [0]**，逐元素 [0] 拼接为该条译文。
  解析用 serde_json::Value 手走，不建类型。
- 多条文本：逐条请求（texts.len() 次），150ms 间隔；任何一条失败整批报错。
- build/parse 分离：build(texts) 一次只构建单条（spec 注明 `build(text:&str,...)` 单条签名），
  translate 外层循环。Header：`User-Agent: rumi-translate/0.1`（google 对无 UA 的 gtx 偶发 429）。
- 单测 fixture（写死）：
  `[[["你好","hello",null,null,1]],null,"en",null,null,null,null,[]]` → parse 得 `["你好"]`。
  多段 fixture：`[[["世界","world",null,null,1],["！","!",null,null,1]],null,"en",null,null,null,null,[]]` → `["世界！"]`。

## 2. microsoft.rs（Edge 免费端点，token 缓存）

- token：`GET https://edge.api.cognitive.microsoft.com/authenticate?translatorTextScope=translate`
  响应头 `Authorization: Bearer <jwt>`（注意**在响应头里**，不在 body）。
- 翻译：`POST https://api-edge.cognitive.microsofttranslator.com/translate?api-version=3.0&from={from}&to={to}`
  Header：`Authorization: Bearer <jwt>`，`Content-Type: application/json`。
  Body：`[{"Text":"..."},{"Text":"..."}]`（一次带全部 texts，UTF-8）。
- 响应：`[{"translations":[{"text":"...","to":"zh-Hans"}]},...]` → 按序取 translations[0].text。
- token 缓存：进程内 `Mutex<Option<(String /*jwt*/, Instant)>>`，9 分钟过期重取。
  （单测不触发真实请求：token 逻辑包成 `fn token_valid(t:&Option<(String,Instant)>) -> bool` 纯函数测。）
- 映射：zh→zh-Hans，en→en，ja→ja，ko→ko，fr→fr，de→de，es→es，ru→ru，pt→pt，it→it，vi→vi，ar→ar，th→th。
- build 签名：`build(texts, from, to, token:&str) -> RequestSpec`。
- parse fixture（写死）：`[{"translations":[{"text":"你好","to":"zh-Hans"}]},{"translations":[{"text":"世界","to":"zh-Hans"}]}]` → `["你好","世界"]`。

## 3. deepl.rs

- `POST https://api-{free 免费版 | api 专业版}.deepl.com/v2/translate`
  Header `Authorization: DeepL-Auth-Key <key>`，`Content-Type: application/json`。
  Body：`{"text":["...","..."],"target_lang":"{T}","source_lang":"{S}"}`（text 数组原生批量）。
- 响应：`{"translations":[{"detected_source_language":"EN","text":"..."},...]}`。
- 映射（大写）：zh→ZH，en→EN，ja→JA，ko→KO，fr→FR，de→DE，es→ES，ru→RU，pt→PT，it→IT；vi/ar/th 无（传原码，让服务端报错如实透传）。
- parse fixture（写死）：`{"translations":[{"detected_source_language":"EN","text":"你好"},{"detected_source_language":"EN","text":"世界"}]}` → `["你好","世界"]`。

## 4. deeplx.rs（自建服务）

- `POST {url}`（用户给的完整 URL，默认 `http://127.0.0.1:1188/translate`）
  Body：`{"text":"...","source_lang":"{S}","target_lang":"{T}"}`（DeepLX 单文本协议；
  多条 = 逐条请求，无间隔）。
- 响应：`{"code":200,"data":"..."}` → data。code!=200 或缺 data → 报错带返回原文片段（前 200 字）。
- 映射同 deepl 大写表。
- parse fixture：`{"code":200,"data":"你好"}` → `["你好"]`；错误 fixture `{"code":429,"message":"Too Many Requests"}` → Err 含 "429"。

## 5. openai.rs（OpenAI 兼容 /chat/completions）

- `POST {base_url}/chat/completions`（base_url 已含 /v1 由用户负责；url = trim_end_matches('/') + "/chat/completions"）
  Header：`Authorization: Bearer <key>`，`Content-Type: application/json`。
  Body（serde_json 构造，pretty 无所谓）：
  ```json
  {"model": M, "temperature": 0.0,
   "messages": [
     {"role": "system", "content": "You are a translation engine. Translate the user text from <from_name> to <to_name>. Output ONLY the translation with original line breaks preserved. No explanations."},
     {"role": "user", "content": "<texts 以 \n join>"}]}
  ```
  `<from_name>/<to_name>` 用 `crate::lang::display`。
- 响应：`choices[0].message.content`，按 `\n` split 回 texts.len() 份；**份数不等时不强行对齐**：
  少于请求份数 → 报错 "返回行数不符（期望 N 得到 M）"；多于 → 多余丢弃并继续。
- model 空串默认 "gpt-4o-mini"；api_key 可为空（本地 ollama 类服务无鉴权——不传 Authorization 头当 key 为空）。
- parse fixture（写死）：
  `{"choices":[{"message":{"content":"你好\n世界"}}]}` for 2 条 → `["你好","世界"]`。

## 6. mod.rs（覆写桩）

解开 5 个 `pub mod`；`OnlineEngine` 枚举逐字保留；`translate_online` 按枚举分发到各 provider，
texts 为空 → 返回空 Vec。提供：
```rust
pub fn engine_label(e: &OnlineEngine) -> &'static str; // "Google"/"Microsoft"/"DeepL"/"DeepLX"/"OpenAI 兼容"
```

## 7. tests/online_tests.rs（全部不打网）

- 每个 provider：build() 的 URL/headers/body 断言（含 urlencoded、join 规则、base_url 拼接）+ parse() 全部 fixture + 至少 1 个坏输入（缺字段/坏 JSON → Err）。
- 映射表：每 provider 3 条样例（zh 特化值 + en + 未知码透传）。
- token_valid：有效/过期两态。
- mod 分发：texts 空 → Ok(vec![])（此路径不打网）。

## 8. 回报格式

改动文件清单；ureq 实际 API 与规格差异；cargo test 输出关键行；歧义点清单。
