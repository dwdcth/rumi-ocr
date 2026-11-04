//! translate_ui —— 翻译页（工人 D：docs/translate-spec-ui.md §3）。
//!
//! 行为要点（规格逐条落地）：
//! - 源/目标语言、引擎选择即时持久化（settings.translate.last_from/last_to/engine，
//!   变更即写盘，模式照抄主题/关闭行为等即时保存项）；
//! - 翻译重活放 std 线程（[`crate::ocr_flow::run_blocking`] oneshot 回 dioxus
//!   spawn，模式照抄 ocr_flow；Signal 非 Send，绝不在工作线程碰信号）；
//! - 离线语言包：`registry::MODELS` 逐行展示；已安装清单经
//!   `engine::shared().installed()` 刷新（fs 扫描放线程，线程安全读）；
//! - 模型管理（规格 F/G §6）：已安装行带「删除」——先 `unload_key` 摘进程内
//!   缓存再 `download::remove` 删目录；「更多语言包…」展开官方目录子区——
//!   首次展开 `fetch_catalog` 拉取，逐行 `install_catalog_entry` 下载（进度
//!   复用 prog/busy_key 信号），目录行数据组装抽 [`catalog_rows`] 纯函数直测；
//! - 下载进度：进度回调在下载线程上产生 → futures mpsc 无界通道（非阻塞 send）
//!   → UI 端 `while let Some(..) = rx.next().await` 泵成信号更新——不用定时器
//!   轮询、也不在回调里碰 Signal；全部 sender 关闭（下载线程结束）后流自然收尾；
//! - rfd 文件对话框一律 std 线程调（Linux GTK 后端同步对话框死锁主线程，
//!   开发经验 §三），选文件与导入包在同一个后台线程里完成；
//! - 文档翻译（规格 J/K §5）：「翻译文档…」按钮 → std 线程 rfd 选文件 + 保存
//!   对话框（默认名 <原名>.bilingual.md，同线程）→ 同线程调
//!   `rumi_translate::docmt::translate_document`（Inline 策略、min_chars=2）；
//!   单元进度经 mpsc 无界通道泵成 doc_prog 信号（文案「已翻译 n/N 单元」）；
//!   与文本「翻译」共用 running 信号互斥；默认输出名推导与过滤器表抽纯函数直测；
//! - OCR 联动：识别页「翻译」按钮写 [`TranslatePrefill`]`{text, run:true}` 并切
//!   tab，本页 use_effect 消费（读后清空），run=true 自动触发一次翻译。
//!
//! 词条 key 常量收在本模块 [`tr`]（i18n.rs 不在本工人文件白名单内；key 原文按
//! 规格 §5 逐字，勿改）。带 {p}/{p1}/{p2} 占位的词条用字符串 replace 插值
//! （现有 i18n::fill 只认 {n}/{v}，filln 只认 {0}{1}…，见回报）。

use crate::i18n;
use crate::settings::{self, TranslateSettings};
use dioxus::prelude::*;
use futures::StreamExt;
use rumi_translate::{lang, registry, Engine};

/// 翻译页共享状态：OCR 结果跳转预填用（translate-spec-ui.md §2）。
/// 识别页按钮写入、翻译页 use_effect 消费；信号由 App 持有并分别传给两侧，
/// 跨页读写同一实例（main.rs 现有 Signal 组织方式 = App 声明 + props 传递）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TranslatePrefill {
    pub text: String,
    pub run: bool,
}

/// 词条 key（translate-spec-ui.md §5 逐字；settings.translate.* 是规格指定的
/// 前缀，与现有设置页 set.* 不同源，勿"统一"成 set.*）。
pub(crate) mod tr {
    // 翻译页
    pub const NAV: &str = "translate.nav";
    pub const TITLE: &str = "translate.title";
    pub const RUN: &str = "translate.run";
    pub const RUNNING: &str = "translate.running";
    pub const FROM: &str = "translate.from";
    pub const TO: &str = "translate.to";
    pub const SWAP: &str = "translate.swap";
    /// 语言下拉「更多语言」分组的 optgroup 标题。
    pub const MORE_LANGS: &str = "translate.more_langs";
    pub const COPY: &str = "translate.copy";
    pub const SOURCE_PH: &str = "translate.source_ph";
    pub const RESULT_PH: &str = "translate.result_ph";
    pub const ENGINE: &str = "translate.engine";
    pub const ENGINE_OFFLINE: &str = "translate.engine.offline";
    pub const ENGINE_GOOGLE: &str = "translate.engine.google";
    pub const ENGINE_MICROSOFT: &str = "translate.engine.microsoft";
    pub const ENGINE_DEEPL: &str = "translate.engine.deepl";
    pub const ENGINE_DEEPLX: &str = "translate.engine.deeplx";
    pub const ENGINE_OPENAI: &str = "translate.engine.openai";
    pub const NEED_CONFIG: &str = "translate.need_config";
    pub const GO_SETTINGS: &str = "translate.go_settings";
    pub const MODELS: &str = "translate.models";
    pub const MODELS_INSTALLED: &str = "translate.models.installed";
    pub const MODELS_DOWNLOAD: &str = "translate.models.download";
    pub const MODELS_IMPORT: &str = "translate.models.import";
    pub const MODELS_DOWNLOADING: &str = "translate.models.downloading";
    pub const MODELS_FAILED: &str = "translate.models.failed";
    // 模型删除 + 目录子区（规格 F/G §7 逐字）
    pub const MODELS_DELETE: &str = "translate.models.delete";
    pub const MODELS_DELETING: &str = "translate.models.deleting";
    pub const MODELS_DELETE_FAILED: &str = "translate.models.delete_failed";
    pub const MODELS_NLLB_LABEL: &str = "translate.models.nllb_label";
    pub const MODELS_MORE: &str = "translate.models.more";
    pub const MODELS_FETCHING: &str = "translate.models.fetching";
    pub const MODELS_FETCH_FAILED: &str = "translate.models.fetch_failed";
    pub const MODELS_RETRY: &str = "translate.models.retry";
    pub const MODELS_VERSION: &str = "translate.models.version";
    pub const OCR_BUTTON: &str = "translate.ocr_button";
    // 文档翻译（规格 J/K §6 逐字）
    pub const DOC_BUTTON: &str = "translate.doc_button";
    pub const DOC_RUNNING: &str = "translate.doc_running";
    pub const DOC_PROGRESS: &str = "translate.doc_progress";
    pub const DOC_DONE: &str = "translate.doc_done";
    /// 布局退化说明（DocStats.note 的展示模板）。
    pub const DOC_NOTE: &str = "translate.doc_note";
    pub const DOC_FAILED: &str = "translate.doc_failed";
    pub const DOC_FILTER: &str = "translate.doc_filter";
    // 设置页翻译小节（规格 §5 指定 settings.translate.* 前缀）
    pub const SET_SECTION: &str = "settings.translate";
    pub const SET_ENGINE: &str = "settings.translate.engine";
    pub const SET_COMPUTE: &str = "settings.translate.compute";
    pub const SET_COMPUTE_INT8: &str = "settings.translate.compute_int8";
    pub const SET_COMPUTE_FLOAT32: &str = "settings.translate.compute_float32";
    pub const SET_LAYOUT: &str = "settings.translate.layout";
    pub const SET_LAYOUT_INLINE: &str = "settings.translate.layout_inline";
    pub const SET_LAYOUT_PAGE: &str = "settings.translate.layout_page";
    pub const SET_LAYOUT_ONLY: &str = "settings.translate.layout_only";
    pub const SET_POLISH: &str = "settings.translate.polish";
    pub const SET_AUTO_OCR: &str = "settings.translate.auto_ocr";
    pub const SET_DEEPL_KEY: &str = "settings.translate.deepl_key";
    pub const SET_DEEPL_PRO: &str = "settings.translate.deepl_pro";
    pub const SET_DEEPLX_URL: &str = "settings.translate.deeplx_url";
    pub const SET_OPENAI_BASE: &str = "settings.translate.openai_base";
    pub const SET_OPENAI_KEY: &str = "settings.translate.openai_key";
    pub const SET_OPENAI_MODEL: &str = "settings.translate.openai_model";
    pub const SET_MIRROR: &str = "settings.translate.mirror";
    pub const SET_MIRROR_ARGOS: &str = "settings.translate.mirror_argos";
    pub const SET_MIRROR_HF: &str = "settings.translate.mirror_hf";
    pub const SET_MIRROR_GITHUB: &str = "settings.translate.mirror_github";
    pub const SET_MIRROR_PH: &str = "settings.translate.mirror_ph";
    pub const SET_GLOSSARY: &str = "settings.translate.glossary";
    pub const SET_GLOSSARY_PH: &str = "settings.translate.glossary_ph";
    // 规格外补充词条（沿用现有 *thread_lost 文案风格；回报已注明）
    pub const THREAD_LOST: &str = "translate.thread_lost";
    pub const MODELS_THREAD_LOST: &str = "translate.models.thread_lost";
}

// ── 纯函数层（可测；规格 §7 要求的三个单测全部打在这层）─────────────────────

/// 引擎可用性判定（规格 §3.3）：离线/Google/Microsoft 恒可选；
/// DeepL/DeepLX/OpenAI 分别要求 key / URL / base_url 非空；未知引擎不可选。
pub(crate) fn engine_enabled(st: &TranslateSettings, key: &str) -> bool {
    match key {
        "offline" | "google" | "microsoft" => true,
        "deepl" => !st.deepl_api_key.is_empty(),
        "deeplx" => !st.deeplx_url.is_empty(),
        "openai" => !st.openai_base_url.is_empty(),
        _ => false,
    }
}

/// 引擎下拉项文案：缺配置的引擎追加「（需配置）」后缀（规格 §3.3 的
/// 「DeepL（需配置）」样式；中英各自取词后直接拼接）。
pub(crate) fn engine_label(key: &str, st: &TranslateSettings, lang: &str) -> String {
    let base = match key {
        "google" => i18n::ts(lang, tr::ENGINE_GOOGLE),
        "microsoft" => i18n::ts(lang, tr::ENGINE_MICROSOFT),
        "deepl" => i18n::ts(lang, tr::ENGINE_DEEPL),
        "deeplx" => i18n::ts(lang, tr::ENGINE_DEEPLX),
        "openai" => i18n::ts(lang, tr::ENGINE_OPENAI),
        _ => i18n::ts(lang, tr::ENGINE_OFFLINE),
    };
    if engine_enabled(st, key) {
        base.to_string()
    } else {
        format!("{base}{}", i18n::ts(lang, tr::NEED_CONFIG))
    }
}

/// 设置 → rumi_translate::Engine 构造映射（规格 §3.4）；未知引擎串保守回退离线。
pub(crate) fn engine_from_settings(st: &TranslateSettings) -> Engine {
    match st.engine.as_str() {
        "google" => Engine::Google,
        "microsoft" => Engine::Microsoft,
        "deepl" => Engine::DeepL { api_key: st.deepl_api_key.clone(), pro: st.deepl_pro },
        "deeplx" => Engine::DeepLX { url: st.deeplx_url.clone() },
        "openai" => Engine::OpenAI {
            base_url: st.openai_base_url.clone(),
            api_key: st.openai_api_key.clone(),
            model: st.openai_model.clone(),
            // 术语表注入（glossary 规格 §4：设置页文本域 → prompt 段）
            glossary: rumi_translate::glossary::Glossary::parse(&st.glossary)
                .terms
                .into_iter()
                .map(|t| (t.src, t.dst))
                .collect(),
        },
        // offline 及任何未知串：离线恒可用，是最安全的回退
        _ => Engine::Offline,
    }
}

/// prefill 消费（规格 §3.7"读后清空"，纯函数可测）：text 非空 → 返回
/// (文本, run) 并清空结构体；text 空 → None（run 残留也不触发）。
pub(crate) fn take_prefill(p: &mut TranslatePrefill) -> Option<(String, bool)> {
    if p.text.is_empty() {
        return None;
    }
    let out = Some((std::mem::take(&mut p.text), p.run));
    p.run = false;
    out
}

/// 设置的布局值 → docmt 策略（纯函数可测）：inline 默认/未知回退段落对照。
fn doc_strategy_from_layout(layout: &str) -> rumi_translate::docmt::BiStrategy {
    match layout {
        "page" => rumi_translate::docmt::BiStrategy::PageAlt,
        "only" => rumi_translate::docmt::BiStrategy::Only,
        _ => rumi_translate::docmt::BiStrategy::Inline,
    }
}

/// 文档翻译默认输出路径（规格 K §5/§7）：与输入同目录、同名去扩展名 +
/// `.bilingual.md`（与 CLI --doc-out 默认值同口径）。多点扩展名按 file_stem
/// 口径去掉最后一段；路径无文件名（如以分隔符结尾）时保守拼接、不 panic。
fn default_doc_out(input: &std::path::Path) -> std::path::PathBuf {
    let stem = input
        .file_stem()
        .unwrap_or_else(|| input.file_name().unwrap_or_default());
    input.with_file_name(format!("{}.bilingual.md", stem.to_string_lossy()))
}

/// 文档翻译文件选择器扩展名表（规格 K §5 顺序逐字：docforge v1 可解析的
/// 9 种文本族格式）。纯函数直测「过滤器串构造」（规格 §7）。
fn doc_filter_extensions() -> &'static [&'static str] {
    &["pdf", "docx", "epub", "txt", "md", "html", "csv", "rtf", "srt"]
}

/// 设置里的语言码 → 下拉合法值：normalize 后不在候选里（空串/乱码）回退 def。
/// 候选 = 常用组 + 更多语言组（两组都是下拉真实渲染的 option）。
fn normalized_or(saved: &str, def: &str) -> String {
    let v = lang::normalize(saved);
    if lang::UI_LANGS.contains(&v.as_str()) || lang::UI_LANGS_MORE.contains(&v.as_str()) {
        v
    } else {
        def.to_string()
    }
}

/// 「更多语言包」目录子区状态机（规格 F/G §6.3）：未拉取 →（首次展开）拉取中
/// → 已加载 / 失败（可重试）。收起不丢状态，重开不重拉。不派生 PartialEq——
/// 判定一律用 matches!，避免耦合 CatalogEntry 的 derive 面合并时对不齐。
#[derive(Debug, Clone)]
enum CatalogState {
    NotFetched,
    Fetching,
    Loaded(Vec<rumi_translate::download::CatalogEntry>),
    Failed(String),
}

// ── 后台动作（std 线程 + oneshot 回 spawn；Signal 只在 UI 线程读写）─────────

/// 已安装清单刷新（规格 §3.6）：`engine::shared().installed()` 扫 models_dir 读
/// manifest，线程安全；fs 扫描放 std 线程，清单回信号。
fn refresh_installed(mut installed: Signal<Vec<rumi_translate::engine::InstalledInfo>>) {
    spawn(async move {
        if let Ok(list) =
            crate::ocr_flow::run_blocking(|| rumi_translate::engine::shared().installed()).await
        {
            installed.set(list);
        }
    });
}

/// 发起一次翻译（规格 §3.4）：std 线程跑 rumi_translate::translate（单条文本，
/// 离线引擎内部分句）；
/// - 运行中 running=true（按钮 disabled + 文案「翻译中…」）；
/// - 错误 → 结果区红字（var(--danger)）；文案含「语言包未安装」→ 高亮对应
///   方向的语言包行（该行即缺失项）；
/// - 完成后 elapsed = 纯翻译段耗时（`.elapsed()`，规格 §3.5）。
#[allow(clippy::too_many_arguments)]
fn start_translate(
    from_sel: Signal<String>,
    to_sel: Signal<String>,
    src: Signal<String>,
    mut result: Signal<String>,
    mut result_err: Signal<Option<String>>,
    mut highlight: Signal<Option<String>>,
    mut elapsed: Signal<String>,
    mut running: Signal<bool>,
) {
    let text = src();
    if text.trim().is_empty() {
        return; // 空文本不跑（按钮禁用兜底）
    }
    let st = settings::get().translate;
    let from = from_sel();
    let to = to_sel();
    // 错误路径判「语言包未安装」高亮方向用（from/to 即将 move 进线程闭包）
    let (from_err, to_err) = (from.clone(), to.clone());
    let engine = engine_from_settings(&st);
    let rx = crate::ocr_flow::run_blocking(move || {
        let t0 = std::time::Instant::now();
        let res = rumi_translate::translate(&[text], &from, &to, &engine, st.polish);
        (res, t0.elapsed().as_secs_f64())
    });
    running.set(true);
    result_err.set(None);
    highlight.set(None);
    elapsed.set(String::new());
    spawn(async move {
        match rx.await {
            Ok((Ok(mut outs), secs)) => {
                result.set(outs.pop().unwrap_or_default());
                elapsed.set(format!("{secs:.1} s"));
            }
            Ok((Err(e), _)) => {
                // translate crate 的 EngineError::ModelNotInstalled 文案含
                // 「语言包未安装」；命中则按当前方向高亮语言包行（registry key
                // 形如 "en_zh" = "{from}_{to}"）
                if e.contains("语言包未安装") {
                    highlight.set(Some(format!(
                        "{}_{}",
                        lang::normalize(&from_err),
                        lang::normalize(&to_err)
                    )));
                }
                result_err.set(Some(e));
            }
            Err(_) => result_err.set(Some(i18n::tt(tr::THREAD_LOST).to_string())),
        }
        running.set(false);
    });
}

/// start_doc_translate 后台线程的结算（规格 K §5）：Cancelled = 用户在选文件/
/// 保存对话框任一取消（静默复位）；Done = 成功（统计 + 输出全路径，状态行只
/// 展示文件名）；Failed = translate_document 报错原文（红字展示）。
enum DocOutcome {
    Cancelled,
    Done(rumi_translate::docmt::DocStats, std::path::PathBuf),
    Failed(String),
}

/// 翻译整个文档（规格 K §5）：rfd 选文件与保存对话框必须在 std 线程（Linux
/// GTK 同步对话框死锁主线程，开发经验 §三；模式照抄 start_import），两对话框
/// 与 translate_document 重活合并进同一后台线程（选中→保存→翻译一气呵成）；
/// - 互斥：与「翻译」按钮共用 running 信号（同一时刻只允许一个翻译动作）；
/// - 选项：strategy=Inline、min_chars=2 为规格 §5 固定值；引擎/polish 取点击
///   时设置快照，from/to 取当前下拉值；
/// - 进度：on_progress 回调在翻译线程上产生 → futures mpsc 无界通道（非阻塞
///   send；Signal !Send 绝不在工作线程碰）→ UI 端泵成 doc_prog 信号（文案
///   「已翻译 n/N 单元」；doc 用单元口径，不与模型下载 prog 的字节口径混用）；
/// - 结算：完成状态行「单元 N · 耗时 X s → 文件名」；失败/线程丢失红字；
///   完成态保留到下次发起（发起时统一清空重置）。
fn start_doc_translate(
    from_sel: Signal<String>,
    to_sel: Signal<String>,
    mut running: Signal<bool>,
    mut doc_running: Signal<bool>,
    mut doc_prog: Signal<Option<(usize, usize)>>,
    mut doc_status: Signal<String>,
    mut doc_err: Signal<Option<String>>,
) {
    if running() {
        return; // 与文本翻译互斥（按钮 disabled 兜底）
    }
    let st = settings::get().translate;
    let from = from_sel();
    let to = to_sel();
    let engine = engine_from_settings(&st);
    let (ptx, mut prx) = futures::channel::mpsc::unbounded::<(usize, usize)>();
    let rx = crate::ocr_flow::run_blocking(move || {
        // 对话框 1：选输入文件（过滤器名 i18n + 规格 §5 扩展名表）
        let Some(input) = rfd::FileDialog::new()
            .add_filter(i18n::tt(tr::DOC_FILTER), doc_filter_extensions())
            .pick_file()
        else {
            return DocOutcome::Cancelled; // 用户取消
        };
        // 对话框 2：保存路径（默认名 <原名>.bilingual.md、存输入同目录，同线程）
        let out_name = default_doc_out(&input)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "translation.bilingual.md".to_string());
        let mut save = rfd::FileDialog::new().set_file_name(&out_name)
            // 输出格式按扩展名分派（docmt::translate_document）：
            // .md=双语 Markdown；.docx/.epub=保原样式回写（输入需同格式）
            .add_filter("Markdown (*.md)", &["md"])
            .add_filter("Word (*.docx)", &["docx"])
            .add_filter("EPUB (*.epub)", &["epub"]);
        if let Some(dir) = input.parent() {
            save = save.set_directory(dir);
        }
        let Some(out) = save.save_file() else {
            return DocOutcome::Cancelled; // 用户取消
        };
        // 进度回调在翻译线程上被调：unbounded_send 非阻塞、UnboundedSender
        // 满足 Send+Sync，克隆一份进回调；原句柄留在闭包内保证流生命周期
        let sink = ptx.clone();
        let opts = rumi_translate::docmt::DocTranslateOptions {
            engine,
            from,
            to,
            // 双语布局取设置（settings.translate.doc_layout）；未知值回退段落对照
            strategy: doc_strategy_from_layout(&st.doc_layout),
            polish: st.polish,
            min_chars: 2,
            use_cache: true,
            // 术语表：设置页文本域解析（精确命中固定译法 + OpenAI prompt 注入）
            glossary: rumi_translate::glossary::Glossary::parse(&st.glossary),
            on_progress: Some(std::sync::Arc::new(move |done, total| {
                let _ = sink.unbounded_send((done, total));
            })),
        };
        let res = rumi_translate::docmt::translate_document(&input, &out, &opts);
        drop(ptx); // 关闭进度流：UI 端泵收尾退出
        match res {
            Ok(stats) => DocOutcome::Done(stats, out),
            Err(e) => DocOutcome::Failed(e),
        }
    });
    running.set(true);
    doc_running.set(true);
    doc_prog.set(None);
    doc_status.set(String::new());
    doc_err.set(None);
    spawn(async move {
        // 泵进度：翻译线程回调 → 信号（首个回调前界面显示「文档翻译中…」）
        while let Some((done, total)) = prx.next().await {
            doc_prog.set(Some((done, total)));
        }
        match rx.await {
            Ok(DocOutcome::Done(stats, out)) => {
                // 状态行按规格 K §6 模板插值：单元 N · 耗时 X s → 文件名
                let name = out
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                doc_status.set(
                    i18n::tt(tr::DOC_DONE)
                        .replace("{p1}", &stats.units.to_string())
                        .replace("{p2}", &format!("{:.1}", stats.elapsed_ms as f64 / 1000.0))
                        .replace("{p3}", &name),
                );
                // 布局退化说明（如 page 遇 docx 等无页概念格式 → 原文+译文附录）
                if let Some(note) = stats.note {
                    doc_status.set(format!(
                        "{}\n{}",
                        doc_status(),
                        i18n::tt(tr::DOC_NOTE).replace("{p}", &note)
                    ));
                }
            }
            Ok(DocOutcome::Failed(e)) => {
                doc_err.set(Some(i18n::tt(tr::DOC_FAILED).replace("{p}", &e)));
            }
            Ok(DocOutcome::Cancelled) => {} // 用户取消：静默复位
            // 翻译线程意外退出（panic）：沿用现有 thread_lost 文案（规格外
            // 补充，与 start_translate 同款；回报已注明）
            Err(_) => doc_err.set(Some(i18n::tt(tr::THREAD_LOST).to_string())),
        }
        doc_prog.set(None);
        running.set(false);
        doc_running.set(false);
    });
}

/// 下载一个内置语言包（规格 §3.6）：std 线程跑 `download::install`（断点续传/
/// 多源回退在 translate crate 内）；进度经 futures mpsc 无界通道回 UI 泵；
/// 失败 → 行内红字「失败：{p}」，重试即重新点按钮。一次只允许一个下载/导入。
fn start_download(
    spec: &'static registry::ModelSpec,
    mut busy_key: Signal<Option<String>>,
    mut prog: Signal<Option<(String, u64, Option<u64>)>>,
    mut model_err: Signal<Option<(String, String, ModelErrKind)>>,
    installed: Signal<Vec<rumi_translate::engine::InstalledInfo>>,
    mut highlight: Signal<Option<String>>,
) {
    if busy_key().is_some() {
        return;
    }
    let key = spec.key;
    busy_key.set(Some(key.to_string()));
    prog.set(None);
    model_err.set(None);
    highlight.set(None);
    let st = settings::get().translate;
    let mirrors = settings::mirrors_from_settings(&st);
    let dir = rumi_translate::models_dir();
    let (ptx, mut prx) = futures::channel::mpsc::unbounded::<(u64, Option<u64>)>();
    let rx = crate::ocr_flow::run_blocking(move || {
        // 进度回调在下载线程上被调：unbounded_send 非阻塞、UnboundedSender
        // 满足 Send+Sync，克隆一份进闭包；原句柄留在闭包外保证流生命周期
        let sink = ptx.clone();
        let out = rumi_translate::download::install(spec, &dir, &mirrors, &move |p| {
            let _ = sink.unbounded_send((p.downloaded, p.total));
        });
        drop(ptx); // 关闭进度流：UI 端泵收尾退出
        out
    });
    spawn(async move {
        while let Some((d, total)) = prx.next().await {
            prog.set(Some((key.to_string(), d, total)));
        }
        match rx.await {
            Ok(Ok(_)) => {
                prog.set(None);
                busy_key.set(None);
                refresh_installed(installed);
            }
            Ok(Err(e)) => {
                prog.set(None);
                busy_key.set(None);
                model_err.set(Some((key.to_string(), e, ModelErrKind::Download)));
            }
            Err(_) => {
                // 下载线程意外退出（panic）：清进度、给行内红字
                prog.set(None);
                busy_key.set(None);
                model_err.set(Some((
                    key.to_string(),
                    i18n::tt(tr::MODELS_THREAD_LOST).to_string(),
                    ModelErrKind::Download,
                )));
            }
        }
    });
}

/// 导入本地 .argosmodel 包（规格 §3.6）：rfd 同步对话框在 Linux GTK 后端死锁
/// 主线程（开发经验 §三），选文件与解包导入一并在 std 线程完成；完成后刷新清单。
fn start_import(
    mut busy_key: Signal<Option<String>>,
    mut model_err: Signal<Option<(String, String, ModelErrKind)>>,
    installed: Signal<Vec<rumi_translate::engine::InstalledInfo>>,
) {
    if busy_key().is_some() {
        return;
    }
    busy_key.set(Some("import".to_string()));
    model_err.set(None);
    let dir = rumi_translate::models_dir();
    let rx = crate::ocr_flow::run_blocking(move || {
        let Some(pkg) = rfd::FileDialog::new()
            .add_filter("Argos 语言包", &["argosmodel"])
            .pick_file()
            .map(|p| p.display().to_string())
        else {
            return None; // 用户取消
        };
        Some(rumi_translate::download::import_argosmodel(
            std::path::Path::new(&pkg),
            &dir,
        ))
    });
    spawn(async move {
        match rx.await {
            Ok(Some(Ok(_))) => {
                busy_key.set(None);
                refresh_installed(installed);
            }
            Ok(Some(Err(e))) => {
                busy_key.set(None);
                model_err.set(Some(("import".to_string(), e, ModelErrKind::Import)));
            }
            Ok(None) => busy_key.set(None), // 用户取消
            Err(_) => {
                busy_key.set(None);
                model_err.set(Some((
                    "import".to_string(),
                    i18n::tt(tr::MODELS_THREAD_LOST).to_string(),
                    ModelErrKind::Import,
                )));
            }
        }
    });
}

/// 删除一个已安装语言包（规格 F/G §6.2）：std 线程先 `engine::shared().unload_key`
/// （摘进程内缓存；正在翻译中的调用持有 Arc 不受影响）再 `download::remove`
/// （删 dir/<key>/ 整目录，目录不存在幂等 Ok）——规格指定先摘缓存再删目录；
/// 完成后刷新清单。删除期间 busy_key/deleting 同时置位（与下载/导入互斥，
/// busy_key 命中的行禁删），行内显示「删除中…」；失败 → 行内红字
/// 「删除失败：{p}」（ModelErrKind::Delete 选模板）。
/// 该行实际安装的目录 key：registry key 本身在装 → 用之；否则若「已安装」来自
/// 方向匹配的导入包（目录名≠registry key，如手工导入的 .argosmodel）→ 用导入包
/// 的真实 key。都没有 → 返回 registry key（remove 幂等，删不存在的目录不报错）。
/// NLLB 行（from/to="*"）天然走第一分支。
fn installed_key_for(
    installed: &[rumi_translate::engine::InstalledInfo],
    spec: &registry::ModelSpec,
) -> String {
    if installed.iter().any(|i| i.key == spec.key) {
        return spec.key.to_string();
    }
    installed
        .iter()
        .find(|i| i.from == spec.from && i.to == spec.to)
        .map(|i| i.key.clone())
        .unwrap_or_else(|| spec.key.to_string())
}

fn start_delete(
    key: String,
    mut busy_key: Signal<Option<String>>,
    mut deleting: Signal<Option<String>>,
    mut model_err: Signal<Option<(String, String, ModelErrKind)>>,
    installed: Signal<Vec<rumi_translate::engine::InstalledInfo>>,
) {
    if busy_key().is_some() {
        return; // 一次只允许一个下载/导入/删除（下载中的行也因此禁删）
    }
    busy_key.set(Some(key.clone()));
    deleting.set(Some(key.clone()));
    model_err.set(None);
    let dir = rumi_translate::models_dir();
    // 后台闭包 move 走 key，错误回报用这个副本
    let key_err = key.clone();
    let rx = crate::ocr_flow::run_blocking(move || {
        // unload 返回 false（本就未加载）不影响删除；remove 幂等。
        // key 可能是导入包目录名（见 installed_key_for）
        let _ = rumi_translate::engine::shared().unload_key(&key);
        rumi_translate::download::remove(&key, &dir)
    });
    spawn(async move {
        match rx.await {
            Ok(Ok(_)) => {
                busy_key.set(None);
                deleting.set(None);
                refresh_installed(installed);
            }
            Ok(Err(e)) => {
                busy_key.set(None);
                deleting.set(None);
                model_err.set(Some((key_err.clone(), e, ModelErrKind::Delete)));
            }
            Err(_) => {
                busy_key.set(None);
                deleting.set(None);
                model_err.set(Some((
                    key_err,
                    i18n::tt(tr::MODELS_THREAD_LOST).to_string(),
                    ModelErrKind::Delete,
                )));
            }
        }
    });
}

/// 「更多语言包」目录拉取（规格 F/G §6.3）：std 线程 `fetch_catalog`（多镜像
/// 前缀回退在 translate crate 内）；结果整块回 CatalogState——Loaded 进列表、
/// Failed 在子区显示红字 + [重试]（重新走本函数）。
fn start_fetch_catalog(mut catalog: Signal<CatalogState>) {
    if matches!(catalog(), CatalogState::Fetching) {
        return; // 防重入（展开连点 / 重试连点）
    }
    catalog.set(CatalogState::Fetching);
    let st = settings::get().translate;
    let mirrors = settings::mirrors_from_settings(&st);
    let rx =
        crate::ocr_flow::run_blocking(move || rumi_translate::download::fetch_catalog(&mirrors));
    spawn(async move {
        catalog.set(match rx.await {
            Ok(Ok(list)) => CatalogState::Loaded(list),
            Ok(Err(e)) => CatalogState::Failed(e),
            // 拉取线程意外退出：与失败同路（可重试）
            Err(_) => CatalogState::Failed(i18n::tt(tr::MODELS_THREAD_LOST).to_string()),
        });
    });
}

/// 下载一个目录条目（规格 F/G §6.3）：std 线程 `install_catalog_entry`（单 zip
/// 直链 → 统一布局）；进度复用 prog 信号（busy_key 用 entry.key）、泵与收尾
/// 路径与 start_download 同款；失败 → 目录行红字「失败：{p}」。
fn start_catalog_download(
    entry: rumi_translate::download::CatalogEntry,
    mut busy_key: Signal<Option<String>>,
    mut prog: Signal<Option<(String, u64, Option<u64>)>>,
    mut model_err: Signal<Option<(String, String, ModelErrKind)>>,
    installed: Signal<Vec<rumi_translate::engine::InstalledInfo>>,
) {
    if busy_key().is_some() {
        return;
    }
    let key = entry.key.clone();
    busy_key.set(Some(key.clone()));
    prog.set(None);
    model_err.set(None);
    let st = settings::get().translate;
    let mirrors = settings::mirrors_from_settings(&st);
    let dir = rumi_translate::models_dir();
    let (ptx, mut prx) = futures::channel::mpsc::unbounded::<(u64, Option<u64>)>();
    let rx = crate::ocr_flow::run_blocking(move || {
        // 进度回调在下载线程上被调：unbounded_send 非阻塞、UnboundedSender
        // 满足 Send+Sync，克隆一份进闭包；原句柄留在闭包外保证流生命周期
        let sink = ptx.clone();
        let out = rumi_translate::download::install_catalog_entry(&entry, &dir, &mirrors, &move |p| {
            let _ = sink.unbounded_send((p.downloaded, p.total));
        });
        drop(ptx); // 关闭进度流：UI 端泵收尾退出
        out
    });
    spawn(async move {
        while let Some((d, total)) = prx.next().await {
            prog.set(Some((key.clone(), d, total)));
        }
        match rx.await {
            Ok(Ok(_)) => {
                prog.set(None);
                busy_key.set(None);
                refresh_installed(installed);
            }
            Ok(Err(e)) => {
                prog.set(None);
                busy_key.set(None);
                model_err.set(Some((key, e, ModelErrKind::Download)));
            }
            Err(_) => {
                // 下载线程意外退出（panic）：清进度、给行内红字
                prog.set(None);
                busy_key.set(None);
                model_err.set(Some((
                    key,
                    i18n::tt(tr::MODELS_THREAD_LOST).to_string(),
                    ModelErrKind::Download,
                )));
            }
        }
    });
}

// ── 翻译页组件 ─────────────────────────────────────────────────────────────

/// model_err 第三元：错误种类（决定行内红字用哪条模板——下载/导入「失败：{p}」、
/// 删除「删除失败：{p}」；错误原文存 model_err、模板在渲染体按当前 UI 语言取词）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum ModelErrKind {
    Download,
    Import,
    Delete,
}

/// 语言包行的预渲染数据（rsx 内不做过滤/插值/百分比计算）
struct ModelRowView {
    spec: &'static registry::ModelSpec,
    /// display(from)→display(to)；NLLB 行（kind==Nllb）为「多语言」文案（§6.1）
    label: String,
    size_mb: u64,
    installed: bool,
    /// 本行删除中（§6.2：显示「删除中…」、隐藏删除按钮，完成后清单刷新）
    deleting: bool,
    /// 本行下载中
    busy: bool,
    has_prog: bool,
    /// 进度百分比 0..=100（total 未知时 0，条形图不画）
    pct: u64,
    /// 「66/157 MB」；total 未知 → 「66 MB」
    prog_text: String,
    /// 失败文案「失败：{p}」插值后；空串 = 无
    err: String,
    /// 「语言包未安装」高亮（红边框）
    highlighted: bool,
}

/// MB 展示换算：十进制 MB 取整（与 registry size_mb 的展示口径一致）
fn mb_of(bytes: u64) -> u64 {
    (bytes + 500_000) / 1_000_000
}

/// 「更多语言包」目录子区一行（规格 F/G §6.3）的预渲染数据。
/// 静态四项由纯函数 [`catalog_rows`] 组装（§8：输入只有目录条目与安装清单，
/// 可离线直测）；动态项（version_text/busy/prog/err 依赖 UI 语言与运行中信号）
/// 由组件体在拿到 rows 后补齐——rsx 内不做过滤/插值/百分比计算。
struct CatalogRowView {
    /// 目录条目本体（下载按钮把原件传给 install_catalog_entry）
    entry: rumi_translate::download::CatalogEntry,
    /// display(from)→display(to)（未知码原样，如 "eo"→"eo"）
    label: String,
    /// 目录版本原串（如 "1.1"）
    version: String,
    /// 已安装（key 命中 installed 清单）→ 显示「已安装 ✓」、无下载按钮
    installed: bool,
    // ── 以下由组件体补齐 ──
    /// 「版本 {p}」插值后
    version_text: String,
    /// 本行下载中（busy_key 命中 entry.key）
    busy: bool,
    has_prog: bool,
    /// 进度百分比 0..=100（total 未知时 0，条形图不画）
    pct: u64,
    /// 「66/157 MB」；total 未知 → 「66 MB」
    prog_text: String,
    /// 「失败：{p}」插值后；空串 = 无
    err: String,
}

/// 目录条目 → 行渲染数据组装（规格 F/G §8 纯函数）：方向 display、版本透传、
/// 已安装判定。已安装 = entry.key 命中 installed 清单的 key（install_catalog_entry
/// 落盘目录名与 manifest key 均取 entry.key，installed() 原样回报目录名）；
/// busy/prog/err 依赖运行中信号，不进本函数——由组件体补齐。
fn catalog_rows(
    entries: &[rumi_translate::download::CatalogEntry],
    installed: &[rumi_translate::engine::InstalledInfo],
) -> Vec<CatalogRowView> {
    entries
        .iter()
        .map(|e| CatalogRowView {
            entry: e.clone(),
            label: format!("{}→{}", lang::display(&e.from), lang::display(&e.to)),
            version: e.version.clone(),
            installed: installed.iter().any(|i| i.key == e.key),
            version_text: String::new(),
            busy: false,
            has_prog: false,
            pct: 0,
            prog_text: String::new(),
            err: String::new(),
        })
        .collect()
}

/// 翻译页（规格 §3 布局/行为逐条）：工具行（语言⇄引擎翻译）+ 源文本 10 行 +
/// 结果 8 行只读（右上复制 btn-sm + 耗时）+ 离线语言包块。
#[component]
pub fn TranslatePage(
    mut tab: Signal<usize>,
    lang: Signal<String>,
    mut prefill: Signal<TranslatePrefill>,
) -> Element {
    let lang_v = lang();
    let t = |k: &'static str| i18n::ts(&lang_v, k);

    let st0 = settings::get().translate;
    let init_from = normalized_or(&st0.last_from, "en");
    let init_to = normalized_or(&st0.last_to, "zh");
    let mut from_sel = use_signal(move || init_from.clone());
    let mut to_sel = use_signal(move || init_to.clone());
    let mut engine_sel = use_signal(move || st0.engine.clone());
    let mut src = use_signal(String::new);
    let mut result = use_signal(String::new);
    let mut result_err = use_signal(|| Option::<String>::None);
    // 「语言包未安装」→ 高亮对应方向的语言包行（规格 §3.4）
    let highlight = use_signal(|| Option::<String>::None);
    let elapsed = use_signal(String::new);
    let running = use_signal(|| false);
    // 复制成功角标（沿用 .copied-chip inline + key 重放动画的模式，P0 #5）
    let mut copied_tick = use_signal(|| 0u64);
    // 已安装清单（进页与每次下载/导入成功后刷新；fs 扫描在线程）
    let installed = use_signal(Vec::<rumi_translate::engine::InstalledInfo>::new);
    // 下载/导入进行中的模型 key（"import" = 导入中；删除也置位——同一时刻
    // 只允许一个下载/导入/删除）；None = 空闲
    let busy_key = use_signal(|| Option::<String>::None);
    // 下载进度 (key, downloaded_bytes, total_bytes?)
    let prog = use_signal(|| Option::<(String, u64, Option<u64>)>::None);
    // 下载/导入/删除失败 (key, 原文, 种类)；对应行/入口红字按种类选模板
    let model_err = use_signal(|| Option::<(String, String, ModelErrKind)>::None);
    // 删除进行中的模型 key（§6.2；busy_key 同时置位 → 与下载/导入互斥）
    let deleting = use_signal(|| Option::<String>::None);
    // 更多语言包目录（§6.3）：展开态 + 拉取状态（首次展开拉一次，收起不丢）
    let mut catalog_open = use_signal(|| false);
    let catalog = use_signal(|| CatalogState::NotFetched);
    // 文档翻译（规格 K §5）：running 与文本翻译共用（互斥，规格指定「同一
    // busy 信号」）；doc_running 只标记「正在跑的是文档翻译」（区分按钮文案
    // 与状态区归属）。进度/状态/错误独立成组——模型下载的 prog 是字节口径，
    // 不与单元口径混用同一信号
    let doc_running = use_signal(|| false);
    let doc_prog = use_signal(|| Option::<(usize, usize)>::None);
    let doc_status = use_signal(String::new);
    let doc_err = use_signal(|| Option::<String>::None);
    // 双语布局（就地切换，初值取设置；切换即存回，与设置页同源）
    let mut doc_layout = use_signal(|| settings::get().translate.doc_layout);

    // 挂载刷新清单：本组件随 tab 挂载，每次进翻译页都重新扫描一次
    //（效果叠加规格要求的"启动时 + 每次操作后"）；effect 内无信号读取 → 只跑一次
    use_effect(move || {
        refresh_installed(installed);
    });

    // OCR 跳转预填消费（规格 §3.7）：text 非空 → 填入源文本并清空 prefill；
    // run=true 自动触发一次翻译。读 prefill 建立订阅——页面已打开时收到
    // prefill（如 auto_ocr 路径）也能即时响应；消费后 text 为空，下次重跑
    // 直接返回，读+写同一信号不会成环。
    use_effect(move || {
        if prefill.read().text.is_empty() {
            return;
        }
        let taken = {
            let mut w = prefill.write();
            take_prefill(&mut *w)
        };
        let Some((text, run)) = taken else { return };
        src.set(text);
        if run {
            start_translate(
                from_sel, to_sel, src, result, result_err, highlight, elapsed, running,
            );
        }
    });

    // 渲染体快照（rsx 内禁 let：全部计算在这里做完）
    let from_v = from_sel();
    let to_v = to_sel();
    let engine_v = engine_sel();
    let src_v = src();
    let result_v = result();
    let err_v = result_err();
    let elapsed_v = elapsed();
    let running_v = running();
    let copied_v = copied_tick();
    // 引擎可用性按渲染时设置快照判定（设置页改完切过来即生效）
    let tr_now = settings::get().translate;
    let (en_deepl, en_deeplx, en_openai) = (
        engine_enabled(&tr_now, "deepl"),
        engine_enabled(&tr_now, "deeplx"),
        engine_enabled(&tr_now, "openai"),
    );
    let need_cfg = !engine_enabled(&tr_now, &engine_v);
    let lbl_offline = engine_label("offline", &tr_now, &lang_v);
    let lbl_google = engine_label("google", &tr_now, &lang_v);
    let lbl_microsoft = engine_label("microsoft", &tr_now, &lang_v);
    let lbl_deepl = engine_label("deepl", &tr_now, &lang_v);
    let lbl_deeplx = engine_label("deeplx", &tr_now, &lang_v);
    let lbl_openai = engine_label("openai", &tr_now, &lang_v);
    // 语言包行视图
    let installed_v = installed();
    let busy_v = busy_key();
    let prog_v = prog();
    let model_err_v = model_err();
    let deleting_v = deleting();
    let highlight_v = highlight();
    let any_busy = busy_v.is_some();
    // 文档翻译（规格 K §5）渲染快照 + 状态文案（rsx 外插值）：进度回退
    // 「文档翻译中…」（首个回调前）；错误空串 = 无
    let doc_running_v = doc_running();
    let doc_prog_v = doc_prog();
    let doc_status_v = doc_status();
    let doc_err_v = doc_err();
    let doc_prog_text = match doc_prog_v {
        Some((done, total)) => i18n::ts(&lang_v, tr::DOC_PROGRESS)
            .replace("{p1}", &done.to_string())
            .replace("{p2}", &total.to_string()),
        None => i18n::ts(&lang_v, tr::DOC_RUNNING).to_string(),
    };
    let doc_failed_text = doc_err_v
        .as_ref()
        .map(|e| i18n::ts(&lang_v, tr::DOC_FAILED).replace("{p}", e))
        .unwrap_or_default();
    let import_err_text = model_err_v
        .as_ref()
        .and_then(|(_k, e, kind)| {
            (matches!(kind, ModelErrKind::Import))
                .then(|| i18n::ts(&lang_v, tr::MODELS_FAILED).replace("{p}", e))
        })
        .unwrap_or_default();
    let model_views: Vec<ModelRowView> = registry::MODELS
        .iter()
        .map(|spec| {
            // 已安装判定按方向匹配（registry key 安装包与导入包都覆盖：
            // 导入包的 manifest key 不在注册表内，但 from/to 命中同方向；
            // NLLB 条目 installed() 恒报 from=to="*"，与 spec 方向照常命中）
            let is_installed = installed_v
                .iter()
                .any(|i| i.from == spec.from && i.to == spec.to);
            let downloading = busy_v.as_deref() == Some(spec.key);
            let pv = prog_v.as_ref().filter(|(k, _, _)| k == spec.key);
            let (has_prog, pct, prog_text) = match pv {
                Some((_, d, Some(total))) if *total > 0 => (
                    true,
                    d.min(total) * 100 / *total,
                    i18n::ts(&lang_v, tr::MODELS_DOWNLOADING)
                        .replace("{p1}", &mb_of(*d).to_string())
                        .replace("{p2}", &mb_of(*total).to_string()),
                ),
                Some((_, d, _)) => (true, 0, format!("{} MB", mb_of(*d))),
                None => (false, 0, String::new()),
            };
            let err = model_err_v
                .as_ref()
                .and_then(|(k, e, kind)| {
                    (k == spec.key).then(|| {
                        let tmpl = if matches!(kind, ModelErrKind::Delete) {
                            tr::MODELS_DELETE_FAILED
                        } else {
                            tr::MODELS_FAILED
                        };
                        i18n::ts(&lang_v, tmpl).replace("{p}", e)
                    })
                })
                .unwrap_or_default();
            ModelRowView {
                spec,
                // NLLB 行（kind==Nllb，registry from/to 显示 "*"）：按 §6.1
                // 显示「多语言」文案，不拼方向
                label: if matches!(spec.kind, registry::ModelKind::Nllb) {
                    i18n::ts(&lang_v, tr::MODELS_NLLB_LABEL).to_string()
                } else {
                    format!("{}→{}", lang::display(spec.from), lang::display(spec.to))
                },
                size_mb: spec.size_mb,
                installed: is_installed,
                deleting: deleting_v.as_deref() == Some(spec.key),
                busy: downloading,
                has_prog,
                pct,
                prog_text,
                err,
                highlighted: highlight_v.as_deref() == Some(spec.key),
            }
        })
        .collect();

    // 目录子区（§6.3）视图：状态三路展开 → 拉取中 / 失败文案 / 条目列表
    let catalog_open_v = catalog_open();
    let catalog_v = catalog();
    let catalog_fetching = matches!(catalog_v, CatalogState::Fetching);
    let catalog_failed_text = match &catalog_v {
        CatalogState::Failed(e) => {
            Some(i18n::ts(&lang_v, tr::MODELS_FETCH_FAILED).replace("{p}", e))
        }
        _ => None,
    };
    let catalog_entries: Vec<rumi_translate::download::CatalogEntry> = match &catalog_v {
        CatalogState::Loaded(list) => list.clone(),
        _ => Vec::new(),
    };
    // 静态项走纯函数（§8），动态项（版本文案/进度/错误）在此按信号补齐
    let mut catalog_views = catalog_rows(&catalog_entries, &installed_v);
    for r in catalog_views.iter_mut() {
        let version_text = i18n::ts(&lang_v, tr::MODELS_VERSION).replace("{p}", &r.version);
        let pv = prog_v.as_ref().filter(|(k, _, _)| k == &r.entry.key);
        let (has_prog, pct, prog_text) = match pv {
            Some((_, d, Some(total))) if *total > 0 => (
                true,
                d.min(total) * 100 / *total,
                i18n::ts(&lang_v, tr::MODELS_DOWNLOADING)
                    .replace("{p1}", &mb_of(*d).to_string())
                    .replace("{p2}", &mb_of(*total).to_string()),
            ),
            Some((_, d, _)) => (true, 0, format!("{} MB", mb_of(*d))),
            None => (false, 0, String::new()),
        };
        let err = model_err_v
            .as_ref()
            .and_then(|(k, e, kind)| {
                // 删除类错误只会挂在内置行，不会落到目录行；这里仍按种类过滤
                (!matches!(kind, ModelErrKind::Delete) && k == &r.entry.key)
                    .then(|| i18n::ts(&lang_v, tr::MODELS_FAILED).replace("{p}", e))
            })
            .unwrap_or_default();
        r.version_text = version_text;
        r.busy = busy_v.as_deref() == Some(r.entry.key.as_str());
        r.has_prog = has_prog;
        r.pct = pct;
        r.prog_text = prog_text;
        r.err = err;
    }

    rsx! {
        div { class: "page",
            div { class: "page-head",
                span { class: "page-title", "{t(tr::TITLE)}" }
            }

            div { class: "card",
                // 工具行：源语言 ⇄ 目标语言 + 引擎 + 翻译（规格 §3 布局首行）
                div { class: "row",
                    label { class: "chip", style: "gap: 6px;",
                        "{t(tr::FROM)}"
                        select {
                            style: "padding: 2px 6px; font-size: var(--fs-s);",
                            onchange: move |e| {
                                let v = e.value();
                                from_sel.set(v.clone());
                                let _ = settings::update(move |st| { st.translate.last_from = v; Ok(()) });
                            },
                            for &l in lang::UI_LANGS.iter() {
                                option { value: "{l}", selected: from_v == l, "{lang::display(l)}" }
                            }
                            optgroup { label: "{t(tr::MORE_LANGS)}",
                                for &l in lang::UI_LANGS_MORE.iter() {
                                    option { value: "{l}", selected: from_v == l, "{lang::display(l)}" }
                                }
                            }
                        }
                    }
                    button { class: "btn-sm", title: "{t(tr::SWAP)}",
                        onclick: move |_| {
                            let (f0, t0) = (from_sel(), to_sel());
                            let (s0, r0) = (src(), result());
                            from_sel.set(t0.clone());
                            to_sel.set(f0.clone());
                            src.set(r0);
                            result.set(s0);
                            let _ = settings::update(move |st| {
                                st.translate.last_from = t0;
                                st.translate.last_to = f0;
                                Ok(())
                            });
                        },
                        "⇄" }
                    label { class: "chip", style: "gap: 6px;",
                        "{t(tr::TO)}"
                        select {
                            style: "padding: 2px 6px; font-size: var(--fs-s);",
                            onchange: move |e| {
                                let v = e.value();
                                to_sel.set(v.clone());
                                let _ = settings::update(move |st| { st.translate.last_to = v; Ok(()) });
                            },
                            for &l in lang::UI_LANGS.iter() {
                                option { value: "{l}", selected: to_v == l, "{lang::display(l)}" }
                            }
                            optgroup { label: "{t(tr::MORE_LANGS)}",
                                for &l in lang::UI_LANGS_MORE.iter() {
                                    option { value: "{l}", selected: to_v == l, "{lang::display(l)}" }
                                }
                            }
                        }
                    }
                    label { class: "chip", style: "gap: 6px; margin-left: auto;",
                        "{t(tr::ENGINE)}"
                        select {
                            style: "padding: 2px 6px; font-size: var(--fs-s);",
                            onchange: move |e| {
                                let v = e.value();
                                engine_sel.set(v.clone());
                                let _ = settings::update(move |st| { st.translate.engine = v; Ok(()) });
                            },
                            option { value: "offline", selected: engine_v == "offline", "{lbl_offline}" }
                            option { value: "google", selected: engine_v == "google", "{lbl_google}" }
                            option { value: "microsoft", selected: engine_v == "microsoft", "{lbl_microsoft}" }
                            option { value: "deepl", selected: engine_v == "deepl", disabled: !en_deepl, "{lbl_deepl}" }
                            option { value: "deeplx", selected: engine_v == "deeplx", disabled: !en_deeplx, "{lbl_deeplx}" }
                            option { value: "openai", selected: engine_v == "openai", disabled: !en_openai, "{lbl_openai}" }
                        }
                    }
                    // 当前引擎缺配置 → 下拉旁「去设置」小链接（规格 §3.3）
                    if need_cfg {
                        button { class: "btn-sm", onclick: move |_| tab.set(crate::TAB_SETTINGS),
                            "{t(tr::GO_SETTINGS)}" }
                    }
                    // 「翻译文档…」（规格 K §5）：btn-sm 不抢主按钮视觉；与
                    // 「翻译」共用 running 信号互斥（任一翻译进行中一并禁用）。
                    // 旁边就地放「双语布局」下拉（2026-11-04 用户反馈：藏设置页
                    // 找不到）——初值取 settings.doc_layout，切换即存回设置，
                    // start_doc_translate 现场读取
                    select {
                        style: "padding: 2px 6px; font-size: var(--fs-s);",
                        title: "{t(tr::SET_LAYOUT)}",
                        disabled: running_v,
                        onchange: move |e| {
                            let v = e.value();
                            doc_layout.set(v.clone());
                            let _ = settings::update(move |st| { st.translate.doc_layout = v; Ok(()) });
                        },
                        option { value: "inline", selected: doc_layout() == "inline", "{t(tr::SET_LAYOUT_INLINE)}" }
                        option { value: "page", selected: doc_layout() == "page", "{t(tr::SET_LAYOUT_PAGE)}" }
                        option { value: "only", selected: doc_layout() == "only", "{t(tr::SET_LAYOUT_ONLY)}" }
                    }
                    button { class: "btn-sm",
                        disabled: running_v,
                        onclick: move |_| start_doc_translate(
                            from_sel, to_sel, running, doc_running, doc_prog, doc_status, doc_err,
                        ),
                        if doc_running_v { "{t(tr::DOC_RUNNING)}" } else { "{t(tr::DOC_BUTTON)}" } }
                    button { class: "primary",
                        disabled: running_v || src_v.trim().is_empty(),
                        onclick: move |_| {
                            start_translate(
                                from_sel, to_sel, src, result, result_err, highlight, elapsed, running,
                            );
                        },
                        if running_v { "{t(tr::RUNNING)}" } else { "{t(tr::RUN)}" } }
                }

                // 文档翻译状态区（规格 K §5）：运行中显示进度行（首个回调前
                // 显示「文档翻译中…」）；结束后保留完成统计或红字错误，直到
                // 下次发起时重置
                if doc_running_v {
                    div { class: "hint", style: "margin-top: 4px; font-variant-numeric: tabular-nums; word-break: break-all;",
                        "{doc_prog_text}" }
                } else if !doc_failed_text.is_empty() {
                    div {
                        style: "margin-top: 4px; font-size: var(--fs-s); color: var(--danger); word-break: break-all;",
                        "{doc_failed_text}"
                    }
                } else if !doc_status_v.is_empty() {
                    div { class: "hint", style: "margin-top: 4px; word-break: break-all;",
                        "{doc_status_v}" }
                }

                // 源文本（10 行，placeholder 按规格）
                textarea {
                    rows: "10",
                    style: "margin-top: var(--sp1);",
                    placeholder: "{t(tr::SOURCE_PH)}",
                    value: "{src_v}",
                    oninput: move |e| src.set(e.value()),
                }

                // 结果区右上：复制 btn-sm + 耗时标注（规格 §3 布局二行）
                div { class: "row", style: "justify-content: flex-end; margin-top: var(--sp1);",
                    if copied_v > 0 {
                        span { class: "copied-chip inline", key: "{copied_v}",
                            "{t(i18n::k::BTN_COPIED)}" }
                    }
                    if !elapsed_v.is_empty() {
                        span { class: "hint", style: "font-variant-numeric: tabular-nums;",
                            "{elapsed_v}" }
                    }
                    button { class: "btn-sm",
                        disabled: result_v.is_empty(),
                        onclick: move |_| match crate::copy_text(&result()) {
                            Ok(()) => copied_tick.set(copied_v + 1),
                            Err(e) => result_err.set(Some(e)),
                        },
                        "{t(tr::COPY)}" }
                }
                // 结果（8 行，只读）
                textarea {
                    rows: "8",
                    readonly: true,
                    placeholder: "{t(tr::RESULT_PH)}",
                    value: "{result_v}",
                }
                // 翻译错误：红字（var(--danger)），旧译文保留在框里
                if let Some(e) = err_v {
                    div {
                        style: "margin-top: 4px; font-size: var(--fs-s); color: var(--danger); word-break: break-all;",
                        "{e}"
                    }
                }
            }

            // ── 离线语言包（规格 §3.6 + F/G §6）───────────────────
            div { class: "card", style: "margin-top: var(--sp2);",
                div { class: "row", style: "justify-content: space-between; align-items: center;",
                    span { class: "schema-head", style: "margin: 0;", "{t(tr::MODELS)}" }
                    div { class: "row",
                        if !import_err_text.is_empty() {
                            span {
                                style: "color: var(--danger); font-size: var(--fs-s); word-break: break-all; max-width: 420px;",
                                "{import_err_text}"
                            }
                        }
                        // 更多语言包目录（§6.3）：展开/收起子区；首次展开拉一次，
                        // 之后复用已加载清单（失败经子区里 [重试] 重拉）
                        button { class: "btn-sm",
                            onclick: move |_| {
                                let next = !catalog_open();
                                catalog_open.set(next);
                                if next && matches!(catalog(), CatalogState::NotFetched) {
                                    start_fetch_catalog(catalog);
                                }
                            },
                            "{t(tr::MODELS_MORE)}" }
                        button { class: "btn-sm",
                            disabled: any_busy,
                            onclick: move |_| start_import(busy_key, model_err, installed),
                            "{t(tr::MODELS_IMPORT)}" }
                    }
                }
                // 目录子区（§6.3）：行数可能上百 → 200px 内滚动（内联样式）
                if catalog_open_v {
                    div { class: "file-list",
                        style: "max-height: 200px; overflow-y: auto; border: 1px solid var(--border); border-radius: var(--radius-s); padding: 6px;",
                        if catalog_fetching {
                            span { class: "hint", "{t(tr::MODELS_FETCHING)}" }
                        }
                        if let Some(ftext) = catalog_failed_text {
                            div { class: "row",
                                span {
                                    style: "color: var(--danger); font-size: var(--fs-s); word-break: break-all;",
                                    "{ftext}"
                                }
                                button { class: "btn-sm",
                                    onclick: move |_| start_fetch_catalog(catalog),
                                    "{t(tr::MODELS_RETRY)}" }
                            }
                        }
                        for v in catalog_views {
                            div { class: "file-row",
                                span { style: "flex: none; font-weight: 500;", "{v.label}" }
                                span { class: "hint", style: "flex: none;", "{v.version_text}" }
                                div { style: "flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; justify-content: flex-end;",
                                    if v.installed {
                                        span { class: "hint", style: "color: var(--ok); flex: none;",
                                            "{t(tr::MODELS_INSTALLED)}" }
                                    } else if v.busy && v.has_prog {
                                        // 进度条：与内置行同款（复用 prog 信号）
                                        div { style: "flex: 1 1 140px; height: 4px; background: var(--panel-2); border-radius: 2px; overflow: hidden;",
                                            div { style: "width: {v.pct}%; height: 100%; background: var(--primary);" } }
                                        span { class: "hint", style: "flex: none; font-variant-numeric: tabular-nums;",
                                            "{v.prog_text}" }
                                    } else {
                                        button { class: "btn-sm",
                                            disabled: any_busy,
                                            onclick: move |_| start_catalog_download(
                                                v.entry.clone(), busy_key, prog, model_err, installed,
                                            ),
                                            "{t(tr::MODELS_DOWNLOAD)}" }
                                    }
                                    if !v.err.is_empty() {
                                        span {
                                            style: "color: var(--danger); font-size: var(--fs-s); word-break: break-all; flex: 1 1 180px; text-align: right;",
                                            "{v.err}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "file-list",
                    for v in model_views {
                        div { class: "file-row",
                            style: if v.highlighted { "border-color: var(--danger);" } else { "" },
                            span { style: "flex: none; font-weight: 500;", "{v.label}" }
                            span { class: "hint", style: "flex: none;", "{v.size_mb} MB" }
                            div { style: "flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; justify-content: flex-end;",
                                if v.installed {
                                    if v.deleting {
                                        // 删除进行中（§6.2）：红字提示；busy_key 置位
                                        // 期间其余下载/导入/删除按钮均已禁用
                                        span { class: "hint", style: "color: var(--danger); flex: none;",
                                            "{t(tr::MODELS_DELETING)}" }
                                    } else {
                                        span { class: "hint", style: "color: var(--ok); flex: none;",
                                            "{t(tr::MODELS_INSTALLED)}" }
                                        // 删除（§6.2）：红字 btn-sm；先 unload_key 再
                                        // remove 在后台线程完成，busy 期间全站禁用
                                        button { class: "btn-sm", style: "color: var(--danger);",
                                            disabled: any_busy,
                                            onclick: move |_| start_delete(
                                                installed_key_for(&installed(), v.spec),
                                                busy_key, deleting, model_err, installed,
                                            ),
                                            "{t(tr::MODELS_DELETE)}" }
                                    }
                                } else if v.busy && v.has_prog {
                                    // 进度条：简易 div width%（规格 §3.6 允许的内联实现）
                                    div { style: "flex: 1 1 140px; height: 4px; background: var(--panel-2); border-radius: 2px; overflow: hidden;",
                                        div { style: "width: {v.pct}%; height: 100%; background: var(--primary);" } }
                                    span { class: "hint", style: "flex: none; font-variant-numeric: tabular-nums;",
                                        "{v.prog_text}" }
                                } else {
                                    button { class: "btn-sm",
                                        disabled: any_busy,
                                        onclick: move |_| start_download(
                                            v.spec, busy_key, prog, model_err, installed, highlight,
                                        ),
                                        "{t(tr::MODELS_DOWNLOAD)}" }
                                }
                                if !v.err.is_empty() {
                                    span {
                                        style: "color: var(--danger); font-size: var(--fs-s); word-break: break-all; flex: 1 1 180px; text-align: right;",
                                        "{v.err}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 引擎可用性判定（规格 §7）：默认设置下离线/Google/Microsoft 恒可选、
    /// DeepL/OpenAI 不可选；DeepLX 因默认地址非空可选；配好对应项逐个解禁；
    /// 未知引擎串不可选。
    #[test]
    fn engine_enabled_follows_settings() {
        let st = TranslateSettings::default();
        assert!(engine_enabled(&st, "offline"));
        assert!(engine_enabled(&st, "google"));
        assert!(engine_enabled(&st, "microsoft"));
        assert!(!engine_enabled(&st, "deepl"), "默认无 DeepL key → 不可选");
        assert!(engine_enabled(&st, "deeplx"), "默认 DeepLX 地址非空 → 可选");
        assert!(!engine_enabled(&st, "openai"), "默认无 base_url → 不可选");
        assert!(!engine_enabled(&st, "garbage"), "未知引擎串不可选");

        // 配置齐了 → 逐个解禁
        let mut s2 = TranslateSettings::default();
        s2.deepl_api_key = "key".into();
        s2.openai_base_url = "http://127.0.0.1:8000/v1".into();
        assert!(engine_enabled(&s2, "deepl"));
        assert!(engine_enabled(&s2, "openai"));
        // 反向：清空即禁用
        s2.deeplx_url = String::new();
        assert!(!engine_enabled(&s2, "deeplx"));
    }

    /// 引擎下拉项文案：可选 = 纯名；缺配置 = 名 + 「（需配置）」后缀
    ///（规格 §3.3「DeepL（需配置）」）。
    #[test]
    fn engine_label_appends_need_config_suffix() {
        let st = TranslateSettings::default();
        let lang = "zh_CN";
        assert_eq!(engine_label("offline", &st, lang), i18n::ts(lang, tr::ENGINE_OFFLINE));
        assert_eq!(engine_label("google", &st, lang), "Google");
        assert_eq!(
            engine_label("deepl", &st, lang),
            format!("{}{}", i18n::ts(lang, tr::ENGINE_DEEPL), i18n::ts(lang, tr::NEED_CONFIG)),
        );
        let mut s2 = st.clone();
        s2.deepl_api_key = "k".into();
        assert_eq!(engine_label("deepl", &s2, lang), i18n::ts(lang, tr::ENGINE_DEEPL));
    }

    /// Engine 构造映射（规格 §7）：六个引擎串 → 对应变体（字段逐个核对），
    /// 未知串保守回退 Offline。
    #[test]
    fn engine_from_settings_maps_all_variants() {
        let base = TranslateSettings::default();
        assert!(matches!(engine_from_settings(&base), Engine::Offline));

        let garbage = TranslateSettings { engine: "garbage".into(), ..Default::default() };
        assert!(matches!(engine_from_settings(&garbage), Engine::Offline));

        let g = TranslateSettings { engine: "google".into(), ..Default::default() };
        assert!(matches!(engine_from_settings(&g), Engine::Google));
        let m = TranslateSettings { engine: "microsoft".into(), ..Default::default() };
        assert!(matches!(engine_from_settings(&m), Engine::Microsoft));

        let mut d = TranslateSettings {
            engine: "deepl".into(),
            ..Default::default()
        };
        d.deepl_api_key = "KEY".into();
        d.deepl_pro = true;
        match engine_from_settings(&d) {
            Engine::DeepL { api_key, pro } => {
                assert_eq!(api_key, "KEY");
                assert!(pro);
            }
            other => panic!("DeepL 映射错误：{other:?}"),
        }

        let x = TranslateSettings {
            engine: "deeplx".into(),
            deeplx_url: "http://127.0.0.1:1188/translate".into(),
            ..Default::default()
        };
        match engine_from_settings(&x) {
            Engine::DeepLX { url } => assert_eq!(url, "http://127.0.0.1:1188/translate"),
            other => panic!("DeepLX 映射错误：{other:?}"),
        }

        let o = TranslateSettings {
            engine: "openai".into(),
            openai_base_url: "http://127.0.0.1:8000/v1".into(),
            openai_api_key: "sk".into(),
            openai_model: "qwen".into(),
            ..Default::default()
        };
        match engine_from_settings(&o) {
            Engine::OpenAI { base_url, api_key, model, glossary } => {
                assert_eq!(base_url, "http://127.0.0.1:8000/v1");
                assert_eq!(api_key, "sk");
                assert_eq!(model, "qwen");
                // 默认设置无术语表 → 引擎带空表（不注入 prompt）
                assert!(glossary.is_empty(), "空设置应构造空术语表");
            }
            other => panic!("OpenAI 映射错误：{other:?}"),
        }
    }

    /// prefill 消费（规格 §7）：非空 → 取走 (文本, run) 并清空（读后清空语义）；
    /// 第二次消费 / 空 text（run 残留）→ None。
    #[test]
    fn take_prefill_consumes_once_then_empty() {
        let mut p = TranslatePrefill { text: "识别结果".into(), run: true };
        assert_eq!(take_prefill(&mut p), Some(("识别结果".to_string(), true)));
        assert_eq!(p, TranslatePrefill::default(), "消费后应清空");
        assert_eq!(take_prefill(&mut p), None, "第二次消费应空");

        let mut p2 = TranslatePrefill { text: String::new(), run: true };
        assert_eq!(take_prefill(&mut p2), None, "空 text 不触发");

        let mut p3 = TranslatePrefill { text: "x".into(), run: false };
        assert_eq!(take_prefill(&mut p3), Some(("x".to_string(), false)));
    }

    /// 语言码兜底（纯函数）：合法码原样、非法/空串回退默认方向。
    #[test]
    fn normalized_or_falls_back_on_unknown_lang() {
        assert_eq!(normalized_or("zh", "en"), "zh");
        assert_eq!(normalized_or("EN", "zh"), "en");
        assert_eq!(normalized_or("", "en"), "en");
        assert_eq!(normalized_or("garbage", "zh"), "zh");
        // 超出 UI_LANGS 候选的两位码同样回退（下拉没有对应 option）
        assert_eq!(normalized_or("zz", "en"), "en");
    }

    // ── 规格 F/G §8：目录行渲染数据组装（catalog_rows 纯函数，≥3 条）─────────

    /// 构造目录条目（url 不参与行渲染，置空）
    fn ce(key: &str, from: &str, to: &str, ver: &str) -> rumi_translate::download::CatalogEntry {
        rumi_translate::download::CatalogEntry {
            key: key.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            version: ver.to_string(),
            url: String::new(),
        }
    }

    /// 构造已安装条目（from/to/bytes 不参与 key 判定，置空/置 0）
    fn inst(key: &str) -> rumi_translate::engine::InstalledInfo {
        rumi_translate::engine::InstalledInfo {
            key: key.to_string(),
            from: String::new(),
            to: String::new(),
            bytes: 0,
        }
    }

    /// §8：方向 display 组装 + 版本透传；空安装清单 → 全部未安装
    #[test]
    fn catalog_rows_maps_direction_and_version() {
        let rows = catalog_rows(&[ce("en_ja", "en", "ja", "1.1")], &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].label,
            format!("{}→{}", lang::display("en"), lang::display("ja"))
        );
        assert_eq!(rows[0].version, "1.1");
        assert!(!rows[0].installed);
    }

    /// §6.3：已安装判定 = key 命中 installed 清单；key 不同（哪怕目录里方向
    /// 撞了别的条目）不算。fetch_catalog 已过滤内置 zh_en/en_zh，不会误标。
    #[test]
    fn catalog_rows_marks_installed_by_key_only() {
        let entries = [ce("en_ja", "en", "ja", "1.1"), ce("en_ko", "en", "ko", "1.0")];
        let installed = [inst("en_ja"), inst("zh_en")];
        let rows = catalog_rows(&entries, &installed);
        assert!(rows[0].installed, "key 命中 → 已安装");
        assert!(!rows[1].installed, "key 未命中 → 未安装（zh_en 是别的条目）");
    }

    /// §6.3：未知语言码 display 原样（如 "eo"→"eo"），不报错不回退
    #[test]
    fn catalog_rows_keeps_unknown_codes_as_is() {
        let rows = catalog_rows(&[ce("en_eo", "en", "eo", "1.2")], &[]);
        assert_eq!(rows[0].label, format!("{}→{}", lang::display("en"), "eo"));
    }

    /// 删除按钮的 key 解析：registry key 在装 → 用之；「已安装」来自方向匹配的
    /// 导入包 → 用导入包真实 key；都没装 → 回退 registry key（remove 幂等）
    #[test]
    fn installed_key_for_prefers_registry_then_direction_match() {
        use rumi_translate::engine::InstalledInfo;
        let spec = registry::by_key("zh_en").unwrap();
        // registry key 在装（含多余导入包）：直取 spec.key
        let list = vec![
            InstalledInfo { key: "zh_en".into(), from: "zh".into(), to: "en".into(), bytes: 1 },
            InstalledInfo { key: "my_pkg".into(), from: "zh".into(), to: "en".into(), bytes: 2 },
        ];
        assert_eq!(installed_key_for(&list, spec), "zh_en");
        // 仅导入包在装（手工 .argosmodel 同方向）：取导入包 key
        let list = vec![
            InstalledInfo { key: "my_pkg".into(), from: "zh".into(), to: "en".into(), bytes: 2 },
        ];
        assert_eq!(installed_key_for(&list, spec), "my_pkg");
        // 都没装：回退 spec.key（幂等删除不报错）
        assert_eq!(installed_key_for(&[], spec), "zh_en");
    }

    /// §8：多条目保持目录顺序；动态字段（busy/prog/err）初始为空——它们依赖
    /// 运行中信号，由组件体补齐，不在纯函数职责内
    #[test]
    fn catalog_rows_preserves_order_and_defaults_dynamic_fields() {
        let entries = [
            ce("en_ja", "en", "ja", "1.1"),
            ce("en_fr", "en", "fr", "1.9"),
            ce("en_ru", "en", "ru", "1.0"),
        ];
        let rows = catalog_rows(&entries, &[]);
        let keys: Vec<&str> = rows.iter().map(|r| r.entry.key.as_str()).collect();
        assert_eq!(keys, ["en_ja", "en_fr", "en_ru"]);
        assert!(rows.iter().all(|r| !r.busy && !r.has_prog && r.err.is_empty()));
    }

    /// §7 新词条（模型删除 + 目录子区）：zh / en 两表都要有——ts 对未知 key
    /// 原样返回 key 本身，漏配会被界面静默显示成 key，这里直接断言防回归
    #[test]
    fn model_delete_and_catalog_keys_exist_in_both_langs() {
        for lang in ["zh_CN", "en_US"] {
            for key in [
                tr::MODELS_DELETE,
                tr::MODELS_DELETING,
                tr::MODELS_DELETE_FAILED,
                tr::MODELS_NLLB_LABEL,
                tr::MODELS_MORE,
                tr::MODELS_FETCHING,
                tr::MODELS_FETCH_FAILED,
                tr::MODELS_RETRY,
                tr::MODELS_VERSION,
            ] {
                assert_ne!(i18n::ts(lang, key), key, "{lang} 缺词：{key}");
            }
        }
    }

    // ── 规格 J/K §7：文档翻译纯函数（默认输出名推导、过滤器串）+ 新词条 ──────

    /// §5/§7 默认输出名推导：同名去扩展 + `.bilingual.md`、存输入同目录
    /// （与 CLI --doc-out 默认值同口径）；无扩展名原样追加；多点扩展名取
    /// file_stem（去掉最后一段）；无文件名路径保守拼接不 panic。
    #[test]
    fn default_doc_out_appends_bilingual_md() {
        assert_eq!(
            default_doc_out(std::path::Path::new("/tmp/report.pdf")),
            std::path::Path::new("/tmp").join("report.bilingual.md"),
            "有扩展名：去扩展 + .bilingual.md、同目录"
        );
        assert_eq!(
            default_doc_out(std::path::Path::new("README")),
            std::path::Path::new("README.bilingual.md"),
            "无扩展名：原样追加"
        );
        assert_eq!(
            default_doc_out(std::path::Path::new("a.b.c")),
            std::path::Path::new("a.b.bilingual.md"),
            "多点扩展名：file_stem 口径"
        );
        assert_eq!(
            default_doc_out(std::path::Path::new("/")),
            std::path::Path::new("/.bilingual.md"),
            "无文件名路径：保守拼接"
        );
    }

    /// §5/§7 过滤器串构造：9 种扩展名与规格 §5 顺序逐字一致
    /// （rfd add_filter 直接吃该表）。
    #[test]
    fn doc_filter_extensions_match_spec() {
        assert_eq!(
            doc_filter_extensions(),
            ["pdf", "docx", "epub", "txt", "md", "html", "csv", "rtf", "srt"].as_slice(),
        );
    }

    /// §6 新词条（文档翻译）：zh / en 两表都要有——ts 对未知 key 原样返回
    /// key 本身，漏配会被界面静默显示成 key，这里直接断言防回归
    #[test]
    fn doc_translate_keys_exist_in_both_langs() {
        for lang in ["zh_CN", "en_US"] {
            for key in [
                tr::DOC_BUTTON,
                tr::DOC_RUNNING,
                tr::DOC_PROGRESS,
                tr::DOC_DONE,
                tr::DOC_FAILED,
                tr::DOC_FILTER,
            ] {
                assert_ne!(i18n::ts(lang, key), key, "{lang} 缺词：{key}");
            }
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::doc_strategy_from_layout;
    use rumi_translate::docmt::BiStrategy;

    /// 设置布局值 → 策略映射（未知/空回退 Inline——段落对照最安全默认）。
    #[test]
    fn layout_maps_to_strategy_with_safe_fallback() {
        assert_eq!(doc_strategy_from_layout("inline"), BiStrategy::Inline);
        assert_eq!(doc_strategy_from_layout("page"), BiStrategy::PageAlt);
        assert_eq!(doc_strategy_from_layout("only"), BiStrategy::Only);
        assert_eq!(doc_strategy_from_layout(""), BiStrategy::Inline);
        assert_eq!(doc_strategy_from_layout("garbage"), BiStrategy::Inline);
    }
}
