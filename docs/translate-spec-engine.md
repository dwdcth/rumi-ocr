# 规格 C：离线引擎（engine.rs）+ CLI（rumi-translate-cli）+ E2E 测试

> 工人 C。总设计 `translate-design.md`；crate 骨架/类型契约见 `translate-spec-core.md`（§8 的桩由你**整文件覆写**）。
> 你只动：`translate/src/engine.rs`（覆写桩）、`translate/src/bin/rumi_translate_cli.rs`（新建）、
> `translate/tests/e2e.rs`（新建）。**禁止**动 lib.rs/其他模块/其他 crate。
> ct2rs 首次编译 10~20 分钟正常，不要 clean，不要反复全量重建；用 `cargo check -p rumi-translate` 增量。

## 1. engine.rs（覆写桩，保持桩的公开签名逐字不变）

### 结构

```rust
struct Loaded {
    translator: ct2rs::Translator<ct2rs::tokenizers::auto::Tokenizer>, // 或等价泛型收尾
    loaded_at: std::time::Instant,
    last_used: std::time::Instant,
}

pub struct OfflineEngine {
    models_dir: PathBuf,
    compute: Compute,
    inner: std::sync::Mutex<HashMap<String, Arc<Loaded>>>,  // key: 模型 key 或导入包目录名
}
```

### 加载逻辑（`fn load(&self, from,to) -> Result<Arc<Loaded>, EngineError>`，内部持锁）

1. `registry::find(from,to)`：None → 若 `models_dir` 下存在**导入的 Argos 包**能对上方向
   （读各子目录 manifest.json 的 from/to 字段匹配）则用之；都没有 → `NoBuiltinModel`。
2. 模型目录 = `models_dir/<key>`；`download::is_complete` 为假 → `ModelNotInstalled { from, to }`。
3. 打开 ct2rs：
   ```rust
   let cfg = ct2rs::Config {
       device: ct2rs::Device::CPU,
       device_indices: vec![0],
       inter_threads: 1,          // 文章实测：inter=1 最稳
       intra_threads: 0,          // 0 = CT2 自行决定（默认 4）
       compute_type: match self.compute {
           Compute::Int8 => ct2rs::ComputeType::Int8,       // fp16 权重加载时动态转 int8
           Compute::Float32 => ct2rs::ComputeType::Float32, // 对照档（内存↑）
       },
       ..Default::default()
   };
   let t = ct2rs::Translator::new(&model_dir, &cfg).map_err(|e| EngineError::Load(e.to_string()))?;
   ```
   （字段名以本地 cargo doc / 源码为准——`cargo doc -p ct2rs --no-deps` 或直接读
   `~/.cargo/registry/src/*/ct2rs-*/src/lib.rs` 核对 Config 字段拼写；若 ct2rs 版本字段不同，
   按实际 API 调整并在回报里注明差异。`Translator::new` 自动探测 source.spm/target.spm。）
4. 缓存进 map（先查缓存命中直接返回 Arc）。

### 翻译逻辑（`translate`）

1. `from == to` → 原文返回。
2. `segment::segment_lines(text)` → 行×段。收集全部非空段（trim 后非空）为 `Vec<String>`。
3. `translator.translate_batch(&sources, &Default::default(), None)`
   → `Vec<(String, Vec<Score>)>` 取 .0。CT2 对空序列会报错，所以第 2 步必须滤空。
   错误 → `EngineError::Infer(e.to_string())`。
4. 按行重排回去：每行的段按序取结果，行内段以空串 join（段边界不留分隔符——
   中文无空格；若 to 是拉丁语系用单空格 join：`if to == "zh" { "" } else { " " }`），
   行间以 `\n` join，行数与输入严格一致。
5. 更新 last_used。

### 卸载（`unload_idle`）

锁内 retain：`loaded.loaded_at.elapsed() > idle && last_used.elapsed() > idle` 的条目删除
（正在翻译中的调用持有 Arc，drop 自然释放）。**不在** unload_idle 里做定时器——由调用方
（UI 每分钟 tick / CLI 进程退出）驱动。注意：条目正在被 Arc 使用也能删（map 删引用，不拆使用中实例）。

### installed / is_installed / shared

- `installed()`：扫 models_dir 一级子目录，读 manifest.json（有 registry key 的带 from/to/bytes；
  导入包按 manifest 实际字段，bytes = 目录文件总大小）。
- `is_installed(from,to)`：registry key complete 或导入包方向匹配。
- `shared()`：`static S: OnceLock<OfflineEngine>`，`get_or_init(|| OfflineEngine::new(crate::models_dir(), Compute::Int8))`。

### Display for EngineError（用户可读中文）

- ModelNotInstalled → `语言包未安装：{from}→{to}，请先在翻译页下载`
- NoBuiltinModel → `{from}→{to} 暂无内置模型（可从翻译页导入 Argos 语言包）`
- Load → `模型加载失败：{0}`
- Infer → `推理失败：{0}`

### engine 单元测试（#[cfg(test)] 模块内，不依赖模型文件）

- `(from,to)→目录解析` 的纯逻辑：把「registry 命中/未命中/is_complete 判定」抽成
  `fn resolve_dir(models_dir:&Path, from:&str, to:&str) -> Resolution` 纯函数，
  对 tempdir 摆假 manifest 测三种 Resolution（RegistryFound / ImportedFound / NotFound / NotInstalled 四种）。
- unload_idle：tempdir 假目录 + 手工往 map 塞条目（需要测试钩子：`#[cfg(test)] fn insert_for_test`）
  ——若结构不便，允许改为只测 `Resolution` 与 Display，unload_idle 靠 E2E 覆盖（在回报注明取舍）。

## 2. bin/rumi_translate_cli.rs（新建）

无 clap（工作区风格：手写解析）。用法（`--help` 输出与下述一致，中文）：

```
rumi-translate-cli —— 离线/在线翻译命令行

用法：
  rumi-translate-cli [选项] [文本]        无文本参数且 stdin 非 tty 时读 stdin
选项：
  --from <lang>    源语言码（默认 en）
  --to <lang>      目标语言码（默认 zh）
  --engine <name>  offline(默认) | google | microsoft | deepl | deeplx | openai
  --api-key <k>    deepl/openai 密钥
  --pro            deepl 专业版端点
  --url <u>        deeplx 服务地址 / openai base_url
  --model <m>      openai 模型名（默认 gpt-4o-mini）
  --file <path>    从文件读文本（UTF-8）
  --no-polish      关闭 zh 译文标点润色
  --json           输出 {"engine":"...","elapsed_ms":N,"texts":[...]}
  --install <key>  安装离线语言包（zh_en / en_zh），进度打 stderr，装完退出
  --list-models    列出内置模型与安装状态后退出
  --models-dir <d> 覆盖模型目录
  --help           本帮助
```

行为细节：
- `--install`：`download::install(registry::by_key(key)?, dir, &Mirrors::default(), &|p| eprintln!(...))`
  （主源 argos-net 直链 ~71MB，失败自动回退 HF 备源）。
- 离线引擎实例：`OfflineEngine::new(dir, Compute::Int8)`（非 shared，CLI 短命进程）。
- 在线引擎枚举从参数构造；google/microsoft 无需参数。
- 输出：非 --json 时逐行 println 译文；错误 eprintln 后 exit code 1。
- `--models-dir` 同时影响 install 与离线翻译。

## 3. tests/e2e.rs（真实网络/模型，全部 #[ignore]）

```rust
//! RUMI_E2E=1 cargo test -p rumi-translate --test e2e -- --ignored
//! 需要网络（huggingface.co 直连或 hf-mirror）。

#[test]
#[ignore = "真实下载 en_zh 模型 ~157MB"]
fn e2e_en_zh_offline() {
    let dir = std::env::temp_dir().join("rumi-translate-e2e-en_zh");
    let spec = registry::by_key("en_zh").unwrap();
    if !download::is_complete(spec, &dir) {
        download::install(spec, &dir, &|_| {}).unwrap();
    }
    let eng = OfflineEngine::new(dir.clone(), Compute::Int8);
    let out = eng.translate("The default port is 8080. Set the environment variable first.", "en", "zh").unwrap();
    assert!(out.contains("8080"), "译文应保留端口号：{out}");
    // 润色路径（facade 层）
    let outs = translate(&[s!("Done. The port is 8080.")], "en", "zh", &Engine::Offline, true).unwrap();
    assert!(outs[0].contains("8080"));
}

#[test]
#[ignore = "真实下载 zh_en 模型 ~157MB"]
fn e2e_zh_en_offline() { /* 同构：输入 "请在启动 Docker 容器前设置环境变量。默认端口为 8080。" 断言含 "8080" */ }

#[test]
#[ignore = "需可访问 translate.googleapis.com"]
fn e2e_google_online() {
    let out = translate(&["The default port is 8080.".to_string()], "en", "zh", &Engine::Google, true).unwrap();
    assert!(!out[0].is_empty());
}
```

（e2e 走 facade `rumi_translate::translate` 时模型目录用 `std::env::set_var("RUMI_TRANSLATE_MODELS_DIR", &dir)`——
  set_var 在 edition 2024 是 unsafe：用 `unsafe { std::env::set_var(...) }` 并注释原因（测试单进程内先于 shared() 使用）；
  或者不设 env、直接 new OfflineEngine 调 translate 逻辑绕过 facade——**采用后者**：e2e 里直接用 OfflineEngine，
  facade 的 google 用例无需模型。）

## 4. 验收

- `cargo check -p rumi-translate` 通过；`cargo test -p rumi-translate`（不含 ignored）全绿。
- 手动冒烟（你自己跑一次并把输出贴回报）：
  `cargo run -p rumi-translate --bin rumi-translate-cli -- --install en_zh --models-dir /tmp/rt-e2e`
  然后 `cargo run -p rumi-translate --bin rumi-translate-cli -- --models-dir /tmp/rt-e2e --to zh "The default port is 8080."`
  （网络不通时如实回报，不算失败——门禁以单测+编译为准，E2E 留给监工复验。）

## 5. 回报格式

改动文件清单；ct2rs 实际 API 与规格差异清单（Config 字段名、Translator 泛型写法等）；
E2E 冒烟输出（成功或网络失败说明）；歧义点清单。
