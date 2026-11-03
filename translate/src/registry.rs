//! 内置离线模型注册表。
//! 每个方向一个 spec，sources 按优先级排列（argos 官方包直链为主源、
//! HF opus-mt 多文件为备源）；安装后统一目录布局（见 download.rs）。
//! bytes 数值来自 argos-net / HuggingFace API 2026-11-03 实测，勿改。

/// HF 仓库里的单个文件（name + 精确字节数，下载校验与完整性判断共用）。
/// as_ 是落盘别名（下载名 name → 落盘名 as_，None=同名）：NLLB 的
/// sentencepiece.bpe.model 需要落盘成 source.spm + target.spm 两份，
/// 第二份用「同 name+bytes 条目 + as_ 别名」表示，下载一次写两份（见 download.rs）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ModelFile {
    pub name: &'static str,
    pub bytes: u64,
    pub as_: Option<&'static str>,
}

/// 模型下载源。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(tag = "kind")]
pub enum ModelSource {
    /// Argos 官方 .argosmodel 直链（单 zip 文件，下载后解包安装）。
    /// URL = mirrors.argos_base + "/" + file；bytes 为直链 content-length（2026-11-03 实测）。
    Argos { file: &'static str, bytes: u64 },
    /// HuggingFace 仓库多文件。URL = https://{host}/{repo}/resolve/main/{file}。
    Hf { repo: &'static str, files: &'static [ModelFile] },
}

/// 模型家族：决定引擎推理时的调用方式——Marian 走 translate_batch；
/// Nllb 走 translate_batch_with_target_prefix（每段带 Flores-200 目标码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ModelKind {
    Marian,
    Nllb,
}

/// 一个内置模型方向（含按优先级排列的下载源）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ModelSpec {
    pub key: &'static str,      // "zh_en" / "en_zh"
    pub from: &'static str,
    pub to: &'static str,
    pub sources: &'static [ModelSource],
    pub size_mb: u64,           // 展示用约数（按主源 zip 大小）
    pub kind: ModelKind,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        key: "zh_en",
        from: "zh",
        to: "en",
        size_mb: 71,
        kind: ModelKind::Marian,
        sources: &[
            ModelSource::Argos {
                file: "translate-zh_en-1_9.argosmodel",
                bytes: 74481402,
            },
            ModelSource::Hf {
                repo: "ooeoeo/opus-mt-zh-en-ct2-float16",
                files: &[
                    ModelFile { name: "model.bin", bytes: 155502615, as_: None },
                    ModelFile { name: "shared_vocabulary.json", bytes: 1303998, as_: None },
                    ModelFile { name: "source.spm", bytes: 804677, as_: None },
                    ModelFile { name: "target.spm", bytes: 806530, as_: None },
                ],
            },
        ],
    },
    ModelSpec {
        key: "en_zh",
        from: "en",
        to: "zh",
        size_mb: 68,
        kind: ModelKind::Marian,
        sources: &[
            ModelSource::Argos {
                file: "translate-en_zh-1_9.argosmodel",
                bytes: 70743021,
            },
            ModelSource::Hf {
                repo: "ooeoeo/opus-mt-en-zh-ct2-float16",
                files: &[
                    ModelFile { name: "model.bin", bytes: 155502615, as_: None },
                    ModelFile { name: "shared_vocabulary.json", bytes: 1303887, as_: None },
                    ModelFile { name: "source.spm", bytes: 806435, as_: None },
                    ModelFile { name: "target.spm", bytes: 804600, as_: None },
                ],
            },
        ],
    },
    // NLLB-200 多语言模型：单一模型覆盖 200 语言（中文↔其他），from/to 置 "*"
    // 不参与方向匹配——引擎在精确方向（内置/导入包）都未命中时按 key 兜底解析
    // （见 engine::resolve_dir）。ct2 int8 量化版，字节数 2026-11-03 HF 实测，勿改。
    ModelSpec {
        key: "nllb_600m",
        from: "*",
        to: "*",
        size_mb: 593,
        kind: ModelKind::Nllb,
        sources: &[ModelSource::Hf {
            repo: "JustFrederik/nllb-200-distilled-600M-ct2-int8",
            files: &[
                ModelFile { name: "model.bin", bytes: 622595991, as_: None },
                ModelFile {
                    name: "sentencepiece.bpe.model",
                    bytes: 4852054,
                    as_: Some("source.spm"),
                },
                ModelFile {
                    name: "sentencepiece.bpe.model",
                    bytes: 4852054,
                    as_: Some("target.spm"),
                },
                ModelFile { name: "shared_vocabulary.txt", bytes: 2568098, as_: None },
                ModelFile { name: "config.json", bytes: 159, as_: None },
            ],
        }],
    },
];

/// 按方向找内置模型；找不到返回 None（调用方报“暂无内置模型”）。
/// from/to 为 "*" 的条目（NLLB）不参与方向匹配，只经 by_key 走兜底。
pub fn find(from: &str, to: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.from == from && m.to == to)
}

/// 按 key 找。
pub fn by_key(key: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.key == key)
}
