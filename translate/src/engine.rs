//! 离线引擎（工人 C 按 `docs/translate-spec-engine.md` §1 实现）。
//!
//! 设计要点（选型依据见 `docs/translate-design.md`）：
//! - 每个方向一个 [`ct2rs::Translator`]，缓存于 `Mutex<HashMap<key, Arc<Loaded>>>`；
//!   闲置卸载由调用方 tick 驱动（[`OfflineEngine::unload_idle`]），本模块不做定时器。
//! - 目录解析：registry 内置模型（`models_dir/<key>`）优先，未命中则扫描导入的
//!   Argos 包（读各子目录 manifest.json 的 from/to 匹配方向）；再未命中时若
//!   NLLB 多语言模型（nllb_600m）完整且目标语言在 Flores-200 码表内，由它
//!   兜底承接任意方向（专用方向模型质量优于多语言蒸馏模型，故排最后）。
//! - ct2rs 0.9 的 `Config` 不暴露 inter_threads：副本数由 `device_indices` 长度决定
//!   （`vec![0]` 即单副本，等价规格的 `inter_threads: 1`）；`num_threads_per_replica`
//!   即 intra_threads（0 = CT2 自行决定）。
//! - CT2 对空序列会报错，翻译前必须滤掉空白段；行结构按 `segment::segment_lines`
//!   严格还原（行数与输入一致）。
//! - translate_texts 把多条文本的非空段合成一次 CT2 批（大文档攒批提速，
//!   CT2 吞吐在 batch）；单条 translate 即长度为 1 的批，同一内核。

use crate::{registry, Compute};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// NLLB 多语言模型的注册表 key（目录名 / 缓存 map key 共用）。
const NLLB_KEY: &str = "nllb_600m";

/// 内部两位码 → NLLB 语言码（Flores-200）。None = NLLB 不支持该目标语言。
/// pub 供 UI 提示“NLLB 不支持该语言”；源语言不限（NLLB 源侧由分词器自辨）。
pub fn nllb_code(lang: &str) -> Option<&'static str> {
    let code = match lang {
        // 常用 14（与 lang::UI_LANGS 对齐；码已逐一对照 NLLB 词表验证，2026-11-03）
        "zh" => "zho_Hans",
        "zht" => "zho_Hant",
        "en" => "eng_Latn",
        "ja" => "jpn_Jpan",
        "ko" => "kor_Hang",
        "fr" => "fra_Latn",
        "de" => "deu_Latn",
        "es" => "spa_Latn",
        "ru" => "rus_Cyrl",
        "pt" => "por_Latn",
        "it" => "ita_Latn",
        "vi" => "vie_Latn",
        "ar" => "arb_Arab",
        "th" => "tha_Thai",
        // 更多语言（与 lang::UI_LANGS_MORE 对齐，逐码经词表 grep 验证）
        "hi" => "hin_Deva",
        "id" => "ind_Latn",
        "ms" => "zsm_Latn",
        "tr" => "tur_Latn",
        "nl" => "nld_Latn",
        "pl" => "pol_Latn",
        "uk" => "ukr_Cyrl",
        "tl" => "tgl_Latn",
        "my" => "mya_Mymr",
        "km" => "khm_Khmr",
        "lo" => "lao_Laoo",
        "mn" => "khk_Cyrl",
        "fa" => "pes_Arab",
        "he" => "heb_Hebr",
        "el" => "ell_Grek",
        "hu" => "hun_Latn",
        "cs" => "ces_Latn",
        "sv" => "swe_Latn",
        "nb" => "nob_Latn",
        "fi" => "fin_Latn",
        "da" => "dan_Latn",
        "ro" => "ron_Latn",
        "bg" => "bul_Cyrl",
        "sr" => "srp_Cyrl",
        "hr" => "hrv_Latn",
        "sk" => "slk_Latn",
        "sl" => "slv_Latn",
        "lt" => "lit_Latn",
        "lv" => "lvs_Latn",
        "et" => "est_Latn",
        "bn" => "ben_Beng",
        "ur" => "urd_Arab",
        "ta" => "tam_Taml",
        "te" => "tel_Telu",
        "ml" => "mal_Mlym",
        "kn" => "kan_Knda",
        "pa" => "pan_Guru",
        "gu" => "guj_Gujr",
        "mr" => "mar_Deva",
        "ne" => "npi_Deva",
        "si" => "sin_Sinh",
        "sw" => "swh_Latn",
        "ha" => "hau_Latn",
        "yo" => "yor_Latn",
        "am" => "amh_Ethi",
        "so" => "som_Latn",
        "kk" => "kaz_Cyrl",
        "uz" => "uzn_Latn",
        "ug" => "uig_Arab",
        "bo" => "bod_Tibt",
        _ => return None,
    };
    Some(code)
}

#[derive(Debug, Clone)]
pub enum EngineError {
    ModelNotInstalled { from: String, to: String },
    NoBuiltinModel { from: String, to: String },
    Load(String),
    Infer(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::ModelNotInstalled { from, to } => {
                write!(f, "语言包未安装：{from}→{to}，请先在翻译页下载")
            }
            EngineError::NoBuiltinModel { from, to } => {
                write!(f, "{from}→{to} 暂无内置模型（可从翻译页导入 Argos 语言包）")
            }
            EngineError::Load(e) => write!(f, "模型加载失败：{e}"),
            EngineError::Infer(e) => write!(f, "推理失败：{e}"),
        }
    }
}

impl std::error::Error for EngineError {}

#[derive(Debug, Clone)]
pub struct InstalledInfo {
    pub key: String,
    pub from: String,
    pub to: String,
    pub bytes: u64,
}

/// 已载入的翻译器实例。调用方持有 [`Arc`] 期间实例不会被拆毁——
/// unload_idle / unload_key 只从 map 删引用，正在翻译中的调用 drop 后自然释放。
/// last_used 用内部可变性：条目经 Arc 共享，刷新时间戳不能外改整个 Arc。
/// nllb：NLLB 家族模型（manifest kind=="nllb" 或 key=="nllb_600m"）——
/// 推理需每段带 Flores-200 目标码前缀，与 Marian 的批翻译调用不同。
/// 分词器双轨：默认纯 Rust [`crate::tokenizer::RumiSpmTokenizer`]（对拍全等、
/// 零 protobuf-lite）；spm-sys 回退 ct2rs auto（其 sentencepiece 探测需
/// `spm-sys` feature 放行，见 Cargo.toml）。
struct Loaded {
    #[cfg(not(feature = "spm-sys"))]
    translator: ct2rs::Translator<crate::tokenizer::RumiSpmTokenizer>,
    #[cfg(feature = "spm-sys")]
    translator: ct2rs::Translator<ct2rs::tokenizers::auto::Tokenizer>,
    loaded_at: Instant,
    last_used: Mutex<Instant>,
    nllb: bool,
}

pub struct OfflineEngine {
    models_dir: PathBuf,
    compute: Compute,
    inner: Mutex<HashMap<String, Arc<Loaded>>>,
}

impl OfflineEngine {
    pub fn new(models_dir: PathBuf, compute: Compute) -> OfflineEngine {
        OfflineEngine {
            models_dir,
            compute,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 翻译一段文本。from == to 直接原样返回；不加载模型时返回
    /// [`EngineError::ModelNotInstalled`] / [`EngineError::NoBuiltinModel`]。
    /// 内部 = 长度为 1 的批量（[`OfflineEngine::translate_texts`] 同一内核：
    /// 分句 → CT2 批 → 行切回 → strip），单条行为与旧实现逐字一致。
    pub fn translate(&self, text: &str, from: &str, to: &str) -> Result<String, EngineError> {
        if from == to {
            return Ok(text.to_string());
        }
        let mut out = self.translate_texts(&[text.to_string()], from, to)?;
        Ok(out.pop().unwrap_or_default())
    }

    /// 批量翻译多条文本（大文档攒批入口）：每条独立走 segment_lines 分句，
    /// 全部非空段合成一个大 CT2 批（translate_batch / NLLB 的
    /// translate_batch_with_target_prefix 同款），结果按各条的段数切回，
    /// 每条独立行重排 + strip_sp_marker。单条内部行为与 translate() 完全
    /// 一致（同一模型只加载一次）。
    pub fn translate_texts(
        &self,
        texts: &[String],
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, EngineError> {
        if from == to {
            return Ok(texts.to_vec());
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // 每条独立分句（外层=文本，中层=行，内层=段；空白行以空段占位保留
        // 行结构），全部非空段合成一次 CT2 批——CT2 的吞吐单位是 batch，
        // 逐条 batch(1) 是大文档翻译的主要吞吐浪费点
        let structures: Vec<Vec<Vec<String>>> = texts
            .iter()
            .map(|t| crate::segment::segment_lines(t))
            .collect();
        let refs: Vec<&[Vec<String>]> = structures.iter().map(|s| s.as_slice()).collect();
        self.translate_segments(&refs, texts, from, to)
    }

    /// 批量内核（translate / translate_texts 共用）：收集全部非空段 → 一次
    /// CT2 批（NLLB 家族带同款 Flores-200 target prefix）→ 按行结构切回重排
    /// （[`split_batch_results`] 纯函数）。from == to 与空输入在调用方
    /// （translate / translate_texts）已短路；零内容文本原样直通见纯函数。
    fn translate_segments(
        &self,
        structures: &[&[Vec<String>]],
        originals: &[String],
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, EngineError> {
        let loaded = self.load(from, to)?;
        // 收集全部非空段（CT2 对空序列报错；空白段不入批，切回时空白原样保留）
        let mut sources: Vec<String> = Vec::new();
        for structure in structures {
            for line in structure.iter() {
                for seg in line {
                    if !seg.trim().is_empty() {
                        sources.push(seg.clone());
                    }
                }
            }
        }
        if sources.is_empty() {
            // 整批空白：没有可翻译内容，逐条原样返回
            self.touch(&loaded);
            return Ok(originals.to_vec());
        }
        let results = if loaded.nllb {
            // NLLB 家族：每段带同一目标语言码（Flores-200）前缀——模型本体
            // 不定方向，全靠 target prefix 指路（zh→ja 实测走通，见模块历史）。
            let code = nllb_code(to)
                .ok_or_else(|| EngineError::Infer(format!("NLLB 不支持目标语言：{to}")))?;
            let prefixes: Vec<Vec<&str>> = vec![vec![code]; sources.len()];
            loaded
                .translator
                .translate_batch_with_target_prefix(&sources, &prefixes, &Default::default(), None)
                .map_err(|e| EngineError::Infer(e.to_string()))?
        } else {
            loaded
                .translator
                .translate_batch(&sources, &Default::default(), None)
                .map_err(|e| EngineError::Infer(e.to_string()))?
        };
        let out = split_batch_results(structures, originals, &results, to);
        self.touch(&loaded);
        Ok(out)
    }

    /// 按需加载（内部持锁）。锁只护 map：模型加载是重活，在锁外进行，
    /// 完成后二次持锁插入（双检——并发时复用先到者，丢弃重复实例）。
    fn load(&self, from: &str, to: &str) -> Result<Arc<Loaded>, EngineError> {
        let (map_key, model_dir) = match resolve_dir(&self.models_dir, from, to) {
            Resolution::RegistryFound { key, dir } => (key, dir),
            Resolution::ImportedFound { dir } => (
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir.to_string_lossy().into_owned()),
                dir,
            ),
            Resolution::NllbFound { key, dir } => (key, dir),
            Resolution::NotInstalled { from, to } => {
                // manifest 缺失/未记账（安装器没跑完，或模型目录是手动摆放/外部解包的）
                // 不拦加载：模型文件齐了就直接用——manifest 只是 is_installed/installed()
                // 的记账口径，不是加载的前置条件
                match registry::find(&from, &to) {
                    Some(spec) => {
                        let dir = self.models_dir.join(spec.key);
                        if model_files_present(&dir) {
                            (spec.key.to_string(), dir)
                        } else {
                            return Err(EngineError::ModelNotInstalled { from, to });
                        }
                    }
                    None => return Err(EngineError::ModelNotInstalled { from, to }),
                }
            }
            Resolution::NotFound { from, to } => return Err(EngineError::NoBuiltinModel { from, to }),
        };
        // NLLB 家族判定：registry nllb 条目，或手摆目录的 manifest kind=="nllb"
        let nllb = map_key == NLLB_KEY
            || read_manifest(&model_dir).map(|m| m.kind == "nllb").unwrap_or(false);
        if let Some(hit) = self.map().get(&map_key).cloned() {
            return Ok(hit);
        }
        // 字段名以 ct2rs 0.9 实际 API 为准（规格草稿的 inter/intra_threads、
        // Int8/Float32 拼写与 0.9.22 不一致，见模块注释与回报）
        let cfg = ct2rs::Config {
            device: ct2rs::Device::CPU,
            device_indices: vec![0],
            num_threads_per_replica: 0,
            compute_type: match self.compute {
                Compute::Int8 => ct2rs::ComputeType::INT8,       // fp16 权重加载时动态转 int8
                Compute::Float32 => ct2rs::ComputeType::FLOAT32, // 对照档（内存↑）
            },
            ..Default::default()
        };
        // 默认：纯 Rust 分词器随 Translator 一起构造（with_tokenizer，分词失败
        // 同样按 Load 报错）；spm-sys：Translator::new 自动探测
        // source.spm/target.spm（ct2rs auto 分词器）
        #[cfg(not(feature = "spm-sys"))]
        let translator = ct2rs::Translator::with_tokenizer(
            &model_dir,
            crate::tokenizer::RumiSpmTokenizer::open(&model_dir)
                .map_err(|e| EngineError::Load(e.to_string()))?,
            &cfg,
        )
        .map_err(|e| EngineError::Load(e.to_string()))?;
        #[cfg(feature = "spm-sys")]
        let translator =
            ct2rs::Translator::new(&model_dir, &cfg).map_err(|e| EngineError::Load(e.to_string()))?;
        let now = Instant::now();
        let loaded = Arc::new(Loaded {
            translator,
            loaded_at: now,
            last_used: Mutex::new(now),
            nllb,
        });
        let mut map = self.map();
        if let Some(hit) = map.get(&map_key) {
            return Ok(Arc::clone(hit));
        }
        map.insert(map_key, Arc::clone(&loaded));
        Ok(loaded)
    }

    /// 已安装清单：扫 models_dir 一级子目录读 manifest.json。
    /// 有 registry key 的条目 from/to/bytes 以注册表与 manifest files 为准
    /// （NLLB 条目 from/to 恒为 "*"，UI 按 kind 自行渲染多语言文案）；
    /// 导入包按 manifest 实际 from/to，bytes = 目录文件总大小。
    pub fn installed(&self) -> Vec<InstalledInfo> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.models_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(man) = read_manifest(&dir) else {
                continue;
            };
            if let Some(spec) = registry::by_key(&man.key) {
                out.push(InstalledInfo {
                    key: spec.key.to_string(),
                    from: spec.from.to_string(),
                    to: spec.to.to_string(),
                    bytes: man.files.iter().map(|f| f.bytes).sum(),
                });
            } else if !man.from.is_empty() && !man.to.is_empty() {
                // 导入包：key 不在注册表，方向取 manifest 实际字段
                out.push(InstalledInfo {
                    key: dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    from: man.from,
                    to: man.to,
                    bytes: dir_size(&dir),
                });
            }
            // key 不在注册表且 manifest 无方向（导入时元数据缺失）：无法报告用途，跳过
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// registry key 完整、导入包方向匹配，或 NLLB 兜底可承该方向。
    pub fn is_installed(&self, from: &str, to: &str) -> bool {
        matches!(
            resolve_dir(&self.models_dir, from, to),
            Resolution::RegistryFound { .. } | Resolution::ImportedFound { .. } | Resolution::NllbFound { .. }
        )
    }

    /// 卸载闲置条目：loaded_at 与 last_used 两个 elapsed 都超过 idle 才删。
    /// 正在被 Arc 使用的条目也能删（map 删引用，不拆使用中实例）。
    /// 不做定时器——由调用方驱动（UI 每分钟 tick / CLI 进程退出）。
    pub fn unload_idle(&self, idle: Duration) {
        let now = Instant::now();
        let last_used = |l: &Loaded| *l.last_used.lock().unwrap_or_else(|p| p.into_inner());
        self.map()
            .retain(|_, l| !is_idle(l.loaded_at, last_used(l), idle, now));
    }

    /// 从缓存摘除指定 key（NLLB 用 "nllb_600m"）；正在翻译中的调用持有
    /// [`Arc`] 不受影响。返回是否确实摘除（本就没载入 → false，删除目录前
    /// 先调它即可，无需判存在）。
    pub fn unload_key(&self, key: &str) -> bool {
        self.map().remove(key).is_some()
    }

    /// 取 map（锁中毒时取回内部数据继续用——持锁线程 panic 不该永久废掉引擎）。
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Loaded>>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 翻译成功后刷新 last_used（条目刚被 unload_idle 删掉则忽略）。
    fn touch(&self, loaded: &Arc<Loaded>) {
        let mut map = self.map();
        for entry in map.values_mut() {
            if Arc::ptr_eq(entry, loaded) {
                *entry.last_used.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                break;
            }
        }
    }
}

/// shared() 的 compute 覆盖点：在首次 shared() 之前调用 [`configure_shared`]
/// 可改变单例精度（幂等，只记第一次；之后调用无效，避免两个精度并存）。
static SHARED_COMPUTE: OnceLock<Compute> = OnceLock::new();

/// 指定 shared() 单例的计算精度；必须在第一次 shared() 之前调用才生效，
/// 默认 [`Compute::Int8`]。UI 启动时按设置页选择调用。
pub fn configure_shared(compute: Compute) {
    let _ = SHARED_COMPUTE.set(compute);
}

/// 进程级单例：首次调用按 [`crate::models_dir`] + 配置精度（默认 Int8）构造。
pub fn shared() -> &'static OfflineEngine {
    static S: OnceLock<OfflineEngine> = OnceLock::new();
    S.get_or_init(|| {
        let compute = *SHARED_COMPUTE.get_or_init(|| Compute::Int8);
        OfflineEngine::new(crate::models_dir(), compute)
    })
}

/// `(from,to)` → 模型目录解析结果（纯逻辑，单测直测）。
#[derive(Debug, Clone, PartialEq)]
enum Resolution {
    /// registry 命中且安装完整（key 即 `models_dir` 下的目录名）
    RegistryFound { key: String, dir: PathBuf },
    /// registry 未命中，但 models_dir 下有导入包方向匹配
    ImportedFound { dir: PathBuf },
    /// registry 精确方向与导入包都未命中，NLLB 多语言模型（nllb_600m）
    /// 完整且目标语言在 Flores-200 码表内 → 兜底承接任意方向
    NllbFound { key: String, dir: PathBuf },
    /// registry 命中但目录不完整（未装完/装坏了）
    NotInstalled { from: String, to: String },
    /// registry 未命中且没有导入包匹配
    NotFound { from: String, to: String },
}

/// 生产入口：查 registry 后走 [`resolve_with`]（该函数可注入，测试不依赖真实注册表）。
/// 精确方向（内置+导入包）优先，都未命中才落 NLLB 兜底——zh_en/en_zh 装好了
/// 不该被 NLLB 抢走（专用模型质量优于多语言蒸馏模型）。
fn resolve_dir(models_dir: &Path, from: &str, to: &str) -> Resolution {
    let base = match registry::find(from, to) {
        Some(spec) => {
            let dir = models_dir.join(spec.key);
            // download::is_complete 为权威判定（注意：它接收模型根目录、内部自行
            // join(key)，此处传 models_dir 而非已拼好的 dir，避免双重 join）；
            // manifest 自检兜底——两份实现读同一份 manifest.json + 文件字节数
            let complete = crate::download::is_complete(spec, models_dir)
                || manifest_complete(&dir);
            resolve_with(Some(spec.key), complete, models_dir, from, to)
        }
        None => resolve_with(None, false, models_dir, from, to),
    };
    // NLLB 兜底覆盖两类：NotFound（无专用条目）与 NotInstalled（有专用条目但
    // 未装）——用户装了「中文↔200 语言」模型后，任何支持的方向都应能翻；
    // 专用未装不该挡路（专用已装则上面已 RegistryFound 命中，不会走到这里，
    // 专用优先语义不变）。
    if matches!(
        base,
        Resolution::NotFound { .. } | Resolution::NotInstalled { .. }
    ) && nllb_code(to).is_some()
    {
        if let Some(spec) = registry::by_key(NLLB_KEY) {
            let dir = models_dir.join(spec.key);
            let complete =
                crate::download::is_complete(spec, models_dir) || manifest_complete(&dir);
            if complete {
                return Resolution::NllbFound {
                    key: spec.key.to_string(),
                    dir,
                };
            }
        }
    }
    base
}

/// resolve_dir 的可注入内核：`registry_key` 传 Some 即「registry 命中」分支。
/// 抽出来是为了单测用假 key 覆盖四种 Resolution，不耦合 ModelSpec 的字段形状
///（该结构仍在并行迭代，测试不应随其字段增删而破坏）。
fn resolve_with(
    registry_key: Option<&str>,
    complete: bool,
    models_dir: &Path,
    from: &str,
    to: &str,
) -> Resolution {
    match registry_key {
        Some(key) => {
            let dir = models_dir.join(key);
            if complete {
                Resolution::RegistryFound {
                    key: key.to_string(),
                    dir,
                }
            } else {
                Resolution::NotInstalled {
                    from: from.to_string(),
                    to: to.to_string(),
                }
            }
        }
        None => match scan_imported(models_dir, from, to) {
            Some(dir) => Resolution::ImportedFound { dir },
            None => Resolution::NotFound {
                from: from.to_string(),
                to: to.to_string(),
            },
        },
    }
}

/// 扫 models_dir 一级子目录，找 manifest.json 的 from/to 与方向一致的导入包。
fn scan_imported(models_dir: &Path, from: &str, to: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(models_dir).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(man) = read_manifest(&dir) else {
            continue;
        };
        if man.from == from && man.to == to {
            return Some(dir);
        }
    }
    None
}

#[derive(Debug, serde::Deserialize)]
struct ManifestFile {
    name: String,
    bytes: u64,
}

/// manifest.json（安装器/导入器统一格式；字段都可能缺，全给默认值——
/// 导入包元数据读不到时 from/to 为空串，永不匹配任何真实方向）。
/// kind：模型家族（marian/nllb），NLLB 手摆目录据此识别。
#[derive(Debug, Default, serde::Deserialize)]
struct Manifest {
    #[serde(default)]
    key: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    files: Vec<ManifestFile>,
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 目录完整性自检：manifest.json 可解析且 files 里每个文件字节数相符。
/// 与 download::is_complete 同语义（那份以 spec 声明值为准，这份以 manifest 为准）。
fn manifest_complete(dir: &Path) -> bool {
    let Some(man) = read_manifest(dir) else {
        return false;
    };
    man.files.iter().all(|f| {
        std::fs::metadata(dir.join(&f.name))
            .map(|m| m.len() == f.bytes)
            .unwrap_or(false)
    })
}

/// 统一布局下的最小可加载集：model.bin + source.spm + target.spm
/// （CT2 权重 + auto 分词器探测的两个 spm；vocabulary 由 CT2 按模型自省）。
fn model_files_present(dir: &Path) -> bool {
    ["model.bin", "source.spm", "target.spm"].iter().all(|f| {
        std::fs::metadata(dir.join(f))
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    })
}

/// 目录文件总大小（导入包 bytes 口径）。符号链接不跟随，避免成环。
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if ft.is_dir() {
            total += dir_size(&path);
        } else if ft.is_file() {
            if let Ok(m) = std::fs::metadata(&path) {
                total += m.len();
            }
        }
    }
    total
}

/// 闲置判定纯函数（unload_idle 的 retain 谓词；抽出来便于单测）。
fn is_idle(loaded_at: Instant, last_used: Instant, idle: Duration, now: Instant) -> bool {
    now.duration_since(loaded_at) > idle && now.duration_since(last_used) > idle
}

/// 批结果按各文本的行结构切分重排（纯逻辑，无 CT2/模型耦合，单测直测）：
/// `structures[i]` 是第 i 条文本的 segment_lines 行结构（外层=行，内层=段，
/// 空白行/空白段以空串占位）；`originals` 与外层对齐，供零内容文本原样
/// 直通；`results` 是整批 CT2 译文（与各文本全部非空段按序一一对应）。
/// 返回与外层一一对应的成品文本：非空段按序消费批结果（不足段数以空串
/// 兜底——CT2 正常不缺项，与旧单条路径 unwrap_or_default 同语义）、空白段
/// 还原空串、行内段以 seg_sep join（to=="zh" 无分隔符——中文无空格；拉丁
/// 语系用单空格）、行间 \n join、行数与输入严格一致；出口统一清理
/// SentencePiece ▁(U+2581) 标记（ct2rs 反解不处理句首等位置，任何方向译文
/// 都可能带出；直通文本未过模型，原样返回不 strip——空白原文的首尾空白
/// 不该被行结构 join 吃掉，与旧单条路径一致）。
fn split_batch_results(
    structures: &[&[Vec<String>]],
    originals: &[String],
    results: &[(String, Option<f32>)],
    to: &str,
) -> Vec<String> {
    let seg_sep = if to == "zh" { "" } else { " " };
    let mut next = results.iter();
    let mut out = Vec::with_capacity(structures.len());
    for (structure, original) in structures.iter().zip(originals) {
        // 零内容文本（全段空白）：原样直通，不消费批结果
        if !structure.iter().flatten().any(|seg| !seg.trim().is_empty()) {
            out.push(original.clone());
            continue;
        }
        let mut out_lines: Vec<String> = Vec::with_capacity(structure.len());
        for line in structure.iter() {
            let mut parts: Vec<String> = Vec::with_capacity(line.len());
            for seg in line {
                if seg.trim().is_empty() {
                    parts.push(String::new());
                } else {
                    parts.push(next.next().map(|(t, _)| t.clone()).unwrap_or_default());
                }
            }
            out_lines.push(parts.join(seg_sep));
        }
        out.push(crate::strip_sp_marker(&out_lines.join("\n")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_with_registry_found() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_with(Some("xx_yy"), true, dir.path(), "xx", "yy");
        assert_eq!(
            res,
            Resolution::RegistryFound {
                key: "xx_yy".to_string(),
                dir: dir.path().join("xx_yy"),
            }
        );
    }

    #[test]
    fn resolve_with_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_with(Some("xx_yy"), false, dir.path(), "xx", "yy");
        assert_eq!(
            res,
            Resolution::NotInstalled {
                from: "xx".to_string(),
                to: "yy".to_string(),
            }
        );
    }

    #[test]
    fn resolve_with_imported_found() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("argos-en_zh");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("manifest.json"),
            r#"{"key":"","from":"en","to":"zh","source":"argos","files":[]}"#,
        )
        .unwrap();
        let res = resolve_with(None, false, dir.path(), "en", "zh");
        assert_eq!(res, Resolution::ImportedFound { dir: pkg });
    }

    #[test]
    fn resolve_with_not_found_and_direction_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        // 方向不匹配的导入包不算命中
        let pkg = dir.path().join("argos-ja_zh");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("manifest.json"), r#"{"from":"ja","to":"zh"}"#).unwrap();
        assert_eq!(
            resolve_with(None, false, dir.path(), "en", "zh"),
            Resolution::NotFound {
                from: "en".to_string(),
                to: "zh".to_string(),
            }
        );
        // 空目录也是 NotFound
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_with(None, false, empty.path(), "en", "zh"),
            Resolution::NotFound {
                from: "en".to_string(),
                to: "zh".to_string(),
            }
        );
    }

    #[test]
    fn manifest_complete_two_states() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("en_zh");
        std::fs::create_dir_all(&model).unwrap();
        // 没有 manifest：不完整
        assert!(!manifest_complete(&model));
        // manifest 列了 4 字节 model.bin，实际 4 字节：完整
        std::fs::write(
            model.join("manifest.json"),
            r#"{"key":"en_zh","from":"en","to":"zh","files":[{"name":"model.bin","bytes":4}]}"#,
        )
        .unwrap();
        std::fs::write(model.join("model.bin"), b"abcd").unwrap();
        assert!(manifest_complete(&model));
        // 文件字节数不符：不完整
        std::fs::write(model.join("model.bin"), b"abc").unwrap();
        assert!(!manifest_complete(&model));
        // 文件缺失：不完整
        std::fs::remove_file(model.join("model.bin")).unwrap();
        assert!(!manifest_complete(&model));
        // manifest 坏 JSON：不完整
        std::fs::write(model.join("manifest.json"), "not json").unwrap();
        assert!(!manifest_complete(&model));
    }

    #[test]
    fn model_files_present_minimum_set() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("en_zh");
        // 什么都没有：不可加载
        assert!(!model_files_present(&model));
        std::fs::create_dir_all(&model).unwrap();
        // 三个模型文件齐了（哪怕没有 manifest）：可加载
        std::fs::write(model.join("model.bin"), b"M").unwrap();
        std::fs::write(model.join("source.spm"), b"S").unwrap();
        std::fs::write(model.join("target.spm"), b"T").unwrap();
        assert!(model_files_present(&model));
        // 缺 target.spm：不可加载
        std::fs::remove_file(model.join("target.spm")).unwrap();
        assert!(!model_files_present(&model));
    }

    #[test]
    fn is_idle_predicate() {
        let now = Instant::now();
        let hour_ago = now.checked_sub(Duration::from_secs(3600)).unwrap();
        let min_ago = now.checked_sub(Duration::from_secs(60)).unwrap();
        let idle = Duration::from_secs(600);
        // 加载后一直没用：超时 → 卸
        assert!(is_idle(hour_ago, hour_ago, idle, now));
        // 最近用过：不卸
        assert!(!is_idle(hour_ago, min_ago, idle, now));
        // 刚加载：不卸
        assert!(!is_idle(min_ago, min_ago, idle, now));
        // idle = 0：任何已耗时的条目都卸（两个 elapsed 均须严格 > 0）
        assert!(is_idle(hour_ago, hour_ago, Duration::ZERO, now));
    }

    #[test]
    fn nllb_code_core_table() {
        assert_eq!(nllb_code("zh"), Some("zho_Hans"));
        assert_eq!(nllb_code("en"), Some("eng_Latn"));
        assert_eq!(nllb_code("ja"), Some("jpn_Jpan"));
        assert_eq!(nllb_code("ko"), Some("kor_Hang"));
        assert_eq!(nllb_code("ar"), Some("arb_Arab"));
        // 码表外：None（引擎报“NLLB 不支持目标语言”）
        assert_eq!(nllb_code("eo"), None);
        assert_eq!(nllb_code(""), None);
    }

    #[test]
    fn unload_key_missing_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let eng = OfflineEngine::new(dir.path().to_path_buf(), Compute::Int8);
        // 没载入过的 key：摘除返回 false（幂等口径，UI 先 unload 再 remove 不用预判）
        assert!(!eng.unload_key("nllb_600m"));
    }

    #[test]
    fn resolve_dir_nllb_fallback() {
        let dir = tempfile::tempdir().unwrap();
        // nllb_600m 未装：zh→ja 不吃兜底
        assert_eq!(
            resolve_dir(dir.path(), "zh", "ja"),
            Resolution::NotFound {
                from: "zh".to_string(),
                to: "ja".to_string(),
            }
        );
        // 摆一套 manifest 记账的 nllb_600m（文件字节数与 manifest 相符）
        let nllb = dir.path().join("nllb_600m");
        std::fs::create_dir_all(&nllb).unwrap();
        std::fs::write(
            nllb.join("manifest.json"),
            r#"{"key":"nllb_600m","from":"*","to":"*","kind":"nllb","files":[
                {"name":"model.bin","bytes":4},
                {"name":"source.spm","bytes":2},
                {"name":"target.spm","bytes":2}]}"#,
        )
        .unwrap();
        std::fs::write(nllb.join("model.bin"), b"abcd").unwrap();
        std::fs::write(nllb.join("source.spm"), b"SS").unwrap();
        std::fs::write(nllb.join("target.spm"), b"TT").unwrap();
        // zh→ja：registry 精确方向与导入包都未命中 → NLLB 兜底
        assert_eq!(
            resolve_dir(dir.path(), "zh", "ja"),
            Resolution::NllbFound {
                key: "nllb_600m".to_string(),
                dir: nllb.clone(),
            }
        );
        // NLLB 码表外的目标语言不吃兜底（eo 无 Flores-200 码）
        assert_eq!(
            resolve_dir(dir.path(), "en", "eo"),
            Resolution::NotFound {
                from: "en".to_string(),
                to: "eo".to_string(),
            }
        );
        // 专用未装也吃 NLLB 兜底（用户装了多语言模型就应全方向可翻，2026-11-03
        // 监工改）：en→zh 有注册表条目但目录未装 → NllbFound 而非 NotInstalled
        assert!(matches!(
            resolve_dir(dir.path(), "en", "zh"),
            Resolution::NllbFound { .. }
        ));
        // 专用已装则优先于兜底：摆齐 en_zh 的 manifest 记账 → RegistryFound
        let enzh = dir.path().join("en_zh");
        std::fs::create_dir_all(&enzh).unwrap();
        std::fs::write(
            enzh.join("manifest.json"),
            r#"{"key":"en_zh","from":"en","to":"zh","kind":"marian","files":[
                {"name":"model.bin","bytes":4},
                {"name":"source.spm","bytes":2},
                {"name":"target.spm","bytes":2}]}"#,
        )
        .unwrap();
        std::fs::write(enzh.join("model.bin"), b"abcd").unwrap();
        std::fs::write(enzh.join("source.spm"), b"SS").unwrap();
        std::fs::write(enzh.join("target.spm"), b"TT").unwrap();
        assert!(matches!(
            resolve_dir(dir.path(), "en", "zh"),
            Resolution::RegistryFound { .. }
        ));
    }

    #[test]
    fn translate_same_lang_passthrough() {
        // from == to 不加载模型，空 models_dir 也能通
        let dir = tempfile::tempdir().unwrap();
        let eng = OfflineEngine::new(dir.path().to_path_buf(), Compute::Int8);
        assert_eq!(eng.translate("hello world", "en", "en").unwrap(), "hello world");
        assert_eq!(eng.translate("", "zh", "zh").unwrap(), "");
    }

    // ---------- translate_texts 批量内核：切分纯函数（均匀/不均/空文本混入） ----------

    /// 单行构造器：段列表 → 行结构的一行。
    fn line(segs: &[&str]) -> Vec<String> {
        segs.iter().map(|s| s.to_string()).collect()
    }

    fn batch_result(text: &str) -> (String, Option<f32>) {
        (text.to_string(), None)
    }

    /// 均匀：各条段数一致，批结果按序等分切回，一条不多拿。
    #[test]
    fn split_batch_results_uniform() {
        let t0 = vec![line(&["A."])];
        let t1 = vec![line(&["B."])];
        let t2 = vec![line(&["C."])];
        let structures = vec![&t0[..], &t1[..], &t2[..]];
        let originals = ["A.".to_string(), "B.".to_string(), "C.".to_string()];
        let results = [batch_result("a"), batch_result("b"), batch_result("c")];
        assert_eq!(
            split_batch_results(&structures, &originals, &results, "en"),
            ["a", "b", "c"]
        );
    }

    /// 不均：各条段数不同（2/1），批结果按各条非空段数顺序切分，行内段
    /// 以单空格 join（非 zh 目标）。
    #[test]
    fn split_batch_results_uneven() {
        let t0 = vec![line(&["First.", "Second."])];
        let t1 = vec![line(&["Third."])];
        let structures = vec![&t0[..], &t1[..]];
        let originals = ["First. Second.".to_string(), "Third.".to_string()];
        let results = [batch_result("x"), batch_result("y"), batch_result("z")];
        assert_eq!(
            split_batch_results(&structures, &originals, &results, "en"),
            ["x y", "z"]
        );
    }

    /// 空文本混入：零内容文本原样直通（原文逐字保真，含空白），不消费
    /// 批结果；全空批不吃任何批结果。
    #[test]
    fn split_batch_results_empty_and_blank_mixed() {
        // 空串 + 空白原文 + 正常文本：前两条直通，第三条吃唯一批结果
        let s0 = vec![line(&[""])];
        let s1 = vec![line(&["   "])];
        let s2 = vec![line(&["D."])];
        let structures = vec![&s0[..], &s1[..], &s2[..]];
        let originals = ["".to_string(), "   ".to_string(), "D.".to_string()];
        let results = [batch_result("d")];
        assert_eq!(
            split_batch_results(&structures, &originals, &results, "en"),
            ["", "   ", "d"]
        );
        // 全空批：results 为空，全部原样直通
        let structures2 = vec![&s0[..], &s1[..]];
        let originals2 = ["".to_string(), "   ".to_string()];
        assert_eq!(split_batch_results(&structures2, &originals2, &[], "en"), ["", "   "]);
    }

    /// zh 目标：行内段无分隔符 join + ▁ 清理；多行文本空白行还原（行数与
    /// 输入严格一致）。
    #[test]
    fn split_batch_results_zh_seg_sep_and_blank_lines() {
        // zh：两段同行无分隔符直连；结果带 ▁ → strip 还原空格
        let s0 = vec![line(&["你。", "好。"])];
        let structures = vec![&s0[..]];
        let originals = ["你。好。".to_string()];
        let results = [batch_result("▁你。"), batch_result("好。")];
        assert_eq!(
            split_batch_results(&structures, &originals, &results, "zh"),
            ["你。好。"]
        );
        // 多行：空白行还原空行，行间 \n join
        let s1 = vec![line(&["A."]), line(&[""]), line(&["B."])];
        let structures2 = vec![&s1[..]];
        let originals2 = ["A.\n\nB.".to_string()];
        let results2 = [batch_result("a"), batch_result("b")];
        assert_eq!(
            split_batch_results(&structures2, &originals2, &results2, "en"),
            ["a\n\nb"]
        );
    }

    /// translate_texts 与 translate 同款直通/错误面：from == to 逐条原样
    /// （不加载模型）；模型缺失时报错与单条路径逐字一致（委托同内核的
    /// 回归闸门）。
    #[test]
    fn translate_texts_passthrough_and_missing_model_error() {
        let dir = tempfile::tempdir().unwrap();
        let eng = OfflineEngine::new(dir.path().to_path_buf(), Compute::Int8);
        // from == to：逐条原样直通（含空串/空白）
        let texts = vec!["hello".to_string(), String::new(), "  ".to_string()];
        assert_eq!(eng.translate_texts(&texts, "en", "en").unwrap(), texts);
        // 空输入：空返回
        assert_eq!(
            eng.translate_texts(&[], "en", "zh").unwrap(),
            Vec::<String>::new()
        );
        // 模型缺失：与单条 translate 同文案（同一 load 路径）
        let e1 = eng.translate("hello", "en", "zh").unwrap_err();
        let e2 = eng
            .translate_texts(&["hello".to_string()], "en", "zh")
            .unwrap_err();
        assert_eq!(e1.to_string(), e2.to_string());
        assert!(e1.to_string().contains("语言包未安装"));
    }

    #[test]
    fn display_is_user_readable() {
        assert_eq!(
            EngineError::ModelNotInstalled {
                from: "en".into(),
                to: "zh".into(),
            }
            .to_string(),
            "语言包未安装：en→zh，请先在翻译页下载"
        );
        assert_eq!(
            EngineError::NoBuiltinModel {
                from: "ja".into(),
                to: "de".into(),
            }
            .to_string(),
            "ja→de 暂无内置模型（可从翻译页导入 Argos 语言包）"
        );
        assert_eq!(
            EngineError::Load("bad model.bin".into()).to_string(),
            "模型加载失败：bad model.bin"
        );
        assert_eq!(
            EngineError::Infer("empty sequence".into()).to_string(),
            "推理失败：empty sequence"
        );
    }
}
