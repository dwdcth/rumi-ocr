//! 文档翻译：docforge 解析 → 翻译单元 → 引擎翻译 → 双语 Markdown 回写。
//!
//! v1 链路（doc-translate-spec §2）：任意 docforge 支持格式进、双语 Markdown
//! 出；docx/epub 回写上游到位后只加出口不改链路。解析默认不注入 OCR 引擎
//! ——扫描页 PDF 由 pdf 后端预检报错，本层转译为面向用户的行动指引（见
//! [`translate_parse_error`]）；`DocTranslateOptions.ocr_engine` 注入引擎后
//! 扫描页走 OCR→翻译同一管线（类型是 docforge 的 trait 对象，本 crate 不
//! 依赖 rumi-ocr，由上层装配适配器）。

use std::path::Path;

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

/// 文档翻译选项。
pub struct DocTranslateOptions {
    pub engine: crate::Engine,
    /// 内部码；单元级不自动检测（unit.lang 留 v2）。
    pub from: String,
    pub to: String,
    pub strategy: BiStrategy,
    /// 与 facade 同义（zh 目标时润色）。
    pub polish: bool,
    /// 最小字符数（docforge 同款默认 2：滤单字符噪声）。
    pub min_chars: usize,
    /// 进度回调 (已翻译单元数, 总单元数)；调用方取消语义 v1 不做
    /// （返回 false 中止留 v2）。
    pub on_progress: Option<std::sync::Arc<dyn Fn(usize, usize) + Send + Sync>>,
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
/// use_cache=true 时预扫先查文本 hash 缓存（[`DocCache`]，键=文本+翻译参数，
/// 命中跳过引擎但进度照常推进），批内新译文写缓存并在每批边界与收尾各
/// 落盘一次——断点续翻（中断重跑已翻单元全命中）+ 同文本只翻一次
/// （页眉页脚重复文本一石二鸟）。
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
    let doc = parse_document(input, opts.ocr_engine.clone())?;
    let units = docforge::emit::to_translation_units_with(&doc, opts.min_chars);
    if units.is_empty() {
        return Err("文档无可翻译文本单元（空文件或全为非文本块）".to_string());
    }
    let total = units.len();
    // 缓存装配：use_cache 开且非 from==to 直通才加载——直通走 facade 短路
    // 零引擎成本，进缓存无意义（不读不写，也不碰用户缓存目录）。路径用
    // RUMI_TRANSLATE_CACHE 覆盖缺省数据目录（测试隔离的钩子，见下）。
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
    // ---------- 阶段一：判定链预扫（不触引擎、不回调进度） ----------
    // 逐单元依次判 防幻觉 → glossary 精确命中 → running memo → 缓存 →
    // from==to 直通，命中的当场定稿；否则记入 pending（单元下标），留待
    // 阶段二攒批送引擎。两遍结构（预扫 + 批执行）逐位复刻旧单遍逐条流的
    // 判定链顺序与回填结果——统计口径（cache_hits/glossary_hits/running_merged）
    // 与输出不变。
    let mut texts_out: Vec<String> = Vec::with_capacity(total);
    let mut pending: Vec<usize> = Vec::new();
    // running 单元的批内去重（文本 → 首个 pending 槽位）：旧逐条流靠"首现
    // 翻完记 memo"让后续页眉复用同一译文；两阶段流里 memo 要等批执行才填，
    // 预扫阶段用槽位等价复刻——同文本只进批一次，批内重复单元挂到首现
    // 槽位取译文（running_merged 口径不变）。
    let mut pending_slots: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    // 批内重复的 running 文本（单元下标, 首现槽位）——批执行完统一回填
    let mut batch_refs: Vec<(usize, usize)> = Vec::new();
    for (idx, unit) in units.iter().enumerate() {
        chars += unit.text.chars().count();
        let text = if !unit.has_translatable_text() {
            // 防幻觉过滤（docforge@b5bf9a87 谓词）：220V / -10 to 50 C 这类纯
            // 数字/单位/短代码文本送引擎只会得到幻觉（实测 "220V 电话"），
            // 直接原样回填——译文=原文，回写锚点不受影响（也不进缓存）
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
        } else if let Some(hit) = cache
            .as_ref()
            .and_then(|c| c.get(&unit.text, &opts.from, &opts.to, &engine, opts.polish))
        {
            // 文本级命中（同文本跨单元/跨文档/页眉页脚只翻一次）；进度照常推进
            // ——进度是"处理进度"不是"翻译成本"
            cache_hits += 1;
            hit
        } else if passthrough {
            // from==to 直通（旧流发生在 crate::translate/facade 内部，位于
            // 缓存之后）：预扫直接定稿——pending 全空、引擎零接触
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
        texts_out.push(text);
    }
    // ---------- 阶段二：pending 攒批执行 ----------
    // 每批 16 条（复用 UNITS_PER_BATCH——批边界同时是 save 与进度回调边界，
    // 断点续翻与节流语义不变）。离线引擎整批走 engine::shared().translate_texts
    // （一次 CT2 批）；在线引擎保持逐条 crate::translate（provider 自有批量/
    // 限流语义，不掺和离线批）。
    if !pending.is_empty() {
        // 离线批量入口不做方向归一（facade 的 normalize 在其内部做），显式
        // 传入归一码；缓存键仍用原始码，语义不变
        let from = crate::lang::normalize(&opts.from);
        let to = crate::lang::normalize(&opts.to);
        let prescanned = total - pending.len();
        for (ci, chunk) in pending.chunks(UNITS_PER_BATCH).enumerate() {
            let batch: Vec<String> = chunk.iter().map(|&i| units[i].text.clone()).collect();
            let outs = match &engine {
                crate::Engine::Offline => {
                    // 整批 Err（CT2 批调用不区分单条）——错误定位到批内首
                    // pending 单元，粗粒度可接受（与现状"任一单元失败整体
                    // Err"一致）
                    crate::engine::shared()
                        .translate_texts(&batch, &from, &to)
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
                        .map_err(|e| format!("单元 {} 翻译失败：{}", units[chunk[0]].id, e))?
                }
                _ => {
                    // 在线逐条：错误定位逐单元（与旧循环逐字一致）
                    let mut outs = Vec::with_capacity(chunk.len());
                    for &i in chunk {
                        let t = crate::translate(
                            std::slice::from_ref(&units[i].text),
                            &opts.from,
                            &opts.to,
                            &engine,
                            opts.polish,
                        )
                        .map_err(|e| format!("单元 {} 翻译失败：{}", units[i].id, e))?
                        .pop()
                        .unwrap_or_default();
                        outs.push(t);
                    }
                    outs
                }
            };
            for (slot_in_chunk, &i) in chunk.iter().enumerate() {
                texts_out[i] = outs[slot_in_chunk].clone();
                if let Some(c) = cache.as_mut() {
                    c.insert(
                        &units[i].text,
                        &opts.from,
                        &opts.to,
                        &engine,
                        opts.polish,
                        &texts_out[i],
                    );
                }
                // running 首现翻完记入 memo（后续同文本页复用；批内重复已在
                // 预扫挂槽位，不重复进批）
                if units[i].running {
                    running_memo.insert(units[i].text.clone(), texts_out[i].clone());
                }
            }
            // 断点续翻：每批边界落盘一次（崩溃最多丢一批）
            if let Some(c) = cache.as_ref() {
                c.save();
            }
            // 进度回调：每批完成一次（done 含预扫已分流单元——进度是"处理
            // 进度"不是"翻译成本"，粒度从逐条变 16 条一跳，节流语义不变）
            let done = (prescanned + (ci + 1) * UNITS_PER_BATCH).min(total);
            if let Some(cb) = &opts.on_progress {
                cb(done, total);
            }
        }
        // 批内重复 running 文本回填首现槽位译文（预扫挂起的引用）
        for (dup_idx, slot) in batch_refs.drain(..) {
            texts_out[dup_idx] = texts_out[pending[slot]].clone();
        }
    } else if let Some(cb) = &opts.on_progress {
        // 全命中（含 from==to 直通）：预扫全分流、pending 空、引擎零接触；
        // 旧逐条流收尾处也会回调一次 (total, total)
        cb(total, total);
    }
    let translated: Vec<(String, String)> = units
        .iter()
        .zip(texts_out)
        .map(|(u, t)| (u.id.clone(), t))
        .collect();
    // 收尾落盘（全命中时是同内容覆写，幂等无害；缺了这步不满一批的中断前
    // 进度会全丢）
    if let Some(c) = cache.as_ref() {
        c.save();
    }
    // 输出分派（见函数文档）。扩展名小写比较，无扩展名按不支持处理
    // （CLI/UI 的默认输出恒带 .md，走到这里的无扩展名只会是显式误用）
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    // 保原格式回写要求输入与输出同格式（writeback 重解析原文件定位改写）；
    // 跨格式需求（如 txt→docx）一律走 .md 输出。前置友好报错替代深层 ZIP 错误。
    let in_ext = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let needs_same_input = matches!(
        ext.as_str(),
        "docx" | "epub" | "txt" | "html" | "htm" | "fb2" | "odt"
    );
    if needs_same_input && in_ext != ext {
        return Err(format!(
            "输出 .{ext} 需要同格式输入（当前输入 .{in_ext}）；跨格式请输出 .md"
        ));
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
    match ext.as_str() {
        "docx" if docx_alt => docforge::writeback::writeback_docx_alternating(
            input,
            &translated,
            out,
        )
        .map_err(|e| format!("docx 节交替回写失败：{e}"))?,
        "epub" if epub_alt => docforge::writeback::writeback_epub_alternating(
            input,
            &translated,
            out,
        )
        .map_err(|e| format!("epub 部件交替回写失败：{e}"))?,
        "md" | "markdown" => {
            let md = if opts.strategy == BiStrategy::PageAlt {
                page_alternating_markdown(&doc, &translated)
            } else {
                docforge::emit::to_bilingual_markdown_with(&doc, &translated, opts.strategy.into())
            };
            std::fs::write(out, md).map_err(|e| format!("写入 {} 失败：{e}", out.display()))?;
        }
        "docx" => docforge::writeback::writeback_docx(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("docx 回写失败：{e}"))?,
        "epub" => docforge::writeback::writeback_epub(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("epub 回写失败：{e}"))?,
        // 纯文本/标记族回写（docforge@82cdbb46 起；txt 恒开、html 随 default、
        // fb2/odt 已在本 crate feature 集中开启）
        "txt" => docforge::writeback::writeback_txt(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("txt 回写失败：{e}"))?,
        "html" | "htm" => docforge::writeback::writeback_html(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("html 回写失败：{e}"))?,
        "fb2" => docforge::writeback::writeback_fb2(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("fb2 回写失败：{e}"))?,
        "odt" => docforge::writeback::writeback_odt(
            input,
            &translated,
            opts.strategy.into(),
            out,
        )
        .map_err(|e| format!("odt 回写失败：{e}"))?,
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
fn page_alternating_markdown(doc: &docforge::Document, translated: &[(String, String)]) -> String {
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
fn parse_document(
    input: &Path,
    ocr: Option<std::sync::Arc<dyn docforge::ocr::OcrEngine>>,
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
    docforge::DocumentEngine::new(registry)
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
            ("p0-b0".to_string(), "T-alpha 第一页译文".to_string()),
            ("p1-b0".to_string(), "T-beta 第二页译文".to_string()),
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
        let translated = vec![("pch3.xhtml-b0".to_string(), "T-有名页译文".to_string())];
        let md = page_alternating_markdown(&doc, &translated);
        assert!(md.contains("T-有名页译文"), "有名页锚点应匹配：\n{md}");
        assert!(md.contains("Named page body text."));
    }
}
