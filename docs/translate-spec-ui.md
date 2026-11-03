# 规格 D：UI 翻译页 + OCR 联动 + 设置（rumi-ui）

> 工人 D（阶段二，待阶段一合并后开工）。总设计 `translate-design.md`。
> 你只动：`ui/src/translate_ui.rs`（新建）、`ui/src/main.rs`、`ui/src/settings.rs`、
> `ui/assets/i18n/zh-CN.json`、`ui/assets/i18n/en-US.json`、`ui/src/icon.rs`（若需新图标）。
> **禁止**动 translate crate / server / 其他模块。Dioxus 坑清单先读 `docs/开发经验.md` §二。

## 1. 导航与 tab

- `main.rs`：`TAB_TRANSLATE: usize = 4`；侧边导航顺序：主界面、批量、**翻译**、插件、设置
  （在现有 nav 区（约 622-634 行）的 BATCH 项后插入翻译项）。RUMI_START_TAB 注释更新为
  `1=插件 2=设置 3=批量 4=翻译`。tab 内容分发区（约 1012-1021 行）加 `if tab_v == TAB_TRANSLATE { ... }`，
  渲染 `translate_ui::TranslatePage` 组件。
- 图标：`icon.rs` 现有图标列表里挑「译」或新增内联 SVG（两字符交叠的翻译样式，
  16×16 stroke 风格与现有一致；新增则 key `translate`）。

## 2. 跨页通信（main.rs 与 translate_ui.rs）

main.rs 增加全局 Signal（与现有 EngineState 同层声明）：

```rust
/// 翻译页共享状态：OCR 结果跳转预填用。
struct TranslatePrefill { text: String, run: bool }
// main.rs 里: use dioxus::signal::Signal 语法按现有 Signal 用法照抄
pub(crate) fn translate_prefill() -> Signal<TranslatePrefill> // 语义参考现有 clipboard() 单例模式，或直接在 App 作用域声明后经 Context 传递——与现有代码风格一致者优先
```

（实现方式跟随 main.rs 现有 Signal 组织方式；若 main.rs 用 #[derive(Signal)]/ContextProvider 模式照抄。
要求：OCR 页按钮与翻译页组件都能读写同一实例。）

## 3. 翻译页（translate_ui.rs）

布局（沿用批量页/主界面的卡片与工具行 class：`card`/`toolbar`/`btn`/`btn-sm`/`select` 等现有 CSS 类，**不新造风格**）：

```
┌ 翻译 ────────────────────────────────────────────────┐
│ [源语言 select][⇄][目标语言 select] [引擎 select] [翻译 btn] │
│ ┌ 源文本 textarea（10 行，placeholder=粘贴或输入要翻译的文本…）─┐ │
│ ├ 结果 textarea（8 行，只读）+ 右上角 [复制 btn-sm] + 耗时标注 │ │
│ └──────────────────────────────────────────────────┘ │
│ ── 离线语言包 ──────────────────────────────────────── │
│ 中文→英语   150 MB   [已安装 ✓]                        │
│ 英语→中文   150 MB   [下载] [导入语言包…]  ▓▓▓▓░░ 42% · 66/157MB │
└──────────────────────────────────────────────────────┘
```

行为（逐条写死）：
1. 源/目标语言下拉：`lang::UI_LANGS` 逐项 `display()`；选中值持久化到 settings
   `translate.last_from/last_to`（变更即 save，模式照抄现有设置保存）。
2. ⇄ 交换按钮：交换两下拉 + 交换两 textarea 内容。
3. 引擎下拉选项与禁用规则：
   - 离线（value=offline）恒可用；
   - Google（google）/Microsoft（microsoft）恒可选；
   - DeepL：settings 有 deepl_api_key 才可选，否则 disabled 显示「DeepL（需配置）」；
   - DeepLX：settings.deeplx_url 非空才可选；
   - OpenAI 兼容：openai_base_url 非空才可选；
   - disabled 项旁给「去设置」小链接 btn-sm → `tab.set(TAB_SETTINGS)`。
   - 默认选中 settings.translate.engine。
4. 翻译按钮：std 线程跑 `rumi_translate::translate(...)`（oneshot 回 dioxus spawn，模式照抄
   ocr_flow 的后台线程写法）；运行中按钮 disabled + 文案「翻译中…」。
   构造 Engine：offline→Engine::Offline；google→Google；microsoft→Microsoft；
   deepl→DeepL{key,pro}；deeplx→DeepLX{url}；openai→OpenAI{...}，字段取 settings。
   polish 取 settings.translate.polish。错误 → 结果区显示错误文案（红字 var(--danger) 现有变量），
   「语言包未安装」错误额外在下方模型行高亮对应条目。
5. 耗时标注：完成后结果区右上 `12.3 s`（.elapsed()）。
6. 离线语言包块：遍历 `registry::MODELS`，每行：`display(from)→display(to)`、`size_mb MB`、
   状态（启动时与每次操作后经 `shared_engine().installed()` 刷新，线程安全读）。
   - [下载]：std 线程 `download::install(spec, &models_dir(), &mirrors_from_settings(&settings), &|p| …)`，
     进度回调经 channel/Arc<Mutex> 回 UI 更新进度条（进度条用现有 CSS 或简易 div width%——照抄批量页进度样式；
     无现成则 `div style="width:{pct}%"` 内联，高 4px 背景色 var(--primary)）。
   - [导入语言包…]：rfd 文件对话框（**std 线程调用**，开发经验 §三：rfd 同步对话框 Linux 死锁主线程），
     过滤 `.argosmodel`，选中后 std 线程 `download::import_argosmodel`，完成后刷新列表。
   - 下载/导入中的按钮 disabled；失败红色错误文案 + 重试即重新点按钮。
7. OCR 联动（main.rs OCR 结果面板）：在结果区工具行（现有「全部复制」按钮旁）加
   btn-sm「翻译」：读取当前全部结果行 join("\n") → 写 translate_prefill{text, run:true} →
   `tab.set(TAB_TRANSLATE)`。翻译页 render 时读 prefill：text 非空则填入源文本框并清空 prefill；
   run=true 时自动触发一次翻译按钮逻辑。
8. settings.translate.auto_ocr=true 时：OCR 完成回调（ocr_flow 现有完成路径）自动执行第 7 步
   （不改 OCR 渲染逻辑，只在完成通知处加钩子；若完成路径在 main.rs 通知 toast 处，就近插入）。

## 4. settings.rs 扩展

Settings struct 追加（serde 兼容旧 config.toml——全部 #[serde(default)]）：

```rust
#[serde(default)]
pub translate: TranslateSettings,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TranslateSettings {
    pub engine: String,        // offline|google|microsoft|deepl|deeplx|openai（默认 "offline"）
    pub compute: String,       // int8|float32（默认 "int8"）
    pub polish: bool,          // true
    pub auto_ocr: bool,        // false
    pub deepl_api_key: String, // ""
    pub deepl_pro: bool,       // false
    pub deeplx_url: String,    // "http://127.0.0.1:1188/translate"
    pub openai_base_url: String, // ""
    pub openai_api_key: String,  // ""
    pub openai_model: String,    // "gpt-4o-mini"
    pub last_from: String,      // "en"
    pub last_to: String,        // "zh"
    // 镜像（空串 = 内置默认顺序，见 download::Mirrors::default）
    pub mirror_argos: String,   // ""（默认 https://argos-net.com/v1）
    pub mirror_hf: String,      // ""（默认 huggingface.co → hf-mirror.com；非空=只用该 host）
    pub mirror_github: String,  // ""（默认直连+ghfast.top/gh-proxy.com/ghproxy.net；非空=只用该前缀）
}
```
Default impl 按注释默认值。`#[serde(skip_serializing_if)]` 不加（保持 toml 可读全量）。

**镜像 → Mirrors 构造**（translate_ui 与下载调用处共用，写成纯函数 + 单测）：
```rust
pub(crate) fn mirrors_from_settings(s: &TranslateSettings) -> rumi_translate::download::Mirrors {
    let mut m = Mirrors::default();
    if !s.mirror_argos.is_empty() { m.argos_base = s.mirror_argos.trim_end_matches('/').into(); }
    if !s.mirror_hf.is_empty() { m.hf_hosts = vec![s.mirror_hf.clone()]; }
    if !s.mirror_github.is_empty() { m.github_prefixes = vec![s.mirror_github.trim_end_matches('/').into()]; }
    m
}
```

设置页新增「翻译」小节（settings.rs 渲染 + main.rs 分发不变）：
- 引擎 select（六项，同翻译页禁用规则但可保存任意项——离线除外恒可选）；
- 计算精度 select：int8（快，默认）/ float32（稳）；
- checkbox：中文标点润色、OCR 后自动翻译；
- DeepL：key 输入框（password 样式）+ 免费版/专业版 checkbox；
- DeepLX：URL 输入框；
- OpenAI 兼容：base_url / api_key / model 三个输入框；
- 镜像（折叠小节或直接三行）：Argos 源 base / HF 镜像 host / GitHub 镜像前缀，
  placeholder 提示「留空使用内置默认（argos-net.com / huggingface.co→hf-mirror.com /
  直连→ghfast.top→gh-proxy.com→ghproxy.net）」；
- 控件风格照抄现有设置页（label+input 布局），保存时机照抄（变更即存或失焦存，跟随现有）。

## 5. i18n 词条（两个 json 同步加；key 必须逐字一致）

zh-CN.json 追加（en-US.json 同 key 英文）：

| key | zh | en |
| --- | --- | --- |
| translate.nav | 翻译 | Translate |
| translate.title | 翻译 | Translate |
| translate.run | 翻译 | Translate |
| translate.running | 翻译中… | Translating… |
| translate.from | 源语言 | From |
| translate.to | 目标语言 | To |
| translate.swap | 交换 | Swap |
| translate.copy | 复制 | Copy |
| translate.source_ph | 粘贴或输入要翻译的文本… | Paste or type text to translate… |
| translate.result_ph | 译文将显示在这里 | Translation appears here |
| translate.engine | 引擎 | Engine |
| translate.engine.offline | 离线（本地模型） | Offline (local model) |
| translate.engine.google | Google | Google |
| translate.engine.microsoft | Microsoft | Microsoft |
| translate.engine.deepl | DeepL | DeepL |
| translate.engine.deeplx | DeepLX | DeepLX |
| translate.engine.openai | OpenAI 兼容 | OpenAI compatible |
| translate.need_config | （需配置） | (needs config) |
| translate.go_settings | 去设置 | Settings |
| translate.models | 离线语言包 | Offline language packs |
| translate.models.installed | 已安装 ✓ | Installed ✓ |
| translate.models.download | 下载 | Download |
| translate.models.import | 导入语言包… | Import pack… |
| translate.models.downloading | {p1}/{p2} MB | {p1}/{p2} MB |
| translate.models.failed | 失败：{p} | Failed: {p} |
| translate.ocr_button | 翻译 | Translate |
| settings.translate | 翻译 | Translation |
| settings.translate.engine | 默认引擎 | Default engine |
| settings.translate.compute | 计算精度 | Compute type |
| settings.translate.compute_int8 | int8（快，推荐） | int8 (fast, recommended) |
| settings.translate.compute_float32 | float32（对照） | float32 (baseline) |
| settings.translate.polish | 中文标点润色 | Polish Chinese punctuation |
| settings.translate.auto_ocr | 识别后自动翻译 | Auto-translate after OCR |
| settings.translate.deepl_key | DeepL API Key | DeepL API key |
| settings.translate.deepl_pro | DeepL 专业版 | DeepL Pro plan |
| settings.translate.deeplx_url | DeepLX 服务地址 | DeepLX server URL |
| settings.translate.openai_base | OpenAI 兼容 Base URL | OpenAI-compatible base URL |
| settings.translate.openai_key | API Key | API key |
| settings.translate.openai_model | 模型 | Model |
| settings.translate.mirror | 下载镜像 | Download mirrors |
| settings.translate.mirror_argos | Argos 源地址 | Argos source base |
| settings.translate.mirror_hf | HF 镜像 | HF mirror host |
| settings.translate.mirror_github | GitHub 镜像 | GitHub mirror prefix |
| settings.translate.mirror_ph | 留空使用内置默认 | Leave empty for built-in defaults |

（i18n.rs 的 t()/tt() 用法照抄现有调用；带 {p} 占位的词条若现有机制不支持插值，
直接字符串拼接后再进 t() 的静态 key 体系——按现有代码实际能力落地，回报注明。）

## 6. ui/Cargo.toml

追加 `rumi-translate = { path = "../translate" }`（注释：翻译核心（ct2rs 编译在此 crate））。

## 7. 测试与验收

- `cargo check -p rumi-ui` 通过；`cargo test -p rumi-ui` 基线 60+1 全绿（新增逻辑尽量纯函数化可测）。
- translate_ui.rs 里至少 3 个单测：引擎下拉禁用判定纯函数（输入 TranslateSettings → 输出各引擎
  enabled bool）、prefill 消费逻辑（读后清空）、Engine 构造映射（TranslateSettings→rumi_translate::Engine）。
  （组件渲染不测。）
- 手动冒烟（回报贴结果）：cargo run -p rumi-ui → 翻译页：en→zh 离线翻译一句话（模型未装时
  走下载流程到完成）；OCR 截图识别 → 点「翻译」跳转预填。

## 8. 回报格式

改动文件清单；i18n 插值机制的实际处理方式；单测结果；冒烟结果（截图描述即可）；歧义点清单。
