//! download 单测（不打真网）：is_complete 两/三态校验 + import_argosmodel
//! 用现场构造的最小 argosmodel 包（复刻真实布局，规格 §6 测试条目）。

use rumi_translate::download::{self, Mirrors};
use rumi_translate::registry::{ModelFile, ModelKind, ModelSource, ModelSpec};
use std::fs::{self, File};
use std::io::Write as _;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// 复刻真实包布局（顶层 translate-en_zh-1_9/，内含 model/、sentencepiece.model、
/// metadata.json、stanza/）构造最小 zip。
fn build_pkg(path: &Path, with_vocab: bool, with_meta: bool) {
    let f = File::create(path).unwrap();
    let mut w = ZipWriter::new(f);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let add = |w: &mut ZipWriter<File>, name: &str, data: &[u8]| {
        w.start_file(name, opts).unwrap();
        w.write_all(data).unwrap();
    };
    add(&mut w, "translate-en_zh-1_9/model/model.bin", &[0xAB; 16]);
    if with_vocab {
        add(&mut w, "translate-en_zh-1_9/model/shared_vocabulary.json", b"vocab");
    }
    add(&mut w, "translate-en_zh-1_9/model/config.json", b"{}");
    add(&mut w, "translate-en_zh-1_9/sentencepiece.model", &[0x5A; 16]);
    if with_meta {
        add(
            &mut w,
            "translate-en_zh-1_9/metadata.json",
            br#"{"from_code":"en","to_code":"zh"}"#,
        );
    }
    add(&mut w, "translate-en_zh-1_9/stanza/resources.json", b"ignored");
    w.finish().unwrap();
}

/// 仅 key 参与 is_complete 的 manifest 校验，sources 留空即可。
fn spec_with_key(key: &'static str) -> ModelSpec {
    ModelSpec {
        key,
        from: "en",
        to: "zh",
        sources: &[ModelSource::Hf { repo: "x/y", files: &[ModelFile { name: "model.bin", bytes: 16, as_: None }] }],
        size_mb: 1,
        kind: ModelKind::Marian,
    }
}

#[test]
fn is_complete_requires_manifest_and_exact_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = spec_with_key("t_cmp");
    let dir = tmp.path();

    // 空目录 → 不完整
    assert!(!download::is_complete(&spec, dir));

    let d = dir.join("t_cmp");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("model.bin"), vec![0u8; 16]).unwrap();
    // 成品在但 manifest 缺 → 不完整
    assert!(!download::is_complete(&spec, dir));

    fs::write(
        d.join("manifest.json"),
        r#"{"key":"t_cmp","from":"en","to":"zh","source":"hf",
            "files":[{"name":"model.bin","bytes":16}],"installed_bytes":16}"#,
    )
    .unwrap();
    // manifest + 字节相符 → 完整
    assert!(download::is_complete(&spec, dir));

    // 字节数不符 → 不完整
    fs::write(d.join("model.bin"), vec![0u8; 15]).unwrap();
    assert!(!download::is_complete(&spec, dir));
}

#[test]
fn import_argosmodel_unifies_layout_and_writes_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("translate-en_zh-1_9.argosmodel");
    build_pkg(&pkg, true, true);

    let out = download::import_argosmodel(&pkg, tmp.path()).unwrap();
    // 包名取 zip 顶层目录名
    assert_eq!(out, tmp.path().join("translate-en_zh-1_9"));

    // 统一布局五文件齐全且内容正确
    for name in
        ["model.bin", "shared_vocabulary.json", "config.json", "source.spm", "target.spm"]
    {
        assert!(out.join(name).is_file(), "缺 {name}");
    }
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), vec![0xAB; 16]);
    assert_eq!(fs::read(out.join("source.spm")).unwrap(), vec![0x5A; 16]);
    assert_eq!(fs::read(out.join("target.spm")).unwrap(), vec![0x5A; 16]);
    // stanza / metadata.json 不落盘
    assert!(!out.join("stanza").exists());
    assert!(!out.join("metadata.json").exists());

    // manifest 可解析：from/to 取自包内 metadata.json，files 按实际落盘
    let mf: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(mf["key"], "translate-en_zh-1_9");
    assert_eq!(mf["from"], "en");
    assert_eq!(mf["to"], "zh");
    assert_eq!(mf["source"], "argos");
    assert_eq!(mf["kind"], "marian"); // argos 导入包恒为 Marian 家族
    let files = mf["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    assert!(
        files.iter().any(|f| f["name"] == "model.bin" && f["bytes"] == 16),
        "model.bin 字节数未入 manifest"
    );
    // installed_bytes = 16(model) + 5(vocab) + 2(config) + 16 + 16(spm×2)
    assert_eq!(mf["installed_bytes"], 55);

    // 导入结果即完整（is_complete 走 manifest，与来源无关）
    let spec = spec_with_key("translate-en_zh-1_9");
    assert!(download::is_complete(&spec, tmp.path()));
}

#[test]
fn import_argosmodel_keeps_existing_spm() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("withspm-en_zh.argosmodel");
    {
        let f = File::create(&pkg).unwrap();
        let mut w = ZipWriter::new(f);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let add = |w: &mut ZipWriter<File>, name: &str, data: &[u8]| {
            w.start_file(name, opts).unwrap();
            w.write_all(data).unwrap();
        };
        add(&mut w, "p/model.bin", &[0u8; 16]);
        add(&mut w, "p/shared_vocabulary.json", b"vocab");
        add(&mut w, "p/source.spm", b"orig"); // 包内已有 source.spm → 不覆盖
        add(&mut w, "sentencepiece.model", &[0x5A; 16]); // 顶层（无层级）也应被找到
        w.finish().unwrap();
    }
    let out = download::import_argosmodel(&pkg, tmp.path()).unwrap();
    assert_eq!(fs::read(out.join("source.spm")).unwrap(), b"orig");
    assert_eq!(fs::read(out.join("target.spm")).unwrap(), vec![0x5A; 16]);
}

#[test]
fn import_argosmodel_without_metadata_blank_from_to() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("nometa-en_zh.argosmodel");
    build_pkg(&pkg, true, false);
    let out = download::import_argosmodel(&pkg, tmp.path()).unwrap();
    let mf: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(mf["from"], "");
    assert_eq!(mf["to"], "");
}

#[test]
fn import_argosmodel_rejects_missing_vocabulary() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("novocab-en_zh.argosmodel");
    build_pkg(&pkg, false, false);
    let err = download::import_argosmodel(&pkg, tmp.path()).unwrap_err();
    assert!(err.contains("vocabulary"), "got: {err}");
}

#[test]
fn import_flat_pkg_falls_back_to_file_stem() {
    // 扁平 zip（无顶层目录）：包名退回文件名去扩展
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("my-flat-en_zh.argosmodel");
    {
        let f = File::create(&pkg).unwrap();
        let mut w = ZipWriter::new(f);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let add = |w: &mut ZipWriter<File>, name: &str, data: &[u8]| {
            w.start_file(name, opts).unwrap();
            w.write_all(data).unwrap();
        };
        add(&mut w, "model.bin", &[0u8; 16]);
        add(&mut w, "shared_vocabulary.json", b"vocab");
        add(&mut w, "sentencepiece.model", &[0x5A; 16]);
        w.finish().unwrap();
    }
    let out = download::import_argosmodel(&pkg, tmp.path()).unwrap();
    assert_eq!(out, tmp.path().join("my-flat-en_zh"));
    assert!(out.join("source.spm").is_file());
    // Mirrors 仅参与 install（网络路径），此处引用以确保 Default 可构造
    let _ = Mirrors::default();
}
