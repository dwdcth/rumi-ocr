//! rumi-translate-cli —— 离线/在线翻译命令行（工人 C 按 `docs/translate-spec-engine.md` §2 实现）。
//!
//! 手写参数解析（工作区无 clap）。短命进程：离线引擎不走 [`rumi_translate::engine::shared`]，
//! 直接 `OfflineEngine::new(models_dir, Compute::Int8)`，`--models-dir` 才能生效。

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::Instant;

use rumi_translate::download::{self, Mirrors};
use rumi_translate::engine::OfflineEngine;
use rumi_translate::{lang, postfix, registry, translate, Compute, Engine};

const HELP: &str = r#"rumi-translate-cli —— 离线/在线翻译命令行

用法：
  rumi-translate-cli [选项] [文本]        无文本参数且 stdin 非 tty 时读 stdin
选项：
  --from <lang>    源语言码（默认 en）
  --to <lang>      目标语言码（默认 zh）
  --engine <name>  offline(默认) | google | microsoft | deepl | deeplx | openai
  --api-key <k>    deepl/openai 密钥
  --pro            deepl 专业版端点
  --url <u>        deeplx 服务地址 / openai base_url
  --model <m>      openai 模型名（默认 gpt-4o-mini）
  --file <path>    从文件读文本（UTF-8）
  --no-polish      关闭 zh 译文标点润色
  --json           输出 {"engine":"...","elapsed_ms":N,"texts":[...]}
  --install <key>  安装离线语言包（zh_en / en_zh / nllb_600m），进度打 stderr，装完退出
  --remove <key>   删除已安装语言包（删 dir/<key>/ 整目录），完成后退出
  --catalog        拉取官方语言包目录列表（key/方向/版本），纯列表不装
  --list-models    列出内置模型与安装状态后退出
  --models-dir <d> 覆盖模型目录
  --help           本帮助
"#;

struct Args {
    from: String,
    to: String,
    engine: String,
    api_key: Option<String>,
    pro: bool,
    url: Option<String>,
    model: String,
    file: Option<String>,
    no_polish: bool,
    json: bool,
    install: Option<String>,
    remove: Option<String>,
    catalog: bool,
    list_models: bool,
    models_dir: Option<String>,
    text: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            from: "en".to_string(),
            to: "zh".to_string(),
            engine: "offline".to_string(),
            api_key: None,
            pro: false,
            url: None,
            model: "gpt-4o-mini".to_string(),
            file: None,
            no_polish: false,
            json: false,
            install: None,
            remove: None,
            catalog: false,
            list_models: false,
            models_dir: None,
            text: None,
        }
    }
}

fn parse_args() -> Args {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                exit(0);
            }
            "--from" => a.from = next_value(&mut it, &arg),
            "--to" => a.to = next_value(&mut it, &arg),
            "--engine" => a.engine = next_value(&mut it, &arg),
            "--api-key" => a.api_key = Some(next_value(&mut it, &arg)),
            "--pro" => a.pro = true,
            "--url" => a.url = Some(next_value(&mut it, &arg)),
            "--model" => a.model = next_value(&mut it, &arg),
            "--file" => a.file = Some(next_value(&mut it, &arg)),
            "--no-polish" => a.no_polish = true,
            "--json" => a.json = true,
            "--install" => a.install = Some(next_value(&mut it, &arg)),
            "--remove" => a.remove = Some(next_value(&mut it, &arg)),
            "--catalog" => a.catalog = true,
            "--list-models" => a.list_models = true,
            "--models-dir" => a.models_dir = Some(next_value(&mut it, &arg)),
            other if other.starts_with('-') => {
                eprintln!("未知选项：{other}（--help 查看用法）");
                exit(1);
            }
            text => {
                if a.text.is_some() {
                    eprintln!("只接受一个文本参数（多余：{text:?}）");
                    exit(1);
                }
                a.text = Some(text.to_string());
            }
        }
    }
    a
}

/// 取选项的值参数；缺失则报错退出。
fn next_value(it: &mut impl Iterator<Item = String>, opt: &str) -> String {
    match it.next() {
        Some(v) => v,
        None => {
            eprintln!("选项 {opt} 缺少参数（--help 查看用法）");
            exit(1);
        }
    }
}

fn main() {
    let args = parse_args();
    let models_dir: PathBuf = args
        .models_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(rumi_translate::models_dir);

    // --install / --remove / --catalog / --list-models 是管理子命令：做完即退，不进翻译流程
    if let Some(key) = &args.install {
        install_cmd(key, &models_dir);
        return;
    }
    if let Some(key) = &args.remove {
        remove_cmd(key, &models_dir);
        return;
    }
    if args.catalog {
        catalog_cmd();
        return;
    }
    if args.list_models {
        list_models(&models_dir);
        return;
    }

    // 语言码规范化：facade 与离线引擎都按内部两位码工作
    let from = lang::normalize(&args.from);
    let to = lang::normalize(&args.to);
    let polish = !args.no_polish;
    let text = read_text(&args);
    let engine = build_engine(&args);

    let t0 = Instant::now();
    let result: Result<String, String> = match &engine {
        None => {
            // CLI 短命进程：不用 shared()（其 models_dir 固化于进程级单例），直接 new
            let eng = OfflineEngine::new(models_dir.clone(), Compute::Int8);
            eng.translate(&text, &from, &to).map_err(|e| e.to_string())
        }
        Some(e) => {
            // 润色统一由 CLI 末尾做（facade 传 false，避免双重润色）
            translate(std::slice::from_ref(&text), &from, &to, e, false)
                .map(|mut v| v.pop().unwrap_or_default())
        }
    };
    let elapsed_ms = t0.elapsed().as_millis() as u64;

    match result {
        Ok(out) => {
            let out = if polish && to == "zh" {
                postfix::polish_zh(&out)
            } else {
                out
            };
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "engine": args.engine,
                        "elapsed_ms": elapsed_ms,
                        "texts": [out],
                    })
                );
            } else {
                println!("{out}");
            }
        }
        Err(e) => {
            eprintln!("{e}");
            exit(1);
        }
    }
}

/// --engine 名 → facade 引擎枚举；offline 返回 None（CLI 直接 new 离线引擎）。
fn build_engine(args: &Args) -> Option<Engine> {
    match args.engine.as_str() {
        "offline" => None,
        "google" => Some(Engine::Google),
        "microsoft" => Some(Engine::Microsoft),
        "deepl" => Some(Engine::DeepL {
            api_key: require_value(&args.api_key, "deepl 需要 --api-key"),
            pro: args.pro,
        }),
        "deeplx" => Some(Engine::DeepLX {
            url: require_value(&args.url, "deeplx 需要 --url"),
        }),
        "openai" => Some(Engine::OpenAI {
            // base_url 缺省走 OpenAI 官方；本地/兼容服务需显式 --url
            base_url: args
                .url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            api_key: args.api_key.clone().unwrap_or_default(),
            model: args.model.clone(),
        }),
        other => {
            eprintln!("未知引擎：{other}（可选 offline/google/microsoft/deepl/deeplx/openai）");
            exit(1);
        }
    }
}

fn require_value(value: &Option<String>, err: &str) -> String {
    match value {
        Some(v) => v.clone(),
        None => {
            eprintln!("{err}");
            exit(1);
        }
    }
}

/// 文本来源优先级：位置参数 > --file > stdin（stdin 是 tty 且无文本则报错）。
fn read_text(args: &Args) -> String {
    if let Some(t) = &args.text {
        return t.clone();
    }
    if let Some(f) = &args.file {
        return match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("读取文件 {f} 失败：{e}");
                exit(1);
            }
        };
    }
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("读取 stdin 失败：{e}");
            exit(1);
        }
        return buf;
    }
    eprintln!("未提供文本：传位置参数、--file <path>，或经管道送 stdin（--help 查看用法）");
    exit(1);
}

/// 方向展示：NLLB 行显示多语言标识，其余按 from→to（--list-models / --install 共用）。
fn dir_label(spec: &registry::ModelSpec) -> String {
    if spec.kind == registry::ModelKind::Nllb {
        "多语言(zh↔200语言)".to_string()
    } else {
        format!("{}→{}", spec.from, spec.to)
    }
}

/// --install：装语言包到 models_dir/<key>，进度打 stderr，装完（或失败）退出。
fn install_cmd(key: &str, dir: &Path) {
    let Some(spec) = registry::by_key(key) else {
        eprintln!("未知语言包：{key}（可选 zh_en / en_zh / nllb_600m，--list-models 查看清单）");
        exit(1);
    };
    println!(
        "开始安装 {}（{}，约 {} MB）……",
        spec.key,
        dir_label(spec),
        spec.size_mb
    );
    let mirrors = Mirrors::default();
    let progress = |p: download::Progress| {
        eprintln!(
            "{} {}/{}",
            p.file,
            p.downloaded,
            p.total.map(|t| t.to_string()).unwrap_or("?".to_string())
        );
    };
    match download::install(spec, dir, &mirrors, &progress) {
        Ok(path) => println!("安装完成：{}", path.display()),
        Err(e) => {
            eprintln!("安装失败：{e}");
            exit(1);
        }
    }
}

/// --remove：删语言包目录（download::remove 防 key 穿越、幂等），完即退。
/// 短命进程没有引擎缓存可摘，无需 unload。
fn remove_cmd(key: &str, dir: &Path) {
    match download::remove(key, dir) {
        Ok(path) => println!("已删除：{}", path.display()),
        Err(e) => {
            eprintln!("删除失败：{e}");
            exit(1);
        }
    }
}

/// --catalog：拉官方语言包目录列表（key/方向/版本），纯列表不装；失败退出 1。
fn catalog_cmd() {
    match download::fetch_catalog(&Mirrors::default()) {
        Ok(list) => {
            if list.is_empty() {
                println!("（目录为空：官方索引未返回可用条目）");
                return;
            }
            println!("{}\t{}\t{}", "key", "方向", "版本");
            for e in &list {
                println!("{}\t{}→{}\t{}", e.key, e.from, e.to, e.version);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            exit(1);
        }
    }
}

/// --list-models：逐行输出内置模型的 key/方向/约数大小/安装状态。
fn list_models(dir: &Path) {
    if registry::MODELS.is_empty() {
        println!("（注册表为空：内置模型清单尚未收录）");
        return;
    }
    for spec in registry::MODELS {
        // is_complete 接收模型根目录、内部自行 join(key)，此处不能重复拼
        let done = download::is_complete(spec, dir);
        println!(
            "{}\t{}\t约{}MB\t{}",
            spec.key,
            dir_label(spec),
            spec.size_mb,
            if done { "已安装" } else { "未安装" }
        );
    }
}
