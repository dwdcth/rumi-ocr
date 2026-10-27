//! 配置读取：`~/.config/rumi/config.toml`（M3 §6 子集，M3b 的 UI 设置页是写入方）。
//!
//! 热加载策略：**每次请求现读现解析**。文件只有几行 toml，开销可忽略，
//! 换来"改配置立即生效、无需重启"（任务书要求）。
//!
//! 支持平铺与 `[server]` 分节两种写法，分节优先：
//! ```toml
//! engine = "native"          # 或 "plugin:demo_AbaOCR"
//! port = 1224                # 可省，默认 1224
//! token = ""                 # 可省，默认关鉴权
//! [server]                   # 可选分节（覆盖平铺同名项）
//! port = 1224
//! token = ""
//! ```
//! 文件不存在 / 解析失败 → 全默认（native、1224、无 token）。

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 引擎选择（config.toml 的 `engine` 字段语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineChoice {
    /// oar-ocr 进程内引擎
    Native,
    /// Umi 插件协议引擎，值为插件名（插件目录名）
    Plugin(String),
}

impl EngineChoice {
    /// 解析 `engine` 字符串；空/未设置/未知写法一律回退 native。
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        match s.strip_prefix("plugin:") {
            Some(name) if !name.trim().is_empty() => EngineChoice::Plugin(name.trim().to_string()),
            Some(_) => EngineChoice::Native, // "plugin:" 后为空 → native
            None => match s {
                "native" | "" => EngineChoice::Native,
                other => {
                    eprintln!("rumi-server：未知 engine 配置 {other:?}，按 native 处理");
                    EngineChoice::Native
                }
            },
        }
    }

    /// 展示用标签（状态接口返回原样字符串）。
    pub fn label(&self) -> String {
        match self {
            EngineChoice::Native => "native".into(),
            EngineChoice::Plugin(name) => format!("plugin:{name}"),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawServer {
    port: Option<u16>,
    token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawConfig {
    engine: Option<String>,
    port: Option<u16>,
    token: Option<String>,
    server: Option<RawServer>,
    /// `[plugins.<名>.global]` / `[plugins.<名>.local]`（M3b 设置页写入的插件选项）
    plugins: Option<std::collections::BTreeMap<String, RawPluginOpts>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawPluginOpts {
    global: Option<toml::Value>,
    local: Option<toml::Value>,
}

/// 解析后的应用配置（None 字段 = 未配置，取默认）。
#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    pub engine: Option<String>,
    pub port: Option<u16>,
    pub token: Option<String>,
    plugins: std::collections::BTreeMap<String, (Option<serde_json::Value>, Option<serde_json::Value>)>,
}

impl AppConfig {
    /// 读取配置文件；文件不存在或损坏 → [`AppConfig::default`]（任务书：文件不存在当 native）。
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let raw: RawConfig = match toml::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("rumi-server：配置 {} 解析失败（{}），按默认处理", path.display(), e);
                return Self::default();
            }
        };
        let mut cfg = Self {
            engine: raw.engine,
            port: raw.port,
            token: raw.token,
            plugins: raw
                .plugins
                .unwrap_or_default()
                .into_iter()
                .map(|(name, opts)| {
                    let to_json = |v: Option<toml::Value>| {
                        v.and_then(|v| serde_json::to_value(v).ok())
                    };
                    (name, (to_json(opts.global), to_json(opts.local)))
                })
                .collect(),
        };
        // [server] 分节覆盖平铺同名项
        if let Some(s) = raw.server {
            if s.port.is_some() {
                cfg.port = s.port;
            }
            if s.token.is_some() {
                cfg.token = s.token;
            }
        }
        cfg
    }

    /// 引擎选择（未配置 → native）。
    pub fn engine(&self) -> EngineChoice {
        self.engine
            .as_deref()
            .map(EngineChoice::parse)
            .unwrap_or(EngineChoice::Native)
    }

    /// HTTP 端口（默认 1224，Umi 同款）。
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(1224)
    }

    /// 鉴权 token（空串 / 未配置 = 关闭鉴权）。
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref().filter(|t| !t.is_empty())
    }

    /// 用户在设置页保存的插件全局选项（`[plugins.<名>.global]`）；无则 None。
    pub fn plugin_global(&self, name: &str) -> Option<&serde_json::Value> {
        self.plugins.get(name).and_then(|(g, _)| g.as_ref())
    }

    /// 用户保存的插件局部选项（`[plugins.<名>.local]`）。
    pub fn plugin_local(&self, name: &str) -> Option<&serde_json::Value> {
        self.plugins.get(name).and_then(|(_, l)| l.as_ref())
    }
}

/// 默认配置路径：`$HOME/.config/rumi/config.toml`；无 HOME 时返回 None。
pub fn default_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("rumi").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_choice_parsing() {
        assert_eq!(EngineChoice::parse("native"), EngineChoice::Native);
        assert_eq!(EngineChoice::parse(""), EngineChoice::Native);
        assert_eq!(EngineChoice::parse("plugin:demo_AbaOCR"), EngineChoice::Plugin("demo_AbaOCR".into()));
        assert_eq!(EngineChoice::parse(" plugin:x "), EngineChoice::Plugin("x".into()));
        assert_eq!(EngineChoice::parse("plugin:"), EngineChoice::Native);
        assert_eq!(EngineChoice::parse("bogus"), EngineChoice::Native);
        assert_eq!(EngineChoice::Native.label(), "native");
        assert_eq!(EngineChoice::Plugin("p".into()).label(), "plugin:p");
    }

    #[test]
    fn load_missing_file_defaults_to_native() {
        let cfg = AppConfig::load(Path::new("/nonexistent/rumi/config.toml"));
        assert_eq!(cfg.engine(), EngineChoice::Native);
        assert_eq!(cfg.port(), 1224);
        assert_eq!(cfg.token(), None);
    }

    #[test]
    fn load_flat_and_nested_sections() {
        let dir = std::env::temp_dir().join(format!("rumi-cfgtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let flat = dir.join("flat.toml");
        std::fs::write(&flat, "engine = \"plugin:demo_AbaOCR\"\nport = 1225\n").unwrap();
        let cfg = AppConfig::load(&flat);
        assert_eq!(cfg.engine(), EngineChoice::Plugin("demo_AbaOCR".into()));
        assert_eq!(cfg.port(), 1225);

        // [server] 分节覆盖平铺项
        let nested = dir.join("nested.toml");
        std::fs::write(
            &nested,
            "engine = \"native\"\nport = 9\n\n[server]\nport = 12345\ntoken = \"abc\"\n",
        )
        .unwrap();
        let cfg = AppConfig::load(&nested);
        assert_eq!(cfg.engine(), EngineChoice::Native);
        assert_eq!(cfg.port(), 12345);
        assert_eq!(cfg.token(), Some("abc"));

        // 损坏文件 → 默认
        let broken = dir.join("broken.toml");
        std::fs::write(&broken, "???not toml[[[").unwrap();
        let cfg = AppConfig::load(&broken);
        assert_eq!(cfg.port(), 1224);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
