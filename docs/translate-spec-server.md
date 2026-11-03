# 规格 E：server /api/translate（rumi-server）

> 工人 E（阶段二，待阶段一合并后开工）。总设计 `translate-design.md`。
> 你只动：`server/src/http.rs`、`server/src/lib.rs`（若需）、`server/Cargo.toml`、
> `server/tests/`（新增 translate_api_tests.rs）。
> **禁止**动 translate crate / ui / 其他 crate。现有风格先读 `server/src/http.rs` 与 `server/tests/http_tests.rs`。

## 1. Cargo.toml

`server/Cargo.toml` 追加 `rumi-translate = { path = "../translate" }`。

## 2. 路由（http.rs，build_router 内追加）

```rust
.route("/api/translate", post(api_translate))
.route("/api/translate/models", get(api_translate_models))
.route("/api/translate/models/install", post(api_translate_models_install))
```

## 3. POST /api/translate

请求体（serde struct，全字段见下；缺省语义注明）：

```rust
#[derive(Deserialize)]
struct TranslateBody {
    text: Option<String>,          // 与 texts 二选一；两者都给 → 400
    texts: Option<Vec<String>>,
    from: Option<String>,          // 默认 "en"
    to: Option<String>,            // 默认 "zh"
    engine: Option<String>,        // 默认 "offline"（offline|google|microsoft|deepl|deeplx|openai）
    polish: Option<bool>,          // 默认 true（仅 to=zh 生效）
    // 在线引擎参数（engine 选对应项时才使用；都给全也没关系）
    deepl_api_key: Option<String>,
    deepl_pro: Option<bool>,
    deeplx_url: Option<String>,
    openai_base_url: Option<String>,
    openai_api_key: Option<String>,
    openai_model: Option<String>,
}
```

响应（对齐现有 api_ocr 的 JSON 风格——先看现有 handler 用 (StatusCode, Json) 还是别的封装再照抄）：

```json
{"texts": ["译文1"], "engine": "offline", "elapsed_ms": 123}
```

错误（照抄现有错误响应模式；若现有用 `{"code":..,""}` 结构则跟随，无则统一 400/500 + `{"error":"..."}`）：
- 校验错（text/texts 都缺、引擎名未知）：400；
- 离线模型未安装：404 语义（若现有无区分则 400，error 文案透传 engine 错误 Display）；
- 在线网络失败：502。

实现：`tokio::task::spawn_blocking` 包 `rumi_translate::translate(...)`（现有 api_ocr 同款）。
texts 空 → `{"texts":[],"engine":...,"elapsed_ms":0}`。from==to 直接原样返回（facade 已处理，无需特判）。

## 4. GET /api/translate/models

```json
{"models": [
  {"key":"zh_en","from":"zh","to":"en","size_mb":150,"installed":true},
  {"key":"en_zh","from":"en","to":"zh","size_mb":150,"installed":false},
  {"imported": [{"key":"my_pkg","from":"en","to":"zh","bytes": 162000000}]}
]}
```

实现：registry::MODELS + `rumi_translate::shared_engine().installed()`（imported 来自 installed()
中不在 registry 的条目——字段名以 translate crate 实际 InstalledInfo 为准，需 serde::Serialize）。
若 InstalledInfo 未实现 Serialize，则在 server 侧手工组 JSON（不回头改 translate crate）。

## 5. POST /api/translate/models/install

```json
{"key": "en_zh"}
```

- spawn_blocking 调 `download::install(spec, &models_dir(), &|_| {})`（进度不上报，同步装完）。
- 成功 `{"ok": true, "key": "en_zh"}`；未知 key 400；下载失败 502 + error 文案。
- 大文件分钟级：handler 上方注释注明「客户端请自设超时」。

## 6. tests/translate_api_tests.rs（不打真网）

模式照抄 http_tests.rs（构造 Router + tokio 测试端点直调或 reqwest 本地 bind——以现有测试基建为准）：

1. text 与 texts 同给 → 400；都缺 → 400。
2. engine 未知 → 400。
3. 离线翻译（模型目录指到空 tempdir：**如何注入**？看 server 现有 Recognizer 构造方式；
   translate 侧模型目录由 `RUMI_TRANSLATE_MODELS_DIR` 环境变量决定——测试在**进程内先 set_var 再**
   构造 router；edition 2024 set_var 为 unsafe，测试里 `unsafe {}` + 注释「测试串行执行，早于 shared() 初始化」。
   若现有测试并发（tokio::test 并行），改用 `--test-threads` 不可控——**备选方案**：跳过注入，
   断言「返回 400 且 error 含『未安装』或『暂无』」的前提是本机未装模型（CI/干净环境成立；
   装过模型的环境该用例会真翻译——测试名注明 `model_absent`，回报注明取舍）。
4. from==to → 原样返回 200。
5. GET models → 200 且 models 数组含 zh_en/en_zh 两 key。
6. install 未知 key → 400。

## 7. 验收

- `cargo check -p rumi-server`、`cargo test -p rumi-server` 全绿（原有 http_tests 不回归）。
- 冒烟（回报贴输出）：
  `cargo run -p rumi-server &` →
  `curl -s 127.0.0.1:1224/api/translate -H 'content-type: application/json' -d '{"text":"The default port is 8080."}'`
  （模型未装时预期 400/404 + 未安装文案；装好后再跑一次预期译文 JSON。）
  `curl -s 127.0.0.1:1224/api/translate/models`

## 8. 回收格式

改动文件清单；错误响应最终形态（跟随现有风格的说明）；测试输出关键行；冒烟输出；歧义点清单。
