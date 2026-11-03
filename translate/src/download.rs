//! 模型下载与安装：argos-net 直链主源 + HF（含镜像）备源，断点续传，
//! argos zip 与 HF 多文件统一安装为同一目录布局（engine 不感知来源）。
//! 另含官方目录（argos index.json）的拉取/解析与目录条目安装、已装模型删除。

use crate::registry::{ModelFile, ModelKind, ModelSource, ModelSpec};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 单文件下载进度（downloaded 含续传前缀，整文件口径）。
#[derive(Debug, Clone)]
pub struct Progress {
    pub file: &'static str,
    pub downloaded: u64,
    pub total: Option<u64>,
}

pub type ProgressFn<'a> = &'a (dyn Fn(Progress) + Send + Sync);

/// 镜像配置（UI 设置可注入；Default 为内置国内友好默认）。
#[derive(Debug, Clone)]
pub struct Mirrors {
    pub argos_base: String,            // 默认 "https://argos-net.com/v1"
    pub hf_hosts: Vec<String>,         // 默认 ["huggingface.co", "hf-mirror.com"]
    pub github_prefixes: Vec<String>, // 默认 ["", "https://ghfast.top", "https://gh-proxy.com", "https://ghproxy.net"]
}

impl Default for Mirrors {
    fn default() -> Self {
        Self {
            argos_base: "https://argos-net.com/v1".to_string(),
            hf_hosts: vec!["huggingface.co".to_string(), "hf-mirror.com".to_string()],
            github_prefixes: vec![
                String::new(),
                "https://ghfast.top".to_string(),
                "https://gh-proxy.com".to_string(),
                "https://ghproxy.net".to_string(),
            ],
        }
    }
}

/// 每源整体超时（含 302 → CDN 跳转全程；规格 §6）。
const HOST_TIMEOUT: Duration = Duration::from_secs(30);
/// 读缓冲 8KB（规格 §6：into_reader + 8KB buf）。
const READ_BUF: usize = 8 * 1024;
/// 进度回调粒度 256KB（规格 §6）。
const PROGRESS_STEP: u64 = 256 * 1024;

/// 下载 agent：30s 超时 + 5 次重定向（HF resolve 302 → CDN 必须留足）+
/// UA rumi-translate/0.1。系统代理不用显式设置——ureq build() 默认读
/// https_proxy/all_proxy 等环境变量（内部 Proxy::try_from_system），失败自动直连。
fn new_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(HOST_TIMEOUT)
        .redirects(5)
        .user_agent("rumi-translate/0.1")
        .build()
}

/// `.part` 临时文件路径（下载中落盘名，校验通过后 rename 为正式名）。
fn part_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.part"))
}

/// 安装 spec 到 `dir/<key>/`，统一目录布局（argos 与 HF 殊途同归）：
/// `<key>/{model.bin, shared_vocabulary.json, config.json, source.spm, target.spm, manifest.json}`。
/// sources 按优先级尝试；已完整则直接返回；重装时清掉旧成品文件但保留 .part 续传。
pub fn install(
    spec: &ModelSpec,
    dir: &Path,
    mirrors: &Mirrors,
    progress: ProgressFn<'_>,
) -> Result<PathBuf, String> {
    let dest = dir.join(spec.key);
    fs::create_dir_all(&dest).map_err(|e| format!("创建 {} 失败：{e}", dest.display()))?;

    // 已完整 → 幂等返回
    if is_complete(spec, dir) {
        return Ok(dest);
    }
    // 未完整：清掉旧成品（可能来自上一个失败/换源的安装，如 argos 的 config.json），
    // 保留 *.part 供断点续传
    for entry in fs::read_dir(&dest).map_err(|e| format!("读 {} 失败：{e}", dest.display()))? {
        let path = match entry {
            Ok(e) => e.path(),
            Err(_) => continue,
        };
        if path.extension() != Some(OsStr::new("part")) {
            let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        }
    }

    let agent = new_agent();
    let mut errs: Vec<String> = Vec::new();
    for source in spec.sources {
        let res = match source {
            ModelSource::Argos { file, bytes } => {
                install_argos(&agent, *file, *bytes, dir, &dest, mirrors, progress)
            }
            ModelSource::Hf { repo, files } => {
                install_hf(&agent, *repo, files, &dest, mirrors, progress)
            }
        };
        match res {
            Ok(files) => {
                write_manifest(
                    &dest.join("manifest.json"),
                    spec.key,
                    spec.from,
                    spec.to,
                    source_tag(source),
                    kind_tag(spec.kind),
                    &files,
                )?;
                return Ok(dest);
            }
            Err(e) => errs.push(e), // 该源失败，换下一源
        }
    }
    Err(format!(
        "模型 {} 所有源均下载失败：{}",
        spec.key,
        errs.join("；")
    ))
}

/// manifest.json：{"key","from","to","source","kind","files":[{"name","bytes"}...],
/// "installed_bytes"}。kind 标模型家族（marian/nllb），engine 的 Loaded 据此选推理调用。
fn write_manifest(
    path: &Path,
    key: &str,
    from: &str,
    to: &str,
    source: &str,
    kind: &str,
    files: &[(String, u64)],
) -> Result<(), String> {
    let installed = files.iter().map(|(_, b)| b).sum::<u64>();
    let value = serde_json::json!({
        "key": key,
        "from": from,
        "to": to,
        "source": source,
        "kind": kind,
        "files": files
            .iter()
            .map(|(n, b)| serde_json::json!({ "name": n, "bytes": b }))
            .collect::<Vec<_>>(),
        "installed_bytes": installed,
    });
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("序列化 manifest 失败：{e}"))?;
    fs::write(path, body).map_err(|e| format!("写 {} 失败：{e}", path.display()))
}

fn source_tag(source: &ModelSource) -> &'static str {
    match source {
        ModelSource::Argos { .. } => "argos",
        ModelSource::Hf { .. } => "hf",
    }
}

/// ModelKind → manifest kind 字段（argos 包恒为 marian，NLLB 注册表条目为 nllb）。
fn kind_tag(kind: ModelKind) -> &'static str {
    match kind {
        ModelKind::Marian => "marian",
        ModelKind::Nllb => "nllb",
    }
}

/// argos 分支：单 zip 直链下载到 `dir/<file>.part`，校验后解包成统一布局。
/// manifest 的 from/to 恒取 spec（写于 install 层），包内元数据在此不参与。
fn install_argos(
    agent: &ureq::Agent,
    file: &'static str,
    bytes: u64,
    dir: &Path,
    dest: &Path,
    mirrors: &Mirrors,
    progress: ProgressFn<'_>,
) -> Result<Vec<(String, u64)>, String> {
    let url = format!("{}/{}", mirrors.argos_base.trim_end_matches('/'), file);
    install_zip(agent, &url, file, Some(bytes), dir, dest, progress)
}

/// zip 直链 → 统一布局（install 的 argos 分支与 install_catalog_entry 共用内核）：
/// .part 断点续传 → 解包 → 删临时件 → 列落盘文件。expect=Some（argos 直链，
/// 字节数注册表实测可知）按声明值做续传/快速通道判定与最终校验；expect=None
/// （目录条目，远端大小未知）时已有 .part 无法断言完整性，先试解包快速通道、
/// 失败整件重下。
fn install_zip(
    agent: &ureq::Agent,
    url: &str,
    name: &'static str,
    expect: Option<u64>,
    part_dir: &Path,
    dest: &Path,
    progress: ProgressFn<'_>,
) -> Result<Vec<(String, u64)>, String> {
    let part = part_path(part_dir, name); // zip 临时件放在模型根，不进 <key>/（解包后即删）
    match expect {
        Some(bytes) => {
            let len = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            if len > bytes {
                let _ = fs::remove_file(&part); // 超长 .part 是脏数据，重下
            } else if len < bytes {
                fetch_to_file(agent, url, &part, name, expect, progress)?;
            }
            // .part 已满：直接校验解包（上次下载完成但解包/落盘失败的快速通道）
        }
        None => {
            if part.is_file() {
                if unpack_argos(&part, dest).is_ok() {
                    let _ = fs::remove_file(&part);
                    return list_dir_files(dest);
                }
                let _ = fs::remove_file(&part); // 半截/坏件，弃掉重下
            }
            fetch_to_file(agent, url, &part, name, None, progress)?;
        }
    }
    let (_from, _to) = unpack_argos(&part, dest)?; // from/to 恒取 spec/entry，忽略包内元数据
    let _ = fs::remove_file(&part); // 解包成功即删 ~70MB 临时 zip
    list_dir_files(dest)
}

/// HF 分支：逐文件按 mirrors.hf_hosts 顺序下载（单文件级换源）。
/// 落盘名走 [`hf_dest_name`] 别名；同 name+bytes 的重复条目（NLLB 的
/// sentencepiece.bpe.model → source.spm/target.spm 两份）只下载一次，
/// 内容缓存到内存再写第二份——注册表是编译期常量，重复项都是 spm 级小文件，
/// 不会把 model.bin 级大文件吃进内存。
fn install_hf(
    agent: &ureq::Agent,
    repo: &str,
    files: &'static [ModelFile],
    dest: &Path,
    mirrors: &Mirrors,
    progress: ProgressFn<'_>,
) -> Result<Vec<(String, u64)>, String> {
    let mut done: Vec<(String, u64)> = Vec::new();
    let mut dup: Vec<(&'static str, Vec<u8>)> = Vec::new(); // 本轮已下载内容（按 name 去重）
    for f in files {
        let dest_name = hf_dest_name(f);
        let final_path = dest.join(dest_name);
        if fs::metadata(&final_path).map(|m| m.len() == f.bytes).unwrap_or(false) {
            done.push((dest_name.to_string(), f.bytes));
            continue;
        }
        // 去重命中：同 name+bytes 的前一份刚下载过 → 直接复制，不二次下载
        if let Some((_, data)) =
            dup.iter().find(|(n, d)| *n == f.name && d.len() as u64 == f.bytes)
        {
            fs::write(&final_path, data)
                .map_err(|e| format!("落盘 {} 失败：{e}", final_path.display()))?;
            done.push((dest_name.to_string(), f.bytes));
            continue;
        }
        let part = part_path(dest, f.name);
        let len = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if len > f.bytes {
            let _ = fs::remove_file(&part);
        } else if len == f.bytes {
            // 上次下载完成但未及 rename → 直接落盘
            fs::rename(&part, &final_path)
                .map_err(|e| format!("落盘 {} 失败：{e}", final_path.display()))?;
            done.push((dest_name.to_string(), f.bytes));
            continue;
        }
        let mut last_err = String::new();
        let mut ok = false;
        for host in &mirrors.hf_hosts {
            let url = format!("https://{host}/{repo}/resolve/main/{}", f.name);
            match fetch_to_file(agent, &url, &part, f.name, Some(f.bytes), progress) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => last_err = e, // 连接错误/4xx/5xx/字节数不符 → 换下一 host
            }
        }
        if !ok {
            return Err(format!(
                "HF 源下载 {} 失败（已尝试 {}）：{}",
                f.name,
                mirrors.hf_hosts.join("、"),
                last_err
            ));
        }
        fs::rename(&part, &final_path)
            .map_err(|e| format!("落盘 {} 失败：{e}", final_path.display()))?;
        // 进去重缓存，供同 name 的下一份复制（见函数注释的安全性说明）
        let data =
            fs::read(&final_path).map_err(|e| format!("读 {} 失败：{e}", final_path.display()))?;
        dup.push((f.name, data));
        done.push((dest_name.to_string(), f.bytes));
    }
    Ok(done)
}

/// HF 文件的落盘名：as_ 别名优先（NLLB 的 sentencepiece.bpe.model →
/// source.spm / target.spm），否则同名落盘。抽纯函数便于单测（规格 §5）。
fn hf_dest_name(f: &ModelFile) -> &str {
    f.as_.unwrap_or(f.name)
}

/// 从单个 URL 下载到 `part`。Range 断点续传（206 追加 / 200 覆盖重写）；
/// expect=Some 时先比 content-length 与声明值（302 后以最终响应为准），不符即
/// Err 换源，流提前结束也 Err；expect=None（远端大小未知，目录条目）只做
/// 下载不校验总量。进度每 256KB 回调一次。
fn fetch_to_file(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    name: &'static str,
    expect: Option<u64>,
    progress: ProgressFn<'_>,
) -> Result<(), String> {
    let already = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let mut req = agent.get(url);
    if already > 0 {
        req = req.set("Range", &format!("bytes={already}-"));
    }
    let resp = req.call().map_err(|e| format!("{url}：{e}"))?;
    // ureq 对 4xx/5xx 返回 Err，走到这里的必然是 2xx；206=续传，200=服务器不理 Range
    let resumed = resp.status() == 206;
    let start_at = if resumed { already } else { 0 };

    let content_len = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
    if let (Some(expect), Some(cl)) = (expect, content_len) {
        if start_at + cl != expect {
            return Err(format!(
                "{url}：content-length 与声明不符（响应 {cl}，续传前缀 {start_at}，声明 {expect}）"
            ));
        }
    }
    let total = content_len.map(|cl| start_at + cl);

    let mut reader = resp.into_reader();
    let mut file = if resumed {
        OpenOptions::new().create(true).append(true).open(part)
    } else {
        File::create(part) // create + truncate 覆盖写
    }
    .map_err(|e| format!("打开 {} 失败：{e}", part.display()))?;

    let mut buf = [0u8; READ_BUF];
    let mut downloaded = start_at;
    let mut last_report = start_at;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("{name}：读流中断：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("{name}：写盘失败：{e}"))?;
        downloaded += n as u64;
        if downloaded - last_report >= PROGRESS_STEP {
            progress(Progress { file: name, downloaded, total });
            last_report = downloaded;
        }
    }
    if downloaded != last_report {
        // 收尾回调，保证 UI 能看到 100%
        progress(Progress { file: name, downloaded, total });
    }
    if let Some(expect) = expect {
        if downloaded != expect {
            return Err(format!("{name}：流提前结束（{downloaded}/{expect}）"));
        }
    }
    Ok(())
}

/// 完整性 = manifest.json 可解析且其 files 列表逐项字节相符（manifest 在
/// 安装全部就绪后才写，天然作为"装好了"的标记；argos/HF 两源通用）。
pub fn is_complete(spec: &ModelSpec, dir: &Path) -> bool {
    let dest = dir.join(spec.key);
    let Ok(text) = fs::read_to_string(dest.join("manifest.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(files) = value.get("files").and_then(|f| f.as_array()) else {
        return false;
    };
    if files.is_empty() {
        return false;
    }
    files.iter().all(|f| {
        let name = f.get("name").and_then(|n| n.as_str());
        let bytes = f.get("bytes").and_then(|b| b.as_u64());
        match (name, bytes) {
            (Some(n), Some(b)) => {
                fs::metadata(dest.join(n)).map(|m| m.len() == b).unwrap_or(false)
            }
            _ => false,
        }
    })
}

/// 导入本地 .argosmodel 包到 `dir/<pkg>/`（pkg 取 zip 顶层目录名，扁平包退回
/// 文件名去扩展）。解包与 install 的 argos 分支复用同一内部函数；from/to 从包内
/// metadata.json 的 from_code/to_code 读（读不到置空串）；manifest source = "argos"。
pub fn import_argosmodel(pkg: &Path, dir: &Path) -> Result<PathBuf, String> {
    // 包名：zip 顶层目录名优先（实测包顶层是 translate-xx-1_9/），退回文件名去扩展
    let pkg_name = zip_top_dir(pkg)?.unwrap_or_else(|| {
        pkg.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string()
    });
    if pkg_name.is_empty() || pkg_name == "." || pkg_name == ".." {
        return Err(format!("argosmodel 包名无效：{}", pkg.display()));
    }
    let out = dir.join(&pkg_name);
    let (from, to) = unpack_argos(pkg, &out)?;
    let files = list_dir_files(&out)?;
    write_manifest(&out.join("manifest.json"), &pkg_name, &from, &to, "argos", "marian", &files)?;
    Ok(out)
}

/// 删除已装模型：key 只允许 ^[a-z0-9_]+$（key 直接拼进目录名，先挡路径穿越），
/// 删 `dir/<key>/` 整目录（含 .part 残件）。目录不存在 → Ok（幂等）。
/// 返回被删目录 PathBuf（目录本不存在时返回原路径，供调用方提示）。
pub fn remove(key: &str, dir: &Path) -> Result<PathBuf, String> {
    let valid = !key.is_empty()
        && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(format!("非法模型 key：{key}（只允许小写字母/数字/下划线）"));
    }
    let target = dir.join(key);
    if !target.exists() {
        return Ok(target);
    }
    fs::remove_dir_all(&target).map_err(|e| format!("删除 {} 失败：{e}", target.display()))?;
    Ok(target)
}

/// 远程目录条目（来自官方 argos index.json，内置两包已在 parse_index 过滤）。
/// PartialEq 供 UI 侧对比两次拉取的目录是否变化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub key: String,     // 如 "en_ja"（from_to）
    pub from: String,
    pub to: String,
    pub version: String, // 如 "1.1"
    pub url: String,     // 完整直链（index.json links[0]，实测即 argos-net）
}

/// 内置两包在官方目录里也存在：装重复包徒增目录项（同 key 目录名冲突），
/// 目录列表直接过滤。
const BUILTIN_KEYS: [&str; 2] = ["zh_en", "en_zh"];

/// 官方 index.json → 目录条目（纯函数，单测直测）：解析 JSON 数组，取
/// from_code/to_code/package_version 与 links[0]（须为 http 直链，实测恒为
/// argos-net）；码不全/缺 links/无直链的条目跳过，内置两包过滤。
/// 整体不是 JSON 数组 → 空表（网络层错误由 fetch_catalog 负责）。
pub(crate) fn parse_index(text: &str) -> Vec<CatalogEntry> {
    let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(text) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|v| {
            let from = v.get("from_code").and_then(|x| x.as_str())?;
            let to = v.get("to_code").and_then(|x| x.as_str())?;
            let url = v
                .get("links")
                .and_then(|l| l.get(0))
                .and_then(|u| u.as_str())
                .unwrap_or_default();
            if from.is_empty() || to.is_empty() || !url.starts_with("http") {
                return None;
            }
            let key = format!("{from}_{to}");
            if BUILTIN_KEYS.contains(&key.as_str()) {
                return None;
            }
            Some(CatalogEntry {
                key,
                from: from.to_string(),
                to: to.to_string(),
                version: v
                    .get("package_version")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                url: url.to_string(),
            })
        })
        .collect()
}

/// 拉官方语言包目录：依次试 mirrors.github_prefixes（空串=直连）拼
/// raw.githubusercontent.com 的 index.json；某前缀连接/读流失败换下一个，
/// 全挂才 Err。索引体约 34KB，agent 的 30s 整体超时足够。
pub fn fetch_catalog(mirrors: &Mirrors) -> Result<Vec<CatalogEntry>, String> {
    const INDEX: &str =
        "https://raw.githubusercontent.com/argosopentech/argospm-index/main/index.json";
    let agent = new_agent();
    let mut errs: Vec<String> = Vec::new();
    for p in &mirrors.github_prefixes {
        let url = format!("{p}{INDEX}");
        let resp = match agent.get(&url).call() {
            Ok(r) => r,
            Err(e) => {
                errs.push(format!("{url}：{e}"));
                continue;
            }
        };
        let mut text = String::new();
        match resp.into_reader().read_to_string(&mut text) {
            Ok(_) => return Ok(parse_index(&text)),
            Err(e) => errs.push(format!("{url}：读流失败：{e}")),
        }
    }
    Err(format!(
        "目录获取失败（已尝试 {} 个源）：{}",
        mirrors.github_prefixes.len(),
        errs.join("；")
    ))
}

/// 安装目录条目：单 zip 直链 → 统一布局，与 install 的 argos 分支复用同一
/// 内核（[`install_zip`]）；远端 zip 大小未知（expect=None）。key/from/to 取
/// entry，manifest source = "argos"、kind = "marian"（argos 包恒为 Marian 家族）。
pub fn install_catalog_entry(
    entry: &CatalogEntry,
    dir: &Path,
    mirrors: &Mirrors,
    progress: ProgressFn<'_>,
) -> Result<PathBuf, String> {
    // mirrors 暂不参与：目录条目的 url 是完整直链（links[0]），与 install 的
    // argos_base 拼接无关。参数保留是为与 install 签名对齐（调用方统一传参，
    // 官方若换 CDN 再启用镜像逻辑）。
    let _ = mirrors;
    let dest = dir.join(&entry.key);
    fs::create_dir_all(&dest).map_err(|e| format!("创建 {} 失败：{e}", dest.display()))?;
    // 已完整 → 幂等返回（manifest 驱动，与 is_complete 同口径）
    if manifest_complete(&dest) {
        return Ok(dest);
    }
    // 清旧成品（可能来自上次失败安装），保留 .part 供断点续传——与 install 同款
    for e in fs::read_dir(&dest).map_err(|e| format!("读 {} 失败：{e}", dest.display()))? {
        let path = match e {
            Ok(e) => e.path(),
            Err(_) => continue,
        };
        if path.extension() != Some(OsStr::new("part")) {
            let _ =
                if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        }
    }
    let agent = new_agent();
    // 进度回调的文件名是 &'static str，目录条目名运行期才有：leak 一次 ~40B
    // （用户手动触发的安装，频率极低；UI 进度条也只消费 downloaded/total）
    let name: &'static str = zip_url_name(&entry.url);
    let files = install_zip(&agent, &entry.url, name, None, dir, &dest, progress)
        .map_err(|e| format!("语言包 {} 安装失败：{e}", entry.key))?;
    write_manifest(
        &dest.join("manifest.json"),
        &entry.key,
        &entry.from,
        &entry.to,
        "argos",
        "marian",
        &files,
    )?;
    Ok(dest)
}

/// URL 末段作进度显示名（先去 query）；取不到退回 "argosmodel"。
fn zip_url_name(url: &str) -> &'static str {
    let no_query = url.split('?').next().unwrap_or(url);
    let name = no_query.rsplit('/').next().unwrap_or_default();
    if name.is_empty() {
        "argosmodel"
    } else {
        Box::leak(name.to_string().into_boxed_str())
    }
}

/// 目录完整性自检：manifest.json 可解析且 files 里每个文件字节数相符。
/// 与 engine::manifest_complete 同语义（那份服务 resolve_dir 兜底，这份服务
/// install_catalog_entry 的幂等判定；两处各自内联，避免跨模块耦合）。
pub(crate) fn manifest_complete(dir: &Path) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("manifest.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(files) = value.get("files").and_then(|f| f.as_array()) else {
        return false;
    };
    if files.is_empty() {
        return false;
    }
    files.iter().all(|f| {
        let name = f.get("name").and_then(|n| n.as_str());
        let bytes = f.get("bytes").and_then(|b| b.as_u64());
        match (name, bytes) {
            (Some(n), Some(b)) => {
                fs::metadata(dir.join(n)).map(|m| m.len() == b).unwrap_or(false)
            }
            _ => false,
        }
    })
}

/// zip 顶层目录名（第一个带 '/' 的条目的首段）；扁平 zip 返回 None。
fn zip_top_dir(pkg: &Path) -> Result<Option<String>, String> {
    let f = File::open(pkg).map_err(|e| format!("打开 {} 失败：{e}", pkg.display()))?;
    let mut a = zip::ZipArchive::new(f).map_err(|e| format!("读取 zip 失败：{e}"))?;
    for i in 0..a.len() {
        if let Ok(e) = a.by_index(i) {
            if let Some((top, _)) = e.name().split_once('/') {
                if !top.is_empty() && top != "." && top != ".." {
                    return Ok(Some(top.to_string()));
                }
            }
        }
    }
    Ok(None)
}

/// 列出目录下实际落盘文件（name, bytes），按名排序保证 manifest 稳定。
fn list_dir_files(dir: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut v: Vec<(String, u64)> = fs::read_dir(dir)
        .map_err(|e| format!("读 {} 失败：{e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let b = e.metadata().map(|m| m.len()).unwrap_or(0);
            (n, b)
        })
        .collect();
    v.sort();
    Ok(v)
}

/// 把 .argosmodel zip（argos 直链下载件或本地导入包）解包成统一布局到 `out`：
/// `model.bin`（全树按文件名定位，不假设层级；其同目录的 config.json /
/// shared_vocabulary.* 一并放平到 out 根），sentencepiece.model 写两份
/// source.spm / target.spm；stanza/、README、metadata.json 不落盘。
/// 校验：model.bin / vocabulary 缺失即 Err（install 层据此换下一源）。
/// 返回包内 metadata.json 的 (from_code, to_code)，读不到置空串。
pub(crate) fn unpack_argos(zip_path: &Path, out: &Path) -> Result<(String, String), String> {
    let f = File::open(zip_path).map_err(|e| format!("打开 {} 失败：{e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| format!("读取 zip 失败：{e}"))?;
    fs::create_dir_all(out).map_err(|e| format!("创建 {} 失败：{e}", out.display()))?;

    // 收集条目（idx, name）；zip 名以 '/' 分层，目录条目以 '/' 结尾
    let entries: Vec<(usize, String)> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| (i, e.name().to_string())))
        .collect();

    // model.bin 所在目录 = 包内模型目录 M（实测布局 translate-xx-1_9/model/）
    let (_, model_name) = entries
        .iter()
        .find(|(_, n)| zip_leaf(n) == Some("model.bin"))
        .ok_or("包内缺 model.bin")?;
    let m_prefix = dir_prefix(model_name);

    // model.bin → out/model.bin
    let model_idx = entries
        .iter()
        .find(|(_, n)| dir_prefix(n) == m_prefix && zip_leaf(n) == Some("model.bin"))
        .map(|(i, _)| *i)
        .ok_or("包内缺 model.bin")?;
    extract_entry(&mut archive, model_idx, &out.join("model.bin"))?;

    // config.json（CT2 加载配置 add_source_bos 等，必须保留）；缺失不致命
    if let Some((idx, _)) = entries
        .iter()
        .find(|(_, n)| dir_prefix(n) == m_prefix && zip_leaf(n) == Some("config.json"))
    {
        extract_entry(&mut archive, *idx, &out.join("config.json"))?;
    }

    // vocabulary：shared_vocabulary.* 或 source_vocabulary.*+target_vocabulary.* 成对；
    // 另包内自带的真实 source.spm/target.spm（ct2 原生包）优先落盘
    let (mut shared, mut src_v, mut tgt_v) = (false, false, false);
    for (idx, name) in &entries {
        if dir_prefix(name) != m_prefix {
            continue;
        }
        let leaf = zip_leaf(name).unwrap_or_default();
        if leaf == "source.spm" || leaf == "target.spm" {
            extract_entry(&mut archive, *idx, &out.join(leaf))?;
        } else if leaf.starts_with("shared_vocabulary.") {
            extract_entry(&mut archive, *idx, &out.join(leaf))?;
            shared = true;
        } else if leaf.starts_with("source_vocabulary.") {
            extract_entry(&mut archive, *idx, &out.join(leaf))?;
            src_v = true;
        } else if leaf.starts_with("target_vocabulary.") {
            extract_entry(&mut archive, *idx, &out.join(leaf))?;
            tgt_v = true;
        }
    }
    if !(shared || (src_v && tgt_v)) {
        return Err("包内缺 vocabulary".to_string());
    }

    // sentencepiece.model（兼容旧式 sentence-piece.model 名）→ source.spm + target.spm
    let spm_idx = entries
        .iter()
        .find(|(_, n)| {
            matches!(zip_leaf(n), Some("sentencepiece.model") | Some("sentence-piece.model"))
        })
        .map(|(i, _)| *i)
        .ok_or("包内缺 sentencepiece.model")?;
    let mut spm = Vec::new();
    {
        let mut e = archive
            .by_index(spm_idx)
            .map_err(|e| format!("读 zip 条目失败：{e}"))?;
        e.read_to_end(&mut spm).map_err(|e| format!("读 sentencepiece.model 失败：{e}"))?;
    }
    // 写两份；上一步若已从包内落盘真实 spm（不覆盖，包内数据优先）
    for name in ["source.spm", "target.spm"] {
        let path = out.join(name);
        if !path.exists() {
            fs::write(&path, &spm)
                .map_err(|e| format!("写 {} 失败：{e}", path.display()))?;
        }
    }

    // from/to：包内 metadata.json 的 from_code/to_code（词表 json 巨大且无此字段，
    // 按文件名精确找 metadata.json）；不落盘
    let meta = entries
        .iter()
        .find(|(_, n)| zip_leaf(n) == Some("metadata.json"))
        .and_then(|(idx, _)| read_json_meta(&mut archive, *idx));
    Ok(meta.unwrap_or_default())
}

/// 把 zip 条目 idx 解到 `to`（目标名由我们指定，条目名不参与拼路径，无穿越风险）。
fn extract_entry(
    archive: &mut zip::ZipArchive<File>,
    idx: usize,
    to: &Path,
) -> Result<(), String> {
    let mut e = archive.by_index(idx).map_err(|e| format!("读 zip 条目失败：{e}"))?;
    let mut out = File::create(to).map_err(|e| format!("写 {} 失败：{e}", to.display()))?;
    std::io::copy(&mut e, &mut out)
        .map_err(|e| format!("解包 {} 失败：{e}", to.display()))?;
    Ok(())
}

/// 读 zip 内小 json（≤1MB）并取 from_code/to_code；任何一步失败返回 None。
fn read_json_meta(archive: &mut zip::ZipArchive<File>, idx: usize) -> Option<(String, String)> {
    let mut e = archive.by_index(idx).ok()?;
    if e.size() > 1 << 20 {
        return None;
    }
    let mut s = String::new();
    e.read_to_string(&mut s).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some((get("from_code"), get("to_code")))
}

/// zip 条目名的文件名部分；目录条目返回 None。
fn zip_leaf(entry_name: &str) -> Option<&str> {
    if entry_name.ends_with('/') {
        return None;
    }
    Path::new(entry_name).file_name().and_then(|f| f.to_str())
}

/// zip 条目名的目录前缀（顶层条目为空串）。
fn dir_prefix(entry_name: &str) -> &str {
    match entry_name.rfind('/') {
        Some(i) => &entry_name[..i],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    //! unpack_argos 是 pub(crate)，集成测试不可见，故在此直接单测解包逻辑。

    use super::*;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    /// 复刻真实 argos 包布局（规格 §6 测试条目）构造最小 zip 包。
    fn build_pkg(path: &Path, with_vocab: bool, with_meta: bool) {
        let f = File::create(path).unwrap();
        let mut w = ZipWriter::new(f);
        let opts =
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let add = |w: &mut ZipWriter<File>, name: &str, data: &[u8]| {
            w.start_file(name, opts).unwrap();
            w.write_all(data).unwrap();
        };
        add(&mut w, "translate-en_zh-1_9/model/model.bin", &[0xABu8; 16]);
        if with_vocab {
            add(&mut w, "translate-en_zh-1_9/model/shared_vocabulary.json", b"vocab");
        }
        add(&mut w, "translate-en_zh-1_9/model/config.json", b"{}");
        add(&mut w, "translate-en_zh-1_9/sentencepiece.model", &[0x5Au8; 16]);
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

    #[test]
    fn unpack_argos_produces_unified_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("fake.argosmodel");
        build_pkg(&pkg, true, true);
        let out = tmp.path().join("out");
        let (from, to) = unpack_argos(&pkg, &out).unwrap();
        assert_eq!((from.as_str(), to.as_str()), ("en", "zh"));
        // 统一布局五文件
        for name in
            ["model.bin", "shared_vocabulary.json", "config.json", "source.spm", "target.spm"]
        {
            assert!(out.join(name).is_file(), "缺 {name}");
        }
        assert_eq!(fs::read(out.join("model.bin")).unwrap(), vec![0xABu8; 16]);
        // sentencepiece.model 复制两份
        assert_eq!(fs::read(out.join("source.spm")).unwrap(), vec![0x5Au8; 16]);
        assert_eq!(fs::read(out.join("target.spm")).unwrap(), vec![0x5Au8; 16]);
        // stanza / metadata.json 不落盘
        assert!(!out.join("stanza").exists());
        assert!(!out.join("metadata.json").exists());
    }

    #[test]
    fn unpack_argos_rejects_missing_vocabulary() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("fake.argosmodel");
        build_pkg(&pkg, false, false);
        let out = tmp.path().join("out");
        let err = unpack_argos(&pkg, &out).unwrap_err();
        assert!(err.contains("vocabulary"), "got: {err}");
    }

    /// 规格 §5 fixture：3 条——正常（en_ja，双链接）、缺 links（跳过）、
    /// 内置 zh_en（过滤）。
    fn index_fixture() -> String {
        r#"[
            {
                "package_version": "1.1",
                "argos_version": "1.1",
                "from_code": "en",
                "from_name": "English",
                "to_code": "ja",
                "to_name": "Japanese",
                "links": [
                    "https://argos-net.com/v1/translate-en_ja-1_1.argosmodel",
                    "ipfs://QmQjHs8ZhCU6WTV9RRHQMXgy4cDwyjvLUZ4bBp6J9GMCRb"
                ],
                "code": "translate-en_ja"
            },
            {
                "package_version": "1.0",
                "from_code": "xx",
                "to_code": "yy",
                "links": [],
                "code": "translate-xx_yy"
            },
            {
                "package_version": "1.9",
                "from_code": "zh",
                "to_code": "en",
                "links": ["https://argos-net.com/v1/translate-zh_en-1_9.argosmodel"],
                "code": "translate-zh_en"
            }
        ]"#
        .to_string()
    }

    #[test]
    fn parse_index_keeps_normal_filters_builtin_and_bad() {
        let list = parse_index(&index_fixture());
        assert_eq!(list.len(), 1, "只保留 en_ja：缺 links 跳过、zh_en 过滤");
        let e = &list[0];
        assert_eq!(e.key, "en_ja");
        assert_eq!((e.from.as_str(), e.to.as_str()), ("en", "ja"));
        assert_eq!(e.version, "1.1");
        // 直链取 links[0]，ipfs 链接不参与
        assert_eq!(e.url, "https://argos-net.com/v1/translate-en_ja-1_1.argosmodel");
    }

    #[test]
    fn parse_index_garbage_input_yields_empty() {
        assert!(parse_index("not json").is_empty());
        assert!(parse_index(r#"{"a":1}"#).is_empty()); // 非数组
        assert!(parse_index("[]").is_empty());
    }

    #[test]
    fn hf_dest_name_uses_alias() {
        let plain = ModelFile { name: "model.bin", bytes: 1, as_: None };
        let alias = ModelFile { name: "sentencepiece.bpe.model", bytes: 2, as_: Some("target.spm") };
        assert_eq!(hf_dest_name(&plain), "model.bin");
        assert_eq!(hf_dest_name(&alias), "target.spm");
    }
}
