//! 目录/删除/NLLB 注册表单测（不打网）：remove 幂等与防穿越、nllb_code 码表、
//! NLLB 注册表条目断言。parse_index / hf_dest_name 是 pub(crate)/私有函数，
//! 集成测试不可见，其 fixture 单测在 download.rs 内部 #[cfg(test)] mod
//! （与 unpack_argos 同款处理，规格 §5）。

use rumi_translate::download::{self, Mirrors};
use rumi_translate::engine::nllb_code;
use rumi_translate::lang::UI_LANGS;
use rumi_translate::registry::{self, ModelKind, ModelSource};
use std::fs::{self, File};
use std::io::{Read as _, Write as _};

/// remove 正常路径：整目录删除（含嵌套子目录与 .part 残件），返回被删目录。
#[test]
fn remove_deletes_model_dir_wholesale() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let model = dir.join("en_zh");
    fs::create_dir_all(&model).unwrap();
    fs::write(model.join("model.bin"), b"abcd").unwrap();
    fs::write(model.join("model.bin.part"), b"par").unwrap(); // .part 残件一并删
    let sub = model.join("nested");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("x"), b"x").unwrap();

    let removed = download::remove("en_zh", dir).unwrap();
    assert_eq!(removed, dir.join("en_zh"));
    assert!(!model.exists(), "整目录（含嵌套与 .part）都应删除");
    assert!(dir.exists(), "模型根目录保留");
}

/// remove 防穿越：key 直接拼进目录名，^[a-z0-9_]+$ 之外的输入一律 Err 且不落删除。
#[test]
fn remove_rejects_traversal_and_bad_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let victim = dir.join("en_zh");
    fs::create_dir_all(&victim).unwrap(); // 若穿越校验失效，此目录可能被误删
    for bad in ["../x", "..", "", "zh/../en_zh", "EN_ZH", "zh en", "/abs", "a/b"] {
        assert!(download::remove(bad, dir).is_err(), "key {bad:?} 应被拒");
    }
    assert!(victim.exists(), "被拒的 key 不得触发任何删除");
}

/// remove 幂等：目录不存在也 Ok，返回原路径（UI「先卸载再删」无需预判存在）。
#[test]
fn remove_missing_dir_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let removed = download::remove("never_installed", dir).unwrap();
    assert_eq!(removed, dir.join("never_installed"));
    download::remove("never_installed", dir).unwrap(); // 二次删除同样 Ok
}

/// nllb_code：核心码表正确 + UI_LANGS 全集有码（引擎兜底按 to 查表）+ 码表外 None。
#[test]
fn nllb_code_maps_core_and_all_ui_langs() {
    assert_eq!(nllb_code("zh"), Some("zho_Hans"));
    assert_eq!(nllb_code("en"), Some("eng_Latn"));
    assert_eq!(nllb_code("ja"), Some("jpn_Jpan"));
    assert_eq!(nllb_code("th"), Some("tha_Thai"));
    for lang in UI_LANGS {
        assert!(nllb_code(lang).is_some(), "UI 语言 {lang} 应有 NLLB 码");
    }
    assert_eq!(nllb_code("eo"), None);
    assert_eq!(nllb_code(""), None);
}

/// NLLB 注册表条目按规格 §1 逐字段断言（HF 单源 5 文件、同名 spm 两行带别名）。
#[test]
fn nllb_spec_shape_matches_spec() {
    let spec = registry::by_key("nllb_600m").expect("注册表应含 nllb_600m");
    assert_eq!(spec.kind, ModelKind::Nllb);
    assert_eq!((spec.from, spec.to, spec.size_mb), ("*", "*", 593));
    let [ModelSource::Hf { repo, files }] = spec.sources else {
        panic!("NLLB 应只有 HF 单源");
    };
    assert_eq!(*repo, "JustFrederik/nllb-200-distilled-600M-ct2-int8");
    assert_eq!(files.len(), 5);
    assert_eq!(files[0].name, "model.bin");
    assert_eq!(files[0].bytes, 622_595_991); // 规格 §1 实测值，勿改
    // 同 name+bytes 的 sentencepiece.bpe.model 两行 → source.spm / target.spm 两落盘名
    assert_eq!(files[1].name, "sentencepiece.bpe.model");
    assert_eq!(files[1].bytes, 4_852_054);
    assert_eq!(files[1].as_, Some("source.spm"));
    assert_eq!(files[2].name, "sentencepiece.bpe.model");
    assert_eq!(files[2].as_, Some("target.spm"));
    assert_eq!(files[3].name, "shared_vocabulary.txt");
    assert_eq!(files[4].name, "config.json");
}

/// 防回归：内置两包仍为 Marian 家族、无落盘别名；find() 不吃 "*" 条目
/// （NLLB 只经 by_key 走引擎兜底，不参与方向匹配）。
#[test]
fn builtin_marian_rows_keep_shape() {
    for key in ["zh_en", "en_zh"] {
        let spec = registry::by_key(key).unwrap();
        assert_eq!(spec.kind, ModelKind::Marian);
        let ModelSource::Hf { files, .. } = &spec.sources[1] else {
            panic!("内置包备源应为 HF");
        };
        assert!(files.iter().all(|f| f.as_.is_none()));
    }
    // find 是普通字段匹配（语义不变）：真实方向不会命中 from/to="*" 的 NLLB，
    // 只能经 by_key 走引擎兜底；find("*","*") 字面匹配属普通匹配语义，非方向匹配路径
    assert!(registry::find("zh", "ja").is_none());
    assert!(registry::find("zh", "en").is_some());
}

/// install_catalog_entry 全路径（本地一次性 HTTP 直链，不打外网）：
/// zip 下载（远端大小未知，expect=None 路线）→ 解包统一布局 →
/// manifest（source=argos、kind=marian）→ 二次安装幂等（不再打网络）。
#[test]
fn install_catalog_entry_downloads_unpacks_and_is_idempotent() {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    // 现场构造最小 argos 包（复刻真实布局：顶层目录 + model/ + sentencepiece.model）
    let tmp = tempfile::tempdir().unwrap();
    let pkg_path = tmp.path().join("translate-en_ja-1_1.argosmodel");
    {
        let f = File::create(&pkg_path).unwrap();
        let mut w = ZipWriter::new(f);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let add = |w: &mut ZipWriter<File>, name: &str, data: &[u8]| {
            w.start_file(name, opts).unwrap();
            w.write_all(data).unwrap();
        };
        add(&mut w, "translate-en_ja-1_1/model/model.bin", &[0xAB; 16]);
        add(&mut w, "translate-en_ja-1_1/model/shared_vocabulary.json", b"vocab");
        add(&mut w, "translate-en_ja-1_1/model/config.json", b"{}");
        add(&mut w, "translate-en_ja-1_1/sentencepiece.model", &[0x5A; 16]);
        add(
            &mut w,
            "translate-en_ja-1_1/metadata.json",
            br#"{"from_code":"en","to_code":"ja"}"#,
        );
        w.finish().unwrap();
    }
    let body = fs::read(&pkg_path).unwrap();

    // 一次性本地 HTTP 服务器：读掉请求头 → 回 200 + 完整 zip 体
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/translate-en_ja-1_1.argosmodel",
        listener.local_addr().unwrap().port()
    );
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut junk = [0u8; 4096];
        let _ = stream.read(&mut junk); // 请求头只需读掉，不解析
        let mut resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        resp.extend_from_slice(&body);
        stream.write_all(&resp).unwrap();
    });

    let entry = download::CatalogEntry {
        key: "en_ja".to_string(),
        from: "en".to_string(),
        to: "ja".to_string(),
        version: "1.1".to_string(),
        url,
    };
    let models = tempfile::tempdir().unwrap();
    let out = download::install_catalog_entry(&entry, models.path(), &Mirrors::default(), &|_| {})
        .unwrap();
    server.join().unwrap();
    assert_eq!(out, models.path().join("en_ja"));
    // 统一布局五文件齐全（sentencepiece.model 复制两份 spm）
    for name in
        ["model.bin", "shared_vocabulary.json", "config.json", "source.spm", "target.spm"]
    {
        assert!(out.join(name).is_file(), "缺 {name}");
    }
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), vec![0xAB; 16]);
    assert_eq!(fs::read(out.join("source.spm")).unwrap(), vec![0x5A; 16]);
    assert_eq!(fs::read(out.join("target.spm")).unwrap(), vec![0x5A; 16]);
    // manifest：key/from/to 取 entry，source=argos，kind=marian（argos 包恒定）
    let mf: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(mf["key"], "en_ja");
    assert_eq!(mf["from"], "en");
    assert_eq!(mf["to"], "ja");
    assert_eq!(mf["source"], "argos");
    assert_eq!(mf["kind"], "marian");
    // 二次安装幂等：manifest 记账完整 → 早退，不再打网络（服务器线程已退出）
    assert_eq!(
        download::install_catalog_entry(&entry, models.path(), &Mirrors::default(), &|_| {})
            .unwrap(),
        out
    );
}
