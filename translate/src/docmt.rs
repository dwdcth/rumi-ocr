//! 文档翻译：docforge 解析 → 翻译单元 → 引擎翻译 → 双语 Markdown 回写。
//!
//! v1 链路（doc-translate-spec §2）：任意 docforge 支持格式进、双语 Markdown
//! 出；docx/epub 回写上游到位后只加出口不改链路。解析默认不注入 OCR 引擎
//! ——扫描页 PDF 由 pdf 后端预检报错，本层转译为面向用户的行动指引（见
//! [`translate_parse_error`]）；`DocTranslateOptions.ocr_engine` 注入引擎后
//! 扫描页走 OCR→翻译同一管线（类型是 docforge 的 trait 对象，本 crate 不
//! 依赖 rumi-ocr，由上层装配适配器）。

use std::path::Path;

use docforge::writeback::stream::{WritebackFormat, WritebackSession};

use crate::cache::DocCache;

/// 双语策略（Inline/Only/Appendix 镜像 docforge::emit::BiStrategy；
/// PageAlt 为 rumi 层扩展，仅 markdown 输出完整实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BiStrategy {
    /// 原/译同段交错（原文块后紧跟译文块）——阅读对照形态。
    #[default]
    Inline,
    /// 仅译文（替换原文块文本）。
    Only,
    /// 原文完整 + 文末追加全部译文附录（按章节分组）。
    Appendix,
    /// 页级交替：一页原文接一页译文（页间 `---` 分隔）。
    /// 仅 **markdown 输出**完整实现；无页概念的格式（docx/odt 渲染时才分页、
    /// epub 页=spine 部件需动 OPF 结构）退化为 Appendix（原文全文+译文附录
    /// 的文档级两段式最近似），DocStats.note 携带退化说明。
    PageAlt,
}

impl BiStrategy {
    /// CLI/UI 字符串 → 策略；未知 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inline" => Some(BiStrategy::Inline),
            "only" => Some(BiStrategy::Only),
            "appendix" => Some(BiStrategy::Appendix),
            "page" | "pagealt" => Some(BiStrategy::PageAlt),
            _ => None,
        }
    }
}

impl From<BiStrategy> for docforge::emit::BiStrategy {
    fn from(s: BiStrategy) -> Self {
        match s {
            BiStrategy::Inline => docforge::emit::BiStrategy::BilingualInline,
            BiStrategy::Only => docforge::emit::BiStrategy::TranslationOnly,
            // PageAlt 的 docforge 侧映射 = Appendix（不支持页交替的格式的退化目标；
            // markdown 的页交替在 docmt 层自行拼接，不走该映射）
            BiStrategy::Appendix | BiStrategy::PageAlt => {
                docforge::emit::BiStrategy::OriginalPlusAppendix
            }
        }
    }
}

/// 输出扩展名 → 流式回写会话格式（docforge stream::WritebackFormat；htm 与
/// html 同义）。None = 该扩展名不走会话（.md/.markdown 是 rumi 侧 Document
/// 渲染、.pdf 是 bilingual_pdf 生成式、未知扩展名直接报错——session 均不适用）。
/// 返回 Some 的扩展名同时也是「保原格式回写要求同格式输入」的判定面（两份
/// 名单本是同一语义，共用一份避免漂移）。
fn session_writeback_format(ext: &str) -> Option<WritebackFormat> {
    match ext {
        "docx" => Some(WritebackFormat::Docx),
        "epub" => Some(WritebackFormat::Epub),
        "txt" => Some(WritebackFormat::Txt),
        "html" | "htm" => Some(WritebackFormat::Html),
        "fb2" => Some(WritebackFormat::Fb2),
        "odt" => Some(WritebackFormat::Odt),
        _ => None,
    }
}

/// whatlang 的 ISO 639-3 码 → rumi 内部码（auto 源语言检测专用，见
/// [`DocTranslateOptions::from`]）。必须显式建表而不能只靠
/// [`crate::lang::normalize`] 兜底——normalize 对未收录三字码走「前两位
/// 截取」：eng→en、ita→it 恰好对，cmn→cm、por→po、jpn→jp 这类就错
/// （lang::ALIASES 已收 eng/jpn 等部分码，但 whatlang 的 Mandarin 码是
/// "cmn" 不在任何别名表里，漏一个错一个）。常见 13 语（UI_LANGS 常用组）
/// 逐字列出；未收录语言由 [`iso639_3_to_rumi`] 走 normalize 兜底。
const ISO639_3_TO_RUMI: &[(&str, &str)] = &[
    ("cmn", "zh"), // 中文普通话：whatlang 的 Mandarin 码是 cmn；宏码 zho 同映射
    ("zho", "zh"),
    ("eng", "en"),
    ("jpn", "ja"),
    ("kor", "ko"),
    ("fra", "fr"),
    ("deu", "de"),
    ("spa", "es"),
    ("rus", "ru"),
    ("por", "pt"),
    ("ita", "it"),
    ("vie", "vi"),
    ("ara", "ar"),
    ("tha", "th"),
];

/// ISO 639-3 → rumi 内部码：映射表（[`ISO639_3_TO_RUMI`]）命中直取；未收录
/// 走 [`crate::lang::normalize`] 兜底（截不出两位字母码的原样小写返回，交由
/// 引擎按未知语言处理）。纯函数可测（auto 检测方向的正确性单测打在这）。
pub fn iso639_3_to_rumi(lang6393: &str) -> String {
    let key = lang6393.trim().to_ascii_lowercase();
    match ISO639_3_TO_RUMI.iter().find(|(k, _)| *k == key.as_str()) {
        Some((_, v)) => (*v).to_string(),
        None => crate::lang::normalize(&key),
    }
}

/// 按键分组并保持首现顺序（auto 源语言的分组批）：返回 (键, 同键下标升序表)。
/// 纯函数可测——「不同源语言不能同批」是离线 CT2 批调用的硬约束
///（engine::translate_texts 单 from，混合源语言的批会按错误方向整批翻译）。
pub fn group_indices(keys: &[String]) -> Vec<(String, Vec<usize>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, k) in keys.iter().enumerate() {
        if !groups.contains_key(k) {
            order.push(k.clone());
        }
        groups.entry(k.clone()).or_default().push(i);
    }
    order
        .into_iter()
        .map(|k| {
            let idx = groups.remove(&k).unwrap_or_default();
            (k, idx)
        })
        .collect()
}

/// 文档翻译选项。
pub struct DocTranslateOptions {
    pub engine: crate::Engine,
    /// 内部码；"auto" = 逐单元自动检测——docforge 块级 whatlang 检测的
    /// `unit.lang`（ISO 639-3 码，见 [`parse_document`] 的 policy 说明）经
    /// [`iso639_3_to_rumi`] 转内部码作该单元实际 from；检测不出的单元
    /// 译文=原文（诚实：检测不出不瞎翻）。缓存键用单元实际 from（每单元
    /// 不同 from 天然不串键）。
    pub from: String,
    pub to: String,
    pub strategy: BiStrategy,
    /// 与 facade 同义（zh 目标时润色）。
    pub polish: bool,
    /// 最小字符数（docforge 同款默认 2：滤单字符噪声）。
    pub min_chars: usize,
    /// 进度回调 (已翻译单元数, 总单元数, 最近完成单元的章节名)：第三参取
    /// `unit.chapter`（批执行时为批内最后完成单元的 chapter；无章节文档
    /// 恒 None——UI 可据此决定是否加"第三章 · "前缀）。调用方取消语义
    /// 不做（返回 false 中止留 v2）。
    pub on_progress: Option<std::sync::Arc<dyn Fn(usize, usize, Option<&str>) + Send + Sync>>,
    /// 翻译缓存（默认 true）：文本级命中跳过引擎；进度照常推进。
    /// from==to 的直通路径不经过缓存（无引擎成本，无缓存意义）。
    pub use_cache: bool,
    /// 术语表（glossary 规格 §3）：单元文本精确命中 → 直接回填固定译法，
    /// 跳过 running memo/缓存/引擎一切成本（插在防幻觉之后，见
    /// [`translate_document`] 判定链）；OpenAI 兼容引擎另把整表注入
    /// system prompt。默认空表，全链路照旧。
    pub glossary: crate::glossary::Glossary,
    /// 扫描页 PDF 的 OCR 引擎注入：Some 时填给 pdf 后端 `PdfOptions.engine`
    /// （Auto 逐页判定——文字层页直提、扫描页渲染后走该引擎；非 pdf 格式
    /// 不消费）。None 维持 v1 语义：扫描页预检报错，转译为行动指引（见
    /// [`translate_parse_error`]）。类型是 docforge 的 trait 对象，本 crate
    /// 不依赖 rumi-ocr：上层（ui）用 `rumi_ocr::RumiOcrAdapter` 包装全局
    /// 引擎再注入，避免模型双重加载。
    pub ocr_engine: Option<std::sync::Arc<dyn docforge::ocr::OcrEngine>>,
}

/// 文档翻译统计。
#[derive(Debug)]
pub struct DocStats {
    /// 翻译单元数。
    pub units: usize,
    /// 送译原文总字符数（单元 text 按 chars 计）。
    pub chars: usize,
    pub elapsed_ms: u128,
    /// 策略退化说明（如 PageAlt 遇无页概念格式退化为附录时的提示）；
    /// None = 策略按所选完整生效。UI/CLI 在完成态展示。
    pub note: Option<String>,
    /// 缓存命中单元数（use_cache=false 或 from==to 直通时恒 0）。
    /// UI/CLI「缓存命中 n/N」展示待后续，字段先记。
    pub cache_hits: usize,
    /// 跨页重复（页眉/页脚类，docforge running-detect 标记）单元被合并
    /// 复用的次数——同文本只翻一次，后续页复用译文（锚点不缺位）。
    pub running_merged: usize,
    /// 术语表精确命中单元数（命中即固定译法、零引擎成本；glossary 规格 §3）。
    pub glossary_hits: usize,
}

/// 进度回调节流：每 16 单元回调一次（末批不足也回调），UI 不必逐单元重绘。
const UNITS_PER_BATCH: usize = 16;

/// 主入口：input（任意 docforge 支持格式）→ out 双语成品。
///
/// **输出格式按 out 扩展名分派**（大小写不敏感）：`.md`/`.markdown` → 双语
/// Markdown；`.docx` → docforge 定位改写回写（保原样式/资源，仅动正文 XML，
/// 原件为 docx）；`.epub` → epub 回写（原件为 epub）；`.pdf` → 双语对照
/// PDF（纯生成式，任何输入格式可出，见 [`crate::bilingual_pdf`]）。
/// 其他扩展名 Err。
/// docx/epub 回写由 docforge `writeback::*` 重解析原文件对齐 id 锚点
/// （与本函数提取侧同口径），translation 单元 id 不漂移。
///
/// 流程：parse → to_translation_units_with(min_chars) → 判定链预扫
/// （防幻觉 → glossary → running memo → 缓存 → from==to 直通，命中的当场
/// 定稿，未命中记入 pending）→ pending 按 16 条/批攒批送引擎（离线走
/// [`crate::engine`] 单例的 translate_texts 一次 CT2 批——CT2 吞吐在 batch，
/// 逐条 batch(1) 是大文档的主要吞吐浪费点；在线保持逐条 [`crate::translate`]）
/// → 每批完成回调一次 on_progress → 回写落盘。任一单元失败即整体 Err
/// （"单元 {id} 翻译失败"；CT2 整批 Err 定位到批内首 pending 单元）。
///
/// 回写分派（超大文档内存优化，docforge@35a1d99e 流式会话）：默认策略
/// （Inline/Only/Appendix）+ 保格式输出（.docx/.epub/.txt/.html/.htm/.fb2/
/// .odt——本来就要求同格式输入，session 语义完全匹配）走
/// [`WritebackSession`] 流式回写——译文产出点（预扫命中 / 每批完成）即
/// push 直落 spill 磁盘（零 RAM 驻留），判定链+批全部完成后 finish 一次性
/// 打包（与对应全量 writeback_* 同解析器/策略/忽略口径，仅译文喂入从
/// 一次性全集变流式），翻译期间不再驻留全量译文 Vec。PageAlt（md/epub/
/// docx 交替与退化附录）与 .md/.pdf 输出维持全量 translated 路径。任一
/// new/push/finish 失败整体 Err（"回写会话失败：{e}"）；中途失败 spill
/// 临时文件随会话 Drop 兜底删除（docforge 侧自管，无需手动清理）。
///
/// use_cache=true 时预扫先查文本 hash 缓存（[`DocCache`]，键=文本+翻译参数，
/// 命中跳过引擎但进度照常推进），批内新译文写缓存并在每批边界与收尾各
/// 落盘一次——断点续翻（中断重跑已翻单元全命中）+ 同文本只翻一次
/// （页眉页脚重复文本一石二鸟）。
///
/// from=="auto"（逐单元自动检测）：解析时开 docforge 块级语言检测（见
/// [`parse_document`]），每个单元的实际 from = `unit.lang` 经
/// [`iso639_3_to_rumi`] 映射；检测不出（`unit.lang` None）的单元译文=原文
/// 不进批；pending 按**实际源语言分组批**（[`group_indices`]，组内再按
/// 16 条攒批）——engine::translate_texts 单 from，不同源语言不能同批。
/// 每批完成回调一次 on_progress（第三参=批内最后完成单元的章节名）。
pub fn translate_document(
    input: &Path,
    out: &Path,
    opts: &DocTranslateOptions,
) -> Result<DocStats, String> {
    let t0 = std::time::Instant::now();
    // 文件存在性预检先于解析：确定性结论统一口径（pdf 后端对缺文件报
    // "文件不存在"，文本族则报 cannot read——docmt 层先行拦截）
    if !input.is_file() {
        return Err(format!("文件不存在：{}", input.display()));
    }
    // auto 源语言检测开关：from 字面 "auto"（大小写/空白宽容）触发逐单元
    // 检测。**不能**经 lang::normalize 判定——normalize("auto") 走「前两位
    // 截取」得 "au"，恒不等于 "auto"。
    let auto_from = opts.from.trim().eq_ignore_ascii_case("auto");
    // 块级语言检测只在 auto 时开（逐块 whatlang 有成本；固定 from 行为与
    // 旧版完全一致）
    let doc = parse_document(input, opts.ocr_engine.clone(), auto_from)?;
    let units = docforge::emit::to_translation_units_with(&doc, opts.min_chars);
    if units.is_empty() {
        return Err("文档无可翻译文本单元（空文件或全为非文本块）".to_string());
    }
    let total = units.len();
    // 缓存装配：use_cache 开且非 from==to 直通才加载——直通走 facade 短路
    // 零引擎成本，进缓存无意义（不读不写，也不碰用户缓存目录）。路径用
    // RUMI_TRANSLATE_CACHE 覆盖缺省数据目录（测试隔离的钩子，见下）。
    // auto 模式 from=="auto" 不会等于 to，缓存照常加载（键=单元实际 from）。
    let passthrough = crate::lang::normalize(&opts.from) == crate::lang::normalize(&opts.to);
    let mut cache = if opts.use_cache && !passthrough {
        Some(DocCache::load_at(
            cache_path_override().unwrap_or_else(DocCache::default_path),
        ))
    } else {
        None
    };
    let mut cache_hits = 0usize;
    // OpenAI 兼容引擎才有 prompt 注入通道（glossary 规格 §4）：术语表非空时
    // 克隆引擎枚举填入 (原文, 译文) 对——循环外一次性，避免逐单元克隆；
    // google/microsoft/deepl 无 prompt 通道不注入（精确匹配已覆盖关键场景），
    // 术语表为空直接用原引擎（行为与改动前完全一致）。
    let engine = match &opts.engine {
        crate::Engine::OpenAI {
            base_url,
            api_key,
            model,
            ..
        } if !opts.glossary.is_empty() => crate::Engine::OpenAI {
            base_url: base_url.clone(),
            api_key: api_key.clone(),
            model: model.clone(),
            glossary: opts
                .glossary
                .terms
                .iter()
                .map(|t| (t.src.clone(), t.dst.clone()))
                .collect(),
        },
        _ => opts.engine.clone(),
    };
    let glossary = &opts.glossary;
    let mut glossary_hits = 0usize;
    // 跨页重复（页眉/页脚类）单元的文本 → 译文 memo（docforge running-detect
    // 标记，d24c61f0 起）：同文本只送引擎一次，后续页复用——每页锚点都有
    // 译文（回写/附录不缺位），页眉不再翻 N 遍灌满附录。与文本 hash 缓存
    // 天然叠加（第二遍运行时缓存已全命中）。
    let mut running_memo: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut running_merged = 0usize;
    let mut chars = 0usize;
    // 输出/输入扩展名（小写比较；无扩展名按不支持处理——CLI/UI 的默认输出
    // 恒带 .md，走到这里的无扩展名只会是显式误用）。提前到判定链之前：回写
    // 会话判定与其同格式前置检查都要用，纯路径检视行为不变
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let in_ext = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    // 保原格式回写要求输入与输出同格式（writeback 重解析原文件定位改写）；
    // 跨格式需求（如 txt→docx）一律走 .md 输出。前置友好报错替代深层 ZIP
    // 错误。（原在回写阶段检查，随会话建立一并前置——判定链/翻译是可能花
    // 钱花时间的，注定失败的请求不该先跑完再报；文案与口径不变）
    let needs_same_input = session_writeback_format(&ext).is_some();
    if needs_same_input && in_ext != ext {
        return Err(format!(
            "输出 .{ext} 需要同格式输入（当前输入 .{in_ext}）；跨格式请输出 .md"
        ));
    }
    // ---------- 回写会话建立（判定链之前：spill 就绪，译文产出即直落盘） ----------
    // 默认策略（Inline/Only/Appendix）+ 保格式输出组合走流式回写；PageAlt
    // 已排除，strategy 映射即 opts.strategy.into()。会话带 Drop 兜底（docforge
    // stream 侧成败路径都删 spill），中途任何 Err 直接 ? 返回即可。
    let mut session: Option<WritebackSession> = None;
    if opts.strategy != BiStrategy::PageAlt {
        if let Some(fmt) = session_writeback_format(&ext) {
            session = Some(
                WritebackSession::new(fmt, input, out, opts.strategy.into())
                    .map_err(|e| format!("回写会话失败：{e}"))?,
            );
        }
    }
    // 译文暂存（下标=单元序）：仅全量路径（PageAlt 交替 / .md/.pdf 输出）
    // 构造；session 路径判定链命中与批结果都直落 spill，这里恒空——内存
    // 收益就在这一处不攒全量译文
    let mut texts_out: Vec<String> = if session.is_some() {
        Vec::new()
    } else {
        Vec::with_capacity(total)
    };
    // ---------- 阶段一：判定链预扫（不触引擎、不回调进度） ----------
    // 逐单元依次判 防幻觉 → glossary 精确命中 → running memo → 缓存 →
    // from==to 直通，命中的当场定稿；否则记入 pending（单元下标），留待
    // 阶段二攒批送引擎。两遍结构（预扫 + 批执行）逐位复刻旧单遍逐条流的
    // 判定链顺序与回填结果——统计口径（cache_hits/glossary_hits/running_merged）
    // 与输出不变。session 在会话存在时改道直落 spill（见循环尾）。
    // 每单元实际源语言（auto 检测的载体，阶段二分组批/缓存键/引擎调用都用
    // 它）：固定 from 恒 opts.from（现状）；auto 时 unit.lang 经 639-3 映射，
    // None = 检测不出。与 texts_out 同长同序。
    let mut unit_froms: Vec<Option<String>> = Vec::with_capacity(total);
    let mut pending: Vec<usize> = Vec::new();
    // running 单元的批内去重（文本 → 首个 pending 槽位）：旧逐条流靠"首现
    // 翻完记 memo"让后续页眉复用同一译文；两阶段流里 memo 要等批执行才填，
    // 预扫阶段用槽位等价复刻——同文本只进批一次，批内重复单元挂到首现
    // 槽位取译文（running_merged 口径不变）。
    let mut pending_slots: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    // 批内重复的 running 文本（单元下标, 首现槽位）——首现槽位所在批完成时
    // 统一回填（session 路径译文不驻留 texts_out，只能在产出点消费）
    let mut batch_refs: Vec<(usize, usize)> = Vec::new();
    for (idx, unit) in units.iter().enumerate() {
        chars += unit.text.chars().count();
        // 单元实际源语言：auto 时取块级检测结果（whatlang ISO 639-3 码）转
        // rumi 码；检测不出（样本 < 20 字符或 whatlang 未命中）为 None
        let from_u: Option<String> = if auto_from {
            unit.lang.as_deref().map(iso639_3_to_rumi)
        } else {
            Some(opts.from.clone())
        };
        unit_froms.push(from_u.clone());
        let text = if !unit.has_translatable_text() {
            // 防幻觉过滤（docforge@b5bf9a87 谓词）：220V / -10 to 50 C 这类纯
            // 数字/单位/短代码文本送引擎只会得到幻觉（实测 "220V 电话"），
            // 直接原样回填——译文=原文，回写锚点不受影响（也不进缓存）
            unit.text.clone()
        } else if from_u.is_none() {
            // auto 且检测不出：译文=原文。放在缓存之前——缓存键没有可用的
            // from；诚实语义：检测不出不瞎翻，宁可不译也不按错误方向翻
            unit.text.clone()
        } else if let Some(dst) = glossary.exact_match(&unit.text) {
            // 术语表精确命中（glossary 规格 §3，插在防幻觉之后、缓存之前
            // 的顺序铁律）：固定译法直接回填，跳过 running memo/缓存/引擎
            // 一切成本；也不写缓存——改术语立即生效、不留脏缓存。from==to
            // 直通（旧流 facade 短路）也晚于本判定。
            glossary_hits += 1;
            dst.to_string()
        } else if unit.running && running_memo.contains_key(&unit.text) {
            // memo 由阶段二批执行填充，预扫时恒空（批内重复由 pending_slots
            // 去重承担，见上）——分支保留以对齐判定链顺序铁律
            running_merged += 1;
            running_memo.get(&unit.text).unwrap().clone()
        } else if let Some(hit) = cache.as_ref().and_then(|c| {
            c.get(
                &unit.text,
                from_u.as_deref().unwrap_or(&opts.from),
                &opts.to,
                &engine,
                opts.polish,
            )
        }) {
            // 文本级命中（同文本跨单元/跨文档/页眉页脚只翻一次）；进度照常推进
            // ——进度是"处理进度"不是"翻译成本"。缓存键用单元实际 from
            //（auto 每单元可能不同，天然不串键）
            cache_hits += 1;
            hit
        } else if crate::lang::normalize(from_u.as_deref().unwrap_or(&opts.from))
            == crate::lang::normalize(&opts.to)
        {
            // from==to 直通（旧流发生在 crate::translate/facade 内部，位于
            // 缓存之后）：预扫直接定稿——pending 全空、引擎零接触。单元粒度
            // 判定：固定 from 时与旧全局判定完全等价；auto 时混合文档的
            // "检测语言==目标语言"单元同样零引擎成本原样
            unit.text.clone()
        } else if unit.running {
            if let Some(&slot) = pending_slots.get(unit.text.as_str()) {
                // 批内重复的 running 文本（memo 未填的后续页眉）：挂首现
                // 槽位，翻完取用——同文本只送批一次
                running_merged += 1;
                batch_refs.push((idx, slot));
                String::new() // 占位，阶段二统一回填
            } else {
                pending_slots.insert(unit.text.as_str(), pending.len());
                pending.push(idx);
                String::new() // 占位，阶段二回填
            }
        } else {
            pending.push(idx);
            String::new() // 占位，阶段二回填
        };
        match session.as_mut() {
            Some(s) => {
                // 判定链当场定稿的条目（防幻觉原样 / 术语 / running 复用 /
                // 缓存命中 / 直通）立即 push 进会话——译文帧直落 spill 磁盘。
                // pending 占位空串不入（阶段二批完成后再 push 真译文）；
                // session.push 对空白本就静默忽略，这里跳过省一次调用
                if !text.is_empty() {
                    s.push(&unit.id, &text)
                        .map_err(|e| format!("回写会话失败：{e}"))?;
                }
            }
            None => texts_out.push(text),
        }
    }
    // ---------- 阶段二：pending 攒批执行 ----------
    // 每批 16 条（复用 UNITS_PER_BATCH——批边界同时是 save 与进度回调边界，
    // 断点续翻与节流语义不变）。离线引擎整批走 engine::shared().translate_texts
    // （一次 CT2 批）；在线引擎保持逐条 crate::translate（provider 自有批量/
    // 限流语义，不掺和离线批）。
    if !pending.is_empty() {
        // 分组批：auto 模式 pending 里各单元的实际源语言可能不同——
        // engine::translate_texts 单 from，不同源语言不能同批。按实际 from
        // 分组（[`group_indices`] 保首现序）、组内再按 16 条攒批；固定 from
        // 时全 pending 同键一组，分组退化为既有的单序攒批（行为不变）。
        let keys: Vec<String> = pending
            .iter()
            .map(|&i| unit_froms[i].clone().unwrap_or_default())
            .collect();
        let to = crate::lang::normalize(&opts.to);
        let prescanned = total - pending.len();
        // 分组打乱了下标顺序，进度"已处理数"用计数器推进（等价于旧
        // prescanned + (ci+1)*UNITS_PER_BATCH 的单序口径）
        let mut processed = prescanned;
        for (from_g, idxs) in group_indices(&keys) {
            // 离线批量入口不做方向归一（facade 的 normalize 在其内部做），显式
            // 传入归一码；缓存键用单元实际 from（原始码），语义不变
            let from_norm = crate::lang::normalize(&from_g);
            for chunk in idxs.chunks(UNITS_PER_BATCH) {
                // chunk 元素是 pending 槽位（group_indices 返回的 keys 下标），
                // 取文本/回填前必须经 pending[i] 映射回单元下标——e294a7c 引入
                // 分组批时漏了这层映射，预扫命中在前时整批错位（该回归无测试
                // 覆盖：既有用例的 pending 恒为空或恰为全序 0..n），随本处
                // session 化重构一并修复
                let batch: Vec<String> = chunk
                    .iter()
                    .map(|&i| units[pending[i]].text.clone())
                    .collect();
                let outs = match &engine {
                    crate::Engine::Offline => {
                        // 整批 Err（CT2 批调用不区分单条）——错误定位到批内首
                        // pending 单元，粗粒度可接受（与现状"任一单元失败整体
                        // Err"一致）
                        crate::engine::shared()
                            .translate_texts(&batch, &from_norm, &to)
                            .map(|mut outs| {
                                // 与 facade 同款收尾：zh 润色 + ▁ 清理（引擎出口
                                // 已 strip，此处幂等重复无害）
                                if opts.polish && to == "zh" {
                                    for t in outs.iter_mut() {
                                        *t = crate::postfix::polish_zh(t);
                                    }
                                }
                                for t in outs.iter_mut() {
                                    *t = crate::strip_sp_marker(t);
                                }
                                outs
                            })
                            .map_err(|e| {
                                format!("单元 {} 翻译失败：{}", units[pending[chunk[0]]].id, e)
                            })?
                    }
                    _ => {
                        // 在线逐条：错误定位逐单元（与旧循环逐字一致）；from
                        // 用单元实际值（固定模式 == opts.from，行为不变）
                        let mut outs = Vec::with_capacity(chunk.len());
                        for &i in chunk {
                            let ui = pending[i];
                            let t = crate::translate(
                                std::slice::from_ref(&units[ui].text),
                                unit_froms[ui].as_deref().unwrap_or(&from_g),
                                &opts.to,
                                &engine,
                                opts.polish,
                            )
                            .map_err(|e| format!("单元 {} 翻译失败：{}", units[ui].id, e))?
                            .pop()
                            .unwrap_or_default();
                            outs.push(t);
                        }
                        outs
                    }
                };
                for (slot_in_chunk, &i) in chunk.iter().enumerate() {
                    let ui = pending[i];
                    let tr = &outs[slot_in_chunk];
                    // 批结果两条去向：session 路径逐条 push 直落 spill（每批
                    // 完成即入会话，不等全量）；全量路径回填 texts_out
                    match session.as_mut() {
                        Some(s) => {
                            s.push(&units[ui].id, tr)
                                .map_err(|e| format!("回写会话失败：{e}"))?;
                        }
                        None => texts_out[ui] = tr.clone(),
                    }
                    if let Some(c) = cache.as_mut() {
                        c.insert(
                            &units[ui].text,
                            unit_froms[ui].as_deref().unwrap_or(&from_g),
                            &opts.to,
                            &engine,
                            opts.polish,
                            tr,
                        );
                    }
                    // running 首现翻完记入 memo（后续同文本页复用；批内重复已在
                    // 预扫挂槽位，不重复进批）
                    if units[ui].running {
                        running_memo.insert(units[ui].text.clone(), tr.clone());
                    }
                }
                // 批内重复 running 文本（预扫挂起的 (重复单元, 首现槽位)）：
                // 首现落在本批的此刻取同译文回填/入会话。同文本必同键同组，
                // 槽位恰在本批被消费一次；session 路径没有 texts_out 可事后
                // 回填，统一挪到译文产出点（全量路径结果等价，只是提前）
                let mut unmatched_refs = Vec::new();
                for (dup_idx, slot) in batch_refs.drain(..) {
                    match chunk.iter().position(|&s| s == slot) {
                        Some(pos) => {
                            let tr = &outs[pos];
                            match session.as_mut() {
                                Some(s) => {
                                    s.push(&units[dup_idx].id, tr)
                                        .map_err(|e| format!("回写会话失败：{e}"))?;
                                }
                                None => texts_out[dup_idx] = tr.clone(),
                            }
                        }
                        None => unmatched_refs.push((dup_idx, slot)),
                    }
                }
                batch_refs = unmatched_refs;
                // 断点续翻：每批边界落盘一次（崩溃最多丢一批）
                if let Some(c) = cache.as_ref() {
                    c.save();
                }
                // 进度回调：每批完成一次（done 含预扫已分流单元——进度是"处理
                // 进度"不是"翻译成本"，粒度从逐条变 16 条一跳，节流语义不变）；
                // 第三参=批内最后完成单元的章节名（① 规格指定口径）
                processed += chunk.len();
                let done = processed.min(total);
                let last = pending[chunk[chunk.len() - 1]];
                if let Some(cb) = &opts.on_progress {
                    cb(done, total, units[last].chapter.as_deref());
                }
            }
        }
    } else if let Some(cb) = &opts.on_progress {
        // 全命中（含 from==to 直通 / auto 全检测不出）：预扫全分流、pending
        // 空、引擎零接触；旧逐条流收尾处也会回调一次 (total, total)。
        // 第三参=文档最后单元的章节名（"最近完成单元"在收尾点的自然口径）
        let last_chapter = units.last().and_then(|u| u.chapter.as_deref());
        cb(total, total, last_chapter);
    }
    // 收尾落盘（全命中时是同内容覆写，幂等无害；缺了这步不满一批的中断前
    // 进度会全丢）
    if let Some(c) = cache.as_ref() {
        c.save();
    }
    // PageAlt 的格式支持面：markdown 页级交替（docforge@6a1e183 起）、
    // epub 部件交替（b5bf9a87 起，spine 插译文部件）、docx 节交替
    // （9eb3d98d 起，按原文档分节/分页符边界"原文节→分页→译文节"，
    // Word 无真页下的最佳近似）、pdf 按源页分组对照（bilingual_pdf 天然
    // 就是"原文引用 + 译文"的页节形态，无退化可言）；其余无页概念格式
    // 仍退化为附录
    let epub_alt = opts.strategy == BiStrategy::PageAlt
        && ext == "epub"
        && in_ext == "epub";
    let docx_alt = opts.strategy == BiStrategy::PageAlt
        && ext == "docx"
        && in_ext == "docx";
    let note = if opts.strategy == BiStrategy::PageAlt
        && !matches!(ext.as_str(), "md" | "markdown" | "pdf")
        && !epub_alt
        && !docx_alt
    {
        Some(format!(
            ".{ext} 无固定页概念，已按「原文全文 + 译文附录」输出"
        ))
    } else {
        None
    };
    if let Some(s) = session {
        // 流式回写会话收尾：回放 spill → 分发到对应 writeback_* 打包（与全量
        // API 同一入口，语义零分叉）。成败路径都删 spill；DocStats 口径不随
        // 会话变化（units 仍 = 翻译单元总数，UI/CLI 零感知）
        s.finish()
            .map_err(|e| format!("回写会话失败：{e}"))?;
    } else {
        // 全量路径：此刻才攒 translated 全集（session 路径不构造——这就是
        // 内存收益所在）。只剩 PageAlt 交替/退化与 .md/.pdf 输出会用它
        //（docforge@35a1d99e 起全量 API 吃 TranslationPair，二元组 From 即
        // 不校验源的信任锚点形态）
        let translated: Vec<docforge::emit::TranslationPair> = units
            .iter()
            .zip(texts_out)
            .map(|(u, t)| (u.id.clone(), t).into())
            .collect();
        // 输出分派（见函数文档）。扩展名小写比较，无扩展名按不支持处理。
        // docforge@35a1d99e 起回写器返回 WritebackReport（written/skipped
        // 记因）——docmt 的 DocStats 口径不消费它，各臂收尾 `?;` 丢弃
        match ext.as_str() {
            "docx" if docx_alt => {
                docforge::writeback::writeback_docx_alternating(input, &translated, out)
                    .map_err(|e| format!("docx 节交替回写失败：{e}"))?;
            }
            "epub" if epub_alt => {
                docforge::writeback::writeback_epub_alternating(input, &translated, out)
                    .map_err(|e| format!("epub 部件交替回写失败：{e}"))?;
            }
            "md" | "markdown" => {
                let md = if opts.strategy == BiStrategy::PageAlt {
                    page_alternating_markdown(&doc, &translated)
                } else {
                    docforge::emit::to_bilingual_markdown_with(
                        &doc,
                        &translated,
                        opts.strategy.into(),
                    )
                };
                std::fs::write(out, md).map_err(|e| format!("写入 {} 失败：{e}", out.display()))?;
            }
            // 以下保格式分支只剩 PageAlt 策略会走到（默认策略已被 session
            // 分支接管；docx/epub 的交替分支在上面 guard 命中）
            "docx" => {
                docforge::writeback::writeback_docx(
                    input,
                    &translated,
                    opts.strategy.into(),
                    out,
                )
                .map_err(|e| format!("docx 回写失败：{e}"))?;
            }
            "epub" => {
                docforge::writeback::writeback_epub(
                    input,
                    &translated,
                    opts.strategy.into(),
                    out,
                )
                .map_err(|e| format!("epub 回写失败：{e}"))?;
            }
            // 纯文本/标记族回写（docforge@82cdbb46 起；txt 恒开、html 随 default、
            // fb2/odt 已在本 crate feature 集中开启）
            "txt" => {
                docforge::writeback::writeback_txt(
                    input,
                    &translated,
                    opts.strategy.into(),
                    out,
                )
                .map_err(|e| format!("txt 回写失败：{e}"))?;
            }
            "html" | "htm" => {
                docforge::writeback::writeback_html(
                    input,
                    &translated,
                    opts.strategy.into(),
                    out,
                )
                .map_err(|e| format!("html 回写失败：{e}"))?;
            }
            "fb2" => {
                docforge::writeback::writeback_fb2(
                    input,
                    &translated,
                    opts.strategy.into(),
                    out,
                )
                .map_err(|e| format!("fb2 回写失败：{e}"))?;
            }
            "odt" => {
                docforge::writeback::writeback_odt(
                    input,
                    &translated,
                    opts.strategy.into(),
                    out,
                )
                .map_err(|e| format!("odt 回写失败：{e}"))?;
            }
            // 双语对照 PDF（纯生成式，任何输入格式可出——不进 needs_same_input）
            "pdf" => {
                crate::bilingual_pdf::export_bilingual_pdf(&units, &translated, out)
                    .map_err(|e| format!("双语 PDF 导出失败：{e}"))?;
            }
            other => {
                return Err(format!(
                    "不支持的输出格式 .{other}（可选 .md / .docx / .epub / .txt / .html / .fb2 / .odt / .pdf）"
                ))
            }
        }
    }
    Ok(DocStats {
        units: total,
        chars,
        elapsed_ms: t0.elapsed().as_millis(),
        note,
        cache_hits,
        running_merged,
        glossary_hits,
    })
}

/// 页级交替 Markdown（BiStrategy::PageAlt 的 markdown 实现）：
/// 每页渲染原文页 + 译文页（译文页用 TranslationOnly 语义），页间 `---` 分隔。
///
/// 实现要点：逐页构造**单页 Document** 复用 docforge 的块渲染（heading/quote/
/// list/table 不复刻）。id 锚点不漂移的关键：`translation_id` 的页号取
/// `Page.name`（无名为页序）——切单页后页序恒 0，因此对无名页把原页序写进
/// `name`（`Some("2")` → id 仍为 `p2-bN`），有名页原样保留。译文侧传全量
/// `translated`（to_bilingual_markdown_with 按 id 精确匹配，跨页条目自然忽略）。
fn page_alternating_markdown(
    doc: &docforge::Document,
    translated: &[docforge::emit::TranslationPair],
) -> String {
    let mut pages: Vec<String> = Vec::with_capacity(doc.pages.len());
    for (idx, page) in doc.pages.iter().enumerate() {
        let mut single = doc.clone();
        single.pages = vec![page.clone()];
        // 无名页补页序为 name，保持 id 锚点与全量文档一致（见函数文档）
        if single.pages[0].name.is_none() {
            single.pages[0].name = Some(idx.to_string());
        }
        let original = docforge::emit::to_markdown(&single);
        let translated_page = docforge::emit::to_bilingual_markdown_with(
            &single,
            translated,
            docforge::emit::BiStrategy::TranslationOnly,
        );
        // 组内（原文页 ↔ 该页译文）空行分隔；组间（页 ↔ 页）用 --- 分隔
        pages.push(format!("{original}\n\n{translated_page}"));
    }
    pages.join("\n\n---\n\n")
}

/// docforge 解析（文档翻译场景装配）：文本族内置 + docx/epub/pdf（feature
/// 随本 crate 依赖声明启用）。pdf 显式 `PdfOptions{mode: Auto}`，engine 取
/// `ocr`（None 与 docforge 默认装配同款——发布版不内置引擎；Some 为扫描页
/// 识别注入，非 pdf 格式不消费）。写出以固定契约：纯文字层 PDF 正常直提；
/// 扫描页无引擎时预检报错（Auto 不产空页而是 Err，见
/// [`translate_parse_error`]），有引擎则渲染识别后进同一翻译管线。
///
/// lang-detect 装配核查结论（auto 源语言检测的前置调查，2026-11-04）：
/// - **文档级**（整篇判一语 → `DocMeta.extra["language"]`）：处理能力随
///   `plugins::text::register_defaults` 注册（docforge@e1fe673 的
///   plugins/text.rs 底部 `lang_detect::register`，feature=lang-detect 开启），
///   与 `docforge::parse()` 便捷函数的装配同源——本函数的 Registry **不缺**
///   该注册；
/// - **块级**（逐块填 `Block.lang` → `to_translation_units` 复制进
///   `unit.lang`）：不是 Registry 注册，而是 `BackendPolicy`
///   `lang_detect_per_block` 开关（docforge 引擎组装后置遍历
///   `detect_block_langs`）；连 `docforge::parse()` 默认装配都没开
///   （BackendPolicy::default 恒 false）。因此本函数按 `detect_lang` 显式
///   开启——只有 auto 源语言才传 true（固定 from 不花逐块 whatlang 的
///   成本，行为与旧版逐字节一致）。
fn parse_document(
    input: &Path,
    ocr: Option<std::sync::Arc<dyn docforge::ocr::OcrEngine>>,
    detect_lang: bool,
) -> Result<docforge::Document, String> {
    let path = input
        .to_str()
        .ok_or_else(|| format!("路径非 UTF-8，无法解析：{}", input.display()))?;
    let mut registry = docforge::Registry::new();
    docforge::plugins::text::register_defaults(&mut registry);
    docforge::backends::office::register_defaults(&mut registry);
    docforge::backends::epub::register_defaults(&mut registry);
    docforge::backends::pdf::register(
        &mut registry,
        docforge::backends::pdf::PdfOptions {
            mode: docforge::backends::pdf::OcrMode::Auto,
            engine: ocr,
            ..Default::default()
        },
    );
    let policy = docforge::policy::BackendPolicy {
        lang_detect_per_block: detect_lang,
        ..Default::default()
    };
    docforge::DocumentEngine::new(registry)
        .with_policy(policy)
        .parse(path)
        .map_err(translate_parse_error)
}

/// docforge 解析错误 → 面向用户的中文文案。扫描页无引擎（ocr_engine=None
/// 的默认装配）时 pdf 后端预检（先于任何页产出）报 "第 N 页需 OCR 引擎：…"，
/// 转译为行动指引；注入引擎后该文案不再出现（扫描页正常识别进翻译）。
fn translate_parse_error(e: docforge::DocError) -> String {
    if e.message.contains("需 OCR 引擎") {
        "含扫描页且未注入 OCR 引擎（v1 暂不支持扫描 PDF 翻译，请转图片批量 OCR）".to_string()
    } else {
        e.to_string()
    }
}

/// 缓存路径覆盖：环境变量 RUMI_TRANSLATE_CACHE。运行时读取（测试里不引入
/// set_var 的 unsafe），未设置/空串返回 None 走 [`DocCache::default_path`]。
/// 主要供手动验证缓存命中用（集成用例标 #[ignore]，纯逻辑层已覆盖足够）。
fn cache_path_override() -> Option<std::path::PathBuf> {
    match std::env::var("RUMI_TRANSLATE_CACHE") {
        Ok(p) if !p.is_empty() => Some(std::path::PathBuf::from(p)),
        _ => None,
    }
}

#[cfg(test)]
mod page_alt_tests {
    use super::page_alternating_markdown;
    use docforge::model::{Block, BlockKind, Document, Page};

    fn para(text: &str) -> Block {
        Block {
            content: text.to_string(),
            kind: BlockKind::Paragraph,
            ..Default::default()
        }
    }

    /// 手搓两页（无名页，验证 name 补页序的 id 锚点稳定性）：
    /// 输出应为 页1原文 → 页1译文 → --- → 页2原文 → 页2译文。
    #[test]
    fn alternates_pages_with_separator() {
        let doc = Document {
            pages: vec![
                Page { blocks: vec![para("Alpha first page paragraph.")], ..Default::default() },
                Page { blocks: vec![para("Beta second page paragraph.")], ..Default::default() },
            ],
            ..Default::default()
        };
        // id 锚点：无名页 → 页序（p0-b0 / p1-b0）；译文前缀 T- 便于断言归属
        let translated = vec![
            ("p0-b0".to_string(), "T-alpha 第一页译文".to_string()).into(),
            ("p1-b0".to_string(), "T-beta 第二页译文".to_string()).into(),
        ];
        let md = page_alternating_markdown(&doc, &translated);
        let p1o = md.find("Alpha first page").expect("页1原文");
        let p1t = md.find("T-alpha").expect("页1译文");
        let sep = md.find("\n---\n").expect("页分隔线");
        let p2o = md.find("Beta second page").expect("页2原文");
        let p2t = md.find("T-beta").expect("页2译文");
        assert!(p1o < p1t && p1t < sep && sep < p2o && p2o < p2t,
            "应为 页1原文→页1译文→---→页2原文→页2译文：\n{md}");
    }

    /// 有名页（epub 部件名）name 保留原样，id 不漂移。
    #[test]
    fn named_page_keeps_anchor() {
        let doc = Document {
            pages: vec![Page {
                name: Some("ch3.xhtml".to_string()),
                blocks: vec![para("Named page body text.")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let translated = vec![("pch3.xhtml-b0".to_string(), "T-有名页译文".to_string()).into()];
        let md = page_alternating_markdown(&doc, &translated);
        assert!(md.contains("T-有名页译文"), "有名页锚点应匹配：\n{md}");
        assert!(md.contains("Named page body text."));
    }
}
