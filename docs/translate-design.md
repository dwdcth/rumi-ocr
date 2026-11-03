# rumi 翻译功能设计（2026-11-03）

> 离线翻译（Argos/CTranslate2 路线）+ 在线翻译（read-frog 路线）集成设计。
> 推理用 [jkawamoto/ctranslate2-rs](https://github.com/jkawamoto/ctranslate2-rs)（ct2rs）。
> 实现规格见 `docs/translate-spec-{core,engine,online,ui,server}.md`，规格与本文件冲突时以规格为准（规格更细）。

## 一、选型结论（含依据）

### 推理后端：ct2rs + oneDNN（dnnl 特性），不走 OpenBLAS

- ct2rs 0.9.18 的 `dnnl` 特性经 `onednn-src` crate（0.1.22）从源码构建 oneDNN，
  其 vendored oneDNN 子模块 sha `a190d4ab` **与官方 v3.14.1 tag 完全一致**（2026-10-29 发布）。
- oneDNN **v3.14 是首个加入 AMD Zen 专属优化的主线版本**（Zen 4/5 AVX-512 matmul/inner product，
  Intel 侧持续支持）——即"最新的 oneDNN 支持 Intel 和 AMD"成立，ct2rs 可用之。
- 另一硬理由：CTranslate2 的 **int8 推理在 x86-64 上必须 MKL 或 oneDNN 后端**；
  用 OpenBLAS/ruy 则只有 float32 GEMM。故选 `dnnl`，OpenBLAS 不再需要
  （本机也无 gfortran，静态编 OpenBLAS 反而要装依赖）。
- 选型调研记录（2026-11-03）：本机 cmake 4.4.4 / gcc 满足构建；ct2rs build.rs 对
  cmake 4.x 已设 `CMAKE_POLICY_VERSION_MINIMUM=3.5`（dnnl+openmp-runtime-comp 组合命中）。

### 模型：Argos 官方语言包（主源）+ HF opus-mt（备源）+ 镜像可配

- **主源 = argos-net.com 官方 .argosmodel 直链**（2026-11-03 复核 HTTP 200，本机直连可用）：
  - `https://argos-net.com/v1/translate-zh_en-1_9.argosmodel`（74,481,402B）
  - `https://argos-net.com/v1/translate-en_zh-1_9.argosmodel`（70,743,021B）
  - 即微信文章实测的同款 Argos 1.9 语言包；解包实测（en_zh）：顶层 `translate-en_zh-1_9/`，
    内含 `model/`（**int8 量化的** model.bin 82.7MB + shared_vocabulary.json + CT2 config.json，
    注意 config.json 有 add_source_bos/bos_token 等加载配置，必须保留在模型目录）、
    `sentencepiece.model`（单文件双向，安装时复制为 source.spm/target.spm 两份）、
    `metadata.json`（from_code/to_code）、`stanza/`（忽略，我们用自研分句）。
- **备源 = HF `ooeoeo/opus-mt-{zh-en,en-zh}-ct2-float16`**（多文件，fp16，~157MB/方向，
  CPU 加载自动转 float32/int8）。同方向 argos 直链失败时逐源回退。
- Argos 官方旧 CDN `translatepackages.argosopentech.com` 本机 DNS 污染不可达，不内置。
- **镜像设置**（用户可配，内置国内默认）：
  - HF：默认顺序 `huggingface.co → hf-mirror.com`；自定义 host 覆盖。
  - GitHub：默认直连 + `ghfast.top / gh-proxy.com / ghproxy.net` 兜底（2026-11-03 实测三活一死，
    mirror.ghproxy.com 已死不内置）。
  - Argos：默认 `https://argos-net.com/v1`，可自定义 base。
- 下载器尊重系统代理（`https_proxy`/`all_proxy`），支持断点续传、`.part` 临时文件、
  manifest 校验、进度回调；Argos 包与 HF 文件安装后统一目录布局（见 spec-core §6）。

### 分句与润色：自研，不引入 Stanza

文章实测的最大离线坑是 Argos 默认分句器 Stanza 初始化要联网。rumi 自研规则分句器
（CJK+拉丁标点、长句二次切分），翻译后对 zh 译文做保守标点润色（半角→全角，
数字上下文豁免），全部纯函数可单测。

### 在线翻译：对齐 read-frog 的纯翻译 provider 集

read-frog 的翻译 provider 分两类：纯翻译（google-translate / microsoft-translate /
deepl / deeplx）与 LLM（openai-compatible 系）。rumi v1 实现前者全集 + openai 兼容一种：

| engine | 端点 | 鉴权 |
| --- | --- | --- |
| google | `translate.googleapis.com/translate_a/single` (client=gtx) | 免费无 key |
| microsoft | `api-edge.cognitive.microsofttranslator.com`（Edge 免费端点 + auth token 缓存） | 免费无 key |
| deepl | `api-free.deepl.com` / `api.deepl.com` `/v2/translate` | auth key |
| deeplx | 用户自建 DeepLX 服务 URL | 无/自定 |
| openai | 任意 OpenAI 兼容 `/chat/completions`（含本地服务） | base_url+key+model |

默认引擎 = 离线（符合 rumi 离线优先的定位；google/microsoft 端点国内可达性不保证，
文档注明）。

## 二、架构

```
┌─ rumi-ui (Dioxus) ──────────┐   ┌─ rumi-server (axum) ─┐   ┌─ rumi-translate-cli ─┐
│ 翻译页(TAB_TRANSLATE)       │   │ POST /api/translate  │   │ 文本/文件/stdin       │
│ OCR 结果→翻译 一键联动      │   │ GET  models 状态     │   │ models 子命令        │
└──────────────┬──────────────┘   └──────────┬──────────┘   └──────────┬───────────┘
               │          都只依赖 rumi-translate（ct2rs 只在此 crate） │
               ▼                              ▼                          ▼
┌─ translate (rumi-translate) ────────────────────────────────────────────────────┐
│ lib: translate() 统一入口（Engine 枚举分发）                                    │
│ lang.rs 语言码 │ segment.rs 分句 │ postfix.rs 译文润色                          │
│ registry.rs 模型注册表 │ download.rs 下载器(HF/镜像/Argos导入, 断点续传)         │
│ engine.rs 离线引擎（ct2rs, 懒加载+LRU 卸载, Mutex<HashMap<方向, Translator>>)    │
│ online/ 在线 providers（google/microsoft/deepl/deeplx/openai）                  │
│ 模型目录: ~/.local/share/rumi/translate/models/<key>/                           │
└─────────────────────────────────────────────────────────────────────────────────┘
```

- 新 workspace crate `translate/`（lib `rumi-translate` + bin `rumi-translate-cli`），
  ct2rs 重编译只发生在这一个 crate；ui/server/cli 依赖它但不直接依赖 ct2rs。
- 线程模型：全同步 API。UI 沿用现有「std 线程 + oneshot 回 dioxus spawn」模式；
  server 用 `spawn_blocking`。CT2 推理单段 0.1~0.2s 量级（文章实测数据），同步足够。
- 内存：opus-mt int8 约 100~200MB/方向，懒加载，闲置 10 分钟卸载。

## 三、事实约束（工人须知）

1. 工作区 `Cargo.toml` 的 members 增加 `"translate"`（core 工人负责，其他工人不动）。
2. ct2rs 首次编译 10~20 分钟（C++/oneDNN），`cargo check -p rumi-translate` 先跑通一次后
   缓存共享；工人不要反复 clean。
3. ct2rs `Translator::new(path, &Config)` 自动探测模型目录 tokenizer
   （source.spm/target.spm 或 tokenizer.json）；`translate_batch(&Vec<String>, &TranslationOptions, None)`
   返回 `Vec<(String, Vec<Score>)>`。
4. 测试基线不得回归：`cargo test -p rumi-ui -p rumi-ocr`（60+1 / 30+2）+ demo 插件 4 个。
5. UI 词条：zh-CN.json 基准表，en-US.json 可缺词回退；新词条按 `translate.*` 前缀。
6. i18n/Dioxus 已知坑见 `docs/开发经验.md` §二（Signal !Send、rsx 内禁 let、select 无
   value 属性等）。

## 四、验收

1. `cargo build --workspace` 通过；`cargo test -p rumi-translate` 全绿（不打真网的单测）。
2. E2E（`RUMI_E2E=1 cargo test -p rumi-translate --test e2e -- --ignored`）：
   真实下载 en_zh 模型 → 翻译 `The default port is 8080.` → 译文含 `8080`。
3. 在线 E2E（google，视网络可达性 `#[ignore]`）：翻译固定句子断言非空。
4. UI：翻译页输入→翻译→复制；OCR 识别结果一键带入翻译页；模型下载有进度条。
5. server：`curl POST /api/translate` 返回译文。
6. 基线测试全部不回归。
