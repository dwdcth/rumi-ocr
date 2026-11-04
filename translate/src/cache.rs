//! 文档翻译缓存：文本 hash 键值存储（JSON 落盘，LRU 条数上限）。
//!
//! 设计动机（规格 L / 需求 #3）：大文档中断重跑全部重来、PDF 页眉页脚同文本
//! 重复翻 N 遍都是浪费——缓存键用**文本 hash**（非单元 id），同文本跨单元/
//! 跨文档/跨页眉页脚只翻一次。键里并入翻译参数（语言对/引擎判别/润色）：
//! 配置变则 miss，宁可重翻不错用旧译文（保守正确）。

use std::collections::HashMap;
use std::path::PathBuf;

/// 条数上限（规格 §1）：插入后超过即整体清空（取舍见 [`DocCache::insert`]）。
/// pub 是为了让集成测试直接引用（插 MAX+1 条断言清空），不硬编码 5000。
pub const MAX_ENTRIES: usize = 5000;

/// 落盘格式版本：读到非当前版本一律当空缓存（旧格式不迁移，直接重翻）。
const CACHE_VERSION: u32 = 1;

/// 落盘 JSON 形态：`{"version": 1, "entries": {"<key>": "<译文>", ...}}`。
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, String>,
}

/// 缓存键：blake3/sha 不引新依赖——std DefaultHasher（SipHash）足够
/// （缓存是性能优化非安全边界，碰撞最坏一次坏译文重翻即可）。
/// key = hash64(text) 的 hex + "|" + from + "|" + to + "|" + engine_tag + "|" + polish
/// engine_tag：Engine 的判别标识（"offline" / "google" / "deepl:<keyhash8>" /
/// "deeplx:<urlhash8>" / "openai:<base><model><keyhash8>"）——配置变则 miss，
/// 保守正确。
pub struct DocCache {
    entries: HashMap<String, String>, // key -> 译文
    path: PathBuf,
}

impl DocCache {
    /// 缓存文件路径：`~/.local/share/rumi/translate/doc-cache.json`
    /// （RUMI_TRANSLATE_MODELS_DIR 同源规则不适用——缓存独立于模型，用
    /// 环境变量 RUMI_TRANSLATE_CACHE 覆盖，缺省数据目录）。
    pub fn default_path() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rumi")
            .join("translate")
            .join("doc-cache.json")
    }

    /// 加载（文件不存在/损坏 JSON → 空缓存，不报错——缓存损坏不该阻断翻译）。
    pub fn load() -> Self {
        Self::load_at(Self::default_path())
    }

    /// 从指定路径加载（测试隔离入口：测试指向 tempdir，绝不碰用户真实缓存）。
    /// 文件不存在 / 损坏 JSON / 版本不符 → 空缓存（path 照记，save 可覆写修复）。
    pub fn load_at(path: PathBuf) -> Self {
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CacheFile>(&bytes).ok())
            .filter(|f| f.version == CACHE_VERSION)
            .map(|f| f.entries)
            .unwrap_or_default();
        Self { entries, path }
    }

    /// 查询。命中返回译文克隆。
    pub fn get(
        &self,
        text: &str,
        from: &str,
        to: &str,
        engine: &crate::Engine,
        polish: bool,
    ) -> Option<String> {
        let key = cache_key(text, from, to, engine, polish);
        self.entries.get(&key).cloned()
    }

    /// 写入。条数上限的维护在规格备选里三选一：Vec<key> 插入序淘汰最旧 25% /
    /// 全量重建保留后 3750 条 / 直接清空——**采用清空策略**（规格写死）：代码
    /// 最简，缓存语义无损（丢的只是"省一次引擎调用"，重翻即回血）。连刚插入
    /// 的一条也一并清掉（规格测试口径"插 MAX+1 条断言 len 回到 0"）。
    pub fn insert(
        &mut self,
        text: &str,
        from: &str,
        to: &str,
        engine: &crate::Engine,
        polish: bool,
        translation: &str,
    ) {
        let key = cache_key(text, from, to, engine, polish);
        self.entries.insert(key, translation.to_string());
        if self.entries.len() > MAX_ENTRIES {
            self.entries.clear();
        }
    }

    /// 当前条目数（测试断言清空策略用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空缓存。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 落盘（原子写：先写 doc-cache.json.part 再 rename——中途崩溃不会留下
    /// 半截 JSON，下次要么旧缓存要么新缓存）。目录不存在则建。
    /// 序列化/写盘失败静默忽略（缓存尽力而为，不该阻断翻译）。
    pub fn save(&self) {
        if let Some(dir) = self.path.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        let file = CacheFile {
            version: CACHE_VERSION,
            entries: self.entries.clone(),
        };
        let Ok(json) = serde_json::to_string(&file) else {
            return;
        };
        // part 与主文件同目录同分区，rename 才是原子替换
        let part = self.path.with_extension("json.part");
        if std::fs::write(&part, json).is_ok() {
            let _ = std::fs::rename(&part, &self.path);
        }
    }
}

/// 缓存键组装（格式见 [`DocCache`] 类型文档）。
fn cache_key(text: &str, from: &str, to: &str, engine: &crate::Engine, polish: bool) -> String {
    format!(
        "{:016x}|{from}|{to}|{}|{}",
        hash64(text),
        engine_tag(engine),
        polish
    )
}

/// 引擎判别标识（规格 §1）：配置任一分量变化 → tag 变 → 必 miss。key/url 类
/// 敏感分量只进 hash 不进明文（落盘 JSON 不携带密钥材料）。
pub(crate) fn engine_tag(e: &crate::Engine) -> String {
    /// 8 位 hex 摘要（同一 [`hash64`]，缓存非安全边界够用）
    fn hash8(s: &str) -> String {
        format!("{:08x}", hash64(s))
    }
    match e {
        crate::Engine::Offline => "offline".to_string(),
        crate::Engine::Google => "google".to_string(),
        crate::Engine::Microsoft => "microsoft".to_string(),
        crate::Engine::DeepL { api_key, pro } => {
            // pro 走不同 API 端点（api / api-free），译文可能不同——并入摘要
            format!("deepl:{}", hash8(&format!("{api_key}:{pro}")))
        }
        crate::Engine::DeepLX { url } => format!("deeplx:{}", hash8(url)),
        // glossary 字段不进 tag（glossary 规格 §3/§4 的 v1 取舍）：术语精确
        // 命中发生在缓存查询之前、不产生缓存读写；prompt 注入只影响其余
        // 单元的译文语境。tag 改动会让全部既有 openai 缓存条目失效，故保守不动。
        crate::Engine::OpenAI {
            base_url,
            api_key,
            model,
            ..
        } => format!("openai:{base_url}{model}{}", hash8(api_key)),
    }
}

/// SipHash-64：DefaultHasher::new() 用固定密钥，同一条文本跨进程得到同一
/// hash——缓存持久化跨进程有效的前提（RandomState 每进程随机，不可用）。
fn hash64(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}
