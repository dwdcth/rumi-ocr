//! 文档缓存纯逻辑单测（规格 L §3）：全部走 DocCache::load_at(tempdir)，
//! 不读默认路径也不碰 env——测试绝不触及用户真实缓存目录。覆盖：缺失/损坏
//! JSON/版本不符 → 空缓存；get/insert roundtrip 与参数敏感 miss；engine_tag
//! 稳定性与配置敏感；条数超限清空策略；save→load 持久化。

use rumi_translate::cache::{DocCache, MAX_ENTRIES};
use rumi_translate::Engine;

/// tempdir 下的缓存实例（文件名与真实默认一致，part 残件断言才有意义）。
fn cache_at(dir: &std::path::Path) -> DocCache {
    DocCache::load_at(dir.join("doc-cache.json"))
}

/// load_at 不存在文件 → 空缓存（不报错）。
#[test]
fn load_at_missing_file_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let c = cache_at(dir.path());
    assert!(c.is_empty());
    assert_eq!(c.len(), 0);
    assert_eq!(
        c.get("hello", "en", "zh", &Engine::Offline, false),
        None,
        "空缓存任何查询都 miss"
    );
}

/// 损坏 JSON → 空缓存（缓存损坏不该阻断翻译）；随后 save 覆写修复该文件。
#[test]
fn load_at_corrupt_json_is_empty_and_recoverable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc-cache.json");
    std::fs::write(&path, "{not json{{{").unwrap();
    let c = DocCache::load_at(path.clone());
    assert!(c.is_empty(), "损坏 JSON 应当空缓存：");
    c.save();
    let c2 = DocCache::load_at(path);
    assert!(c2.is_empty(), "save 后应已是合法空缓存文件");
}

/// 版本不符（未知/未来格式）→ 当空缓存，不迁移不报错（旧格式直接重翻）。
#[test]
fn load_at_wrong_version_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc-cache.json");
    std::fs::write(&path, r#"{"version": 99, "entries": {"k": "v"}}"#).unwrap();
    let c = DocCache::load_at(path);
    assert!(c.is_empty(), "非当前版本应整体当空缓存");
}

/// get/insert roundtrip：同文本+同参数命中，返回译文克隆。
#[test]
fn insert_get_roundtrip_hits() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cache_at(dir.path());
    c.insert("Hello world", "en", "zh", &Engine::Offline, false, "你好世界");
    assert_eq!(
        c.get("Hello world", "en", "zh", &Engine::Offline, false),
        Some("你好世界".to_string())
    );
    assert_eq!(c.len(), 1);
}

/// 缓存键的参数敏感面：from/to/polish/文本 任一不同即 miss，且查询不增条目
/// （引擎维度单独用例覆盖）。
#[test]
fn param_change_misses() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cache_at(dir.path());
    c.insert("Hello world", "en", "zh", &Engine::Offline, false, "你好世界");
    assert_eq!(
        c.get("Hello world", "ja", "zh", &Engine::Offline, false),
        None,
        "from 不同应 miss"
    );
    assert_eq!(
        c.get("Hello world", "en", "ja", &Engine::Offline, false),
        None,
        "to 不同应 miss"
    );
    assert_eq!(
        c.get("Hello world", "en", "zh", &Engine::Offline, true),
        None,
        "polish 不同应 miss"
    );
    assert_eq!(
        c.get("Other text", "en", "zh", &Engine::Offline, false),
        None,
        "文本不同应 miss"
    );
    assert_eq!(c.len(), 1, "查询不应写入条目");
}

/// engine_tag 稳定性（offline）：DefaultHasher 固定密钥，键跨实例（重载后）
/// 一致——持久化跨进程有效的前提。
#[test]
fn engine_tag_offline_stable_across_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cache_at(dir.path());
    c.insert("Hello world", "en", "zh", &Engine::Offline, false, "你好世界");
    c.save();
    let c2 = DocCache::load_at(dir.path().join("doc-cache.json"));
    assert_eq!(
        c2.get("Hello world", "en", "zh", &Engine::Offline, false),
        Some("你好世界".to_string()),
        "重载后同参数应命中"
    );
}

/// engine_tag 配置敏感（deepl）：key 不同 → miss；pro 不同 → miss（free/pro
/// 走不同 API 端点，译文可能不同，宁可 miss 不错用）。
#[test]
fn engine_tag_deepl_config_sensitive() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cache_at(dir.path());
    c.insert(
        "Hello world",
        "en",
        "zh",
        &Engine::DeepL {
            api_key: "key-a".into(),
            pro: false,
        },
        false,
        "甲",
    );
    assert_eq!(
        c.get(
            "Hello world",
            "en",
            "zh",
            &Engine::DeepL {
                api_key: "key-b".into(),
                pro: false
            },
            false
        ),
        None,
        "deepl key 不同应 miss"
    );
    assert_eq!(
        c.get(
            "Hello world",
            "en",
            "zh",
            &Engine::DeepL {
                api_key: "key-a".into(),
                pro: true
            },
            false
        ),
        None,
        "deepl pro 不同应 miss"
    );
    assert_eq!(
        c.get(
            "Hello world",
            "en",
            "zh",
            &Engine::DeepL {
                api_key: "key-a".into(),
                pro: false
            },
            false
        ),
        Some("甲".to_string()),
        "完全同配置应命中"
    );
}

/// 在线引擎形态互不串台：google/deeplx/openai 各自独立键位，deeplx 换 url、
/// openai 换 model 均 miss。
#[test]
fn engine_tag_online_engines_do_not_cross_hit() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cache_at(dir.path());
    c.insert("Hello world", "en", "zh", &Engine::Google, false, "谷歌");
    c.insert(
        "Hello world",
        "en",
        "zh",
        &Engine::DeepLX {
            url: "http://x:1188".into(),
        },
        false,
        "deeplx",
    );
    c.insert(
        "Hello world",
        "en",
        "zh",
        &Engine::OpenAI {
            base_url: "http://api".into(),
            api_key: "k".into(),
            model: "gpt".into(),
            glossary: Vec::new(),
        },
        false,
        "gpt",
    );
    assert_eq!(c.len(), 3, "三种引擎键位应互不相同");
    assert_eq!(
        c.get("Hello world", "en", "zh", &Engine::Google, false),
        Some("谷歌".to_string())
    );
    assert_eq!(
        c.get(
            "Hello world",
            "en",
            "zh",
            &Engine::DeepLX {
                url: "http://y:1188".into()
            },
            false
        ),
        None,
        "deeplx url 不同应 miss"
    );
    assert_eq!(
        c.get(
            "Hello world",
            "en",
            "zh",
            &Engine::OpenAI {
                base_url: "http://api".into(),
                api_key: "k".into(),
                model: "other".into(),
                glossary: Vec::new(),
            },
            false
        ),
        None,
        "openai model 不同应 miss"
    );
}

/// 条数超限清空策略（规格 §1 写死）：插入 MAX_ENTRIES+1 条 → 整体清空
/// （len 回 0，连刚插入的一条也清掉——重翻即回血，缓存语义无损）。
#[test]
fn overflow_clears_all_entries() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cache_at(dir.path());
    for i in 0..=MAX_ENTRIES {
        c.insert(
            &format!("text-{i}"),
            "en",
            "zh",
            &Engine::Offline,
            false,
            &format!("译-{i}"),
        );
    }
    assert_eq!(c.len(), 0, "超限应整体清空");
    assert_eq!(
        c.get("text-0", "en", "zh", &Engine::Offline, false),
        None,
        "清空后旧条目应 miss"
    );
    // 清空后可继续正常写入（缓存语义无损的直证）
    c.insert("fresh", "en", "zh", &Engine::Offline, false, "新译文");
    assert_eq!(c.len(), 1);
}

/// save→load roundtrip：持久化带 version 头，原子写不留 .part 残件。
#[test]
fn save_load_roundtrip_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc-cache.json");
    let mut c = DocCache::load_at(path.clone());
    c.insert("Hello world", "en", "zh", &Engine::Offline, true, "你好世界");
    c.save();
    assert!(
        !dir.path().join("doc-cache.json.part").exists(),
        "rename 成功后 part 残件应消失"
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains(r#""version":1"#), "应带 version 头：{raw}");
    let c2 = DocCache::load_at(path);
    assert_eq!(
        c2.get("Hello world", "en", "zh", &Engine::Offline, true),
        Some("你好世界".to_string())
    );
    assert_eq!(c2.len(), 1);
}
