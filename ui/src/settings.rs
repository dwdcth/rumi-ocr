//! settings —— `~/.config/rumi/config.toml` 设置持久化（M3 任务书 §6）。
//!
//! 顶层键与 M3a（HTTP server crate）共享：`hotkey` / `engine` / `http_port` / `lang` / `theme`；
//! 插件每插件 options 存在 `[plugins.<名字>.global]` / `[plugins.<名字>.local]` 表。
//! 插件 options 值统一以字符串持久化（TOML 无 null/嵌套数组约束太脆），
//! 发给 worker init/start 时按 schema 叶子类型回调数值（见 [`coerce`]）。
//!
//! 进程内共享：[`init`] 装进 static RwLock；热键/托盘/识别线程读 [`get`]，
//! UI 变更走 [`update`]（改内存 + 写盘）。
//!
//! CLI 级验证：`cargo test -p rumi-ui settings`（roundtrip + 默认值容错）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

pub const DEFAULT_HOTKEY: &str = "alt+q";
pub const DEFAULT_PORT: u16 = 1224;
pub const DEFAULT_LANG: &str = "zh_CN";
/// 默认浅色（Wave3：首次运行不再深色；已存配置的用户值不受影响）
pub const DEFAULT_THEME: &str = "light";
/// 无边框窗口默认开（Wave3：自定义标题栏体验；出问题可在设置页关闭并重启回退）
pub const DEFAULT_FRAMELESS: bool = true;
/// 关闭按钮行为默认值（体验批次）：默认最小化到托盘，与 Umi-OCR 惯例一致；
/// false = 点关闭直接退出。
pub const DEFAULT_CLOSE_TO_TRAY: bool = true;
/// 选区方式默认"跟随后端"（M 波：region_native=false 的后端自动走内置冻结帧选择器）
pub const DEFAULT_PICKER_MODE: &str = "auto";
/// 排版解析默认策略（P2 #11：by_line / merge_para / natural_para）
pub const DEFAULT_LAYOUT: &str = "by_line";
/// 引擎取值："native" 或 "plugin:<名字>"（见 [`engine_plugin`]）
pub const ENGINE_NATIVE: &str = "native";

/// 忽略区域个数上限（P0 防水印）：超出在编辑入口拒绝（[`push_ignore_region`]）。
pub const MAX_IGNORE_REGIONS: usize = 16;

/// 窗口几何（物理像素）：关闭窗口 / 托盘退出时保存，启动时恢复（P0 #4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 单个插件的持久化 options（键 → 字符串值）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginOptions {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub global: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub local: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 全局截图 OCR 热键，global-hotkey 解析格式（如 "alt+q"）。改后重启生效（M3 验收 #5）。
    pub hotkey: String,
    /// "native" | "plugin:<名字>"
    pub engine: String,
    /// Umi 兼容 HTTP 端口（server crate 读同一文件）
    pub http_port: u16,
    /// 插件 worker --lang（联动 plugin_i18n setLangCode）；native 引擎忽略
    pub lang: String,
    /// "dark" | "light"
    pub theme: String,
    /// 每插件 options：插件名 → global/local 键值
    pub plugins: BTreeMap<String, PluginOptions>,
    /// 上次关闭时的窗口几何（None = 用内置默认）
    pub window: Option<WindowState>,
    /// 排版解析策略（P2 #11）："by_line" | "merge_para" | "natural_para"
    pub layout: String,
    /// 忽略规则（P2 #12）：每条一个正则，命中整行的识别行被剔除。
    /// 坏正则在识别管线里被跳过（ocr crate PostRules），设置页保存时给行内提示。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_rules: Vec<String>,
    /// 忽略区域（P0 坐标版防水印）：`[x0, y0, x1, y1]` 为**图像归一化坐标**
    /// ∈ 0..1，相对整图左上原点。注意：这与 Umi-OCR 的"屏幕坐标区域"语义
    /// 不同——我们的作用域是被识别图像本身，因此粘贴图同样生效、跨显示器
    /// 不漂移。识别前矩形内置白（见 ocr_flow::apply_ignore_regions）；
    /// 上限 [`MAX_IGNORE_REGIONS`] 个，超出在识别页编辑入口拒绝。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_regions: Vec<[f32; 4]>,
    /// 识别完成桌面通知（P1 #10，默认关）
    pub notify: bool,
    /// 托盘"暂停热键"切换项的持久化状态（P1 #10；True = 热键已暂停）
    pub hotkey_paused: bool,
    /// 公式模式（J2）：识别页开关，native 引擎分流到结构化管线
    /// （版面检测 + 公式行 → LaTeX，见 ocr_flow::recognize_frame）。默认关。
    pub formula_mode: bool,
    /// 识别完成提示音（J2）：合成 880→1320Hz 两短音（无资产文件），
    /// rodio 播放；无音频设备时静默。默认关。
    pub sound: bool,
    /// 无边框窗口（Wave3）：true = 去掉系统装饰，用自定义 TitleBar（拖拽区 +
    /// 最小化 + 隐藏到托盘）。与热键/端口同口径：**改动需重启应用生效**。
    /// 旧配置文件无该字段 → serde container default 补 [`DEFAULT_FRAMELESS`]。
    pub frameless: bool,
    /// 关闭按钮行为（体验批次）：true = 最小化到托盘（默认，与 Umi-OCR 一致）；
    /// false = 直接退出程序。启动时与点击时动态读取——设置页改完即生效，无需重启。
    pub close_to_tray: bool,
    /// 选区方式（M 波冻结帧选择器）："auto"（跟随后端：caps.region_native=false
    /// 自动走内置冻结帧选择器）| "native"（强制原生选区）| "builtin"（强制内置
    /// 冻结帧拖选，Wayland 用户也可选它统一交互）。每次捕获时读取，保存即生效。
    pub picker_mode: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.into(),
            engine: ENGINE_NATIVE.into(),
            http_port: DEFAULT_PORT,
            lang: DEFAULT_LANG.into(),
            theme: DEFAULT_THEME.into(),
            plugins: BTreeMap::new(),
            window: None,
            layout: DEFAULT_LAYOUT.into(),
            ignore_rules: Vec::new(),
            ignore_regions: Vec::new(),
            notify: false,
            hotkey_paused: false,
            formula_mode: false,
            sound: false,
            frameless: DEFAULT_FRAMELESS,
            close_to_tray: DEFAULT_CLOSE_TO_TRAY,
            picker_mode: DEFAULT_PICKER_MODE.into(),
        }
    }
}

impl Settings {
    /// engine == "plugin:<名字>" 时返回 Some(名字)
    pub fn engine_plugin(&self) -> Option<&str> {
        self.engine.strip_prefix("plugin:").filter(|s| !s.is_empty())
    }
}

/// 选区方式（M 波）：解析后的三档枚举，识别流程按它决定走原生选区还是
/// 内置冻结帧选择器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    /// 跟随后端：caps.region_native=false 的后端（xcap）自动走内置选择器
    Auto,
    /// 强制原生选区（后端不支持时错误如实上抛）
    Native,
    /// 强制内置冻结帧选择器（Wayland 用户也可选，统一交互体验）
    Builtin,
}

/// 存储串 → 选区方式（纯函数可测）；未知串回退 [`DEFAULT_PICKER_MODE`] 语义。
pub fn parse_picker_mode(s: &str) -> PickerMode {
    match s {
        "native" => PickerMode::Native,
        "builtin" => PickerMode::Builtin,
        _ => PickerMode::Auto,
    }
}

/// 配置文件路径。`RUMI_CONFIG_DIR` 优先（测试/打包），否则系统配置目录。
pub fn config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUMI_CONFIG_DIR") {
        return PathBuf::from(dir).join("config.toml");
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("rumi")
        .join("config.toml")
}

/// 从磁盘读配置；文件缺失 → 默认值；解析失败 → 默认值 + 警告（UI 设置页显示）。
pub fn load() -> (Settings, Option<String>) {
    load_from(&config_path())
}

/// [`load`] 的路径显式版（测试用，避免进程级 env 竞争）。
pub fn load_from(path: &std::path::Path) -> (Settings, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (Settings::default(), None); // 首次运行无文件，不算错误
    };
    match toml::from_str::<Settings>(&text) {
        Ok(s) => (s, None),
        Err(e) => (
            Settings::default(),
            Some(format!("配置解析失败（已回退默认值）：{e}")),
        ),
    }
}

/// 写盘（先建父目录）。原子性：写临时文件后 rename。
pub fn save(s: &Settings) -> Result<(), String> {
    save_to(&config_path(), s)
}

/// [`save`] 的路径显式版（测试用）。
pub fn save_to(path: &std::path::Path, s: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录 {}：{e}", parent.display()))?;
    }
    let body = toml::to_string_pretty(s).map_err(|e| format!("配置序列化：{e}"))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("写 {}：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename 配置文件：{e}"))
}

static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();
/// load() 的警告留给设置页展示
static LOAD_WARNING: OnceLock<Option<String>> = OnceLock::new();

fn cell() -> &'static RwLock<Settings> {
    SETTINGS.get_or_init(|| RwLock::new(Settings::default()))
}

/// 启动时装载：main() 在 launch 前调用一次。返回解析警告（None = 无）。
pub fn init() -> Option<String> {
    let (s, warn) = load();
    *cell().write().unwrap_or_else(|p| p.into_inner()) = s;
    LOAD_WARNING.get_or_init(|| warn).clone()
}

/// 设置页展示用
pub fn load_warning() -> Option<String> {
    LOAD_WARNING.get().cloned().flatten()
}

/// 当前设置快照（克隆；热键/托盘/识别线程均可调用）
pub fn get() -> Settings {
    cell()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// 变更：改内存 + 写盘。f 返回的 Err 只放弃写盘（内存已改，调用方自行提示）。
pub fn update(f: impl FnOnce(&mut Settings) -> Result<(), String>) -> Result<(), String> {
    let mut guard = cell().write().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)?;
    save(&guard)
}

/// 忽略规则文本域 → 规则列表（P2 #12，纯函数可测）：
/// 按行切分、去首尾空白、丢弃空行。
pub fn parse_ignore_textarea(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// 校验忽略规则列表，返回其中编译失败的原文（坏正则行内提示用；
/// 管线里坏正则会被 ocr crate 的 PostRules 跳过，不整体失败）。
pub fn bad_ignore_rules(rules: &[String]) -> Vec<String> {
    rules
        .iter()
        .filter(|r| regex::Regex::new(r.as_str()).is_err())
        .cloned()
        .collect()
}

/// 新增一个忽略区域（P0，纯函数可测）：超 [`MAX_IGNORE_REGIONS`] 上限拒绝
/// 并返回提示（识别页 status 行展示）；成功返回追加后的新列表。
pub fn push_ignore_region(
    regions: Vec<[f32; 4]>,
    region: [f32; 4],
) -> Result<Vec<[f32; 4]>, String> {
    if regions.len() >= MAX_IGNORE_REGIONS {
        return Err(format!("忽略区域最多 {} 个，请先删除再添加", MAX_IGNORE_REGIONS));
    }
    let mut out = regions;
    out.push(region);
    Ok(out)
}

/// 把持久化的字符串值按 schema 叶子类型回调成 worker 需要的 JSON 值：
/// - 存储值为空串（= 未设置）且有 schema default → 用 schema default 原值
///   （Umi 语义：init/start 收到的是"填满默认值"的完整 dict，缺键会 KeyError）
/// - 叶子 default 是数字且存储值可解析 → 数值；可解析 bool → bool；其余原样字符串
pub fn coerce(stored: &str, schema_leaf: &serde_json::Value) -> serde_json::Value {
    let default = schema_leaf.get("default");
    if stored.is_empty() {
        if let Some(d) = default {
            return d.clone();
        }
        // optionsList 无 default → Umi 惯例取第一项
        if let Some(first) = schema_leaf
            .get("optionsList")
            .and_then(|v| v.as_array())
            .and_then(|pairs| pairs.first())
            .and_then(|pair| pair.as_array())
            .and_then(|items| items.first())
            .cloned()
        {
            return first;
        }
        return serde_json::json!("");
    }
    if stored.trim().parse::<f64>().is_ok() {
        if let Some(d) = default.filter(|d| d.is_number()) {
            if d.is_i64() || d.is_u64() {
                if let Ok(i) = stored.trim().parse::<i64>() {
                    return serde_json::json!(i);
                }
            }
            if let Ok(f) = stored.trim().parse::<f64>() {
                return serde_json::json!(f);
            }
        }
    }
    if stored == "true" || stored == "false" {
        if matches!(default, Some(d) if d.is_boolean()) {
            return serde_json::json!(stored == "true");
        }
    }
    serde_json::json!(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 并行测试安全的独立临时目录（进程 id + 原子计数）
    fn test_dir(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("{tag}-{}-{n}", std::process::id()))
    }

    #[test]
    fn default_roundtrip() {
        let dir = test_dir("rumi-cfg-test");
        let path = dir.join("config.toml");

        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert_eq!(s, Settings::default());
        assert_eq!(s.hotkey, DEFAULT_HOTKEY);
        assert_eq!(s.http_port, DEFAULT_PORT);

        let mut s2 = s.clone();
        s2.engine = "plugin:demo_AbaOCR".into();
        let mut po = PluginOptions::default();
        po.global.insert("api_key".into(), "sk-1".into());
        po.local.insert("language".into(), "en_US".into());
        s2.plugins.insert("demo_AbaOCR".into(), po);
        save_to(&path, &s2).unwrap();

        let (s3, w3) = load_from(&path);
        assert!(w3.is_none());
        assert_eq!(s3.engine, "plugin:demo_AbaOCR");
        assert_eq!(s3.engine_plugin(), Some("demo_AbaOCR"));
        assert_eq!(s3.plugins["demo_AbaOCR"].global["api_key"], "sk-1");
        assert_eq!(s3.plugins["demo_AbaOCR"].local["language"], "en_US");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn window_state_roundtrip() {
        let dir = test_dir("rumi-cfg-win");
        let path = dir.join("config.toml");

        // 缺省：无 window 段 → None（旧配置文件兼容）
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert_eq!(s.window, None);

        // 写入 → 读回逐字段一致
        let mut s2 = Settings::default();
        s2.window = Some(WindowState { x: -8, y: 40, w: 1024, h: 768 });
        save_to(&path, &s2).unwrap();

        let (s3, w3) = load_from(&path);
        assert!(w3.is_none());
        assert_eq!(
            s3.window,
            Some(WindowState { x: -8, y: 40, w: 1024, h: 768 })
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn broken_file_falls_back() {
        let dir = test_dir("rumi-cfg-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "hotkey = 12345\nhttp_port = \"x\"").unwrap();
        let (_s, warn) = load_from(&path);
        assert!(warn.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn coerce_by_schema_leaf() {
        // 数字叶子 → 数值
        let leaf = serde_json::json!({"title": "阈值", "default": 0.6});
        assert_eq!(coerce("0.8", &leaf), serde_json::json!(0.8));
        // 文本叶子即使长得像数字也保持字符串
        let leaf = serde_json::json!({"title": "密钥", "default": ""});
        assert_eq!(coerce("12345", &leaf), serde_json::json!("12345"));
        // bool 叶子
        let leaf = serde_json::json!({"default": true});
        assert_eq!(coerce("false", &leaf), serde_json::json!(false));
    }

    #[test]
    fn ignore_textarea_parses_and_drops_blank_lines() {
        let rules = parse_ignore_textarea(" ^水印$ \n\n  \nhttps?://\r\n\t\nfooter.*");
        assert_eq!(rules, vec!["^水印$", "https?://", "footer.*"]);
        assert!(parse_ignore_textarea("   \n").is_empty());
    }

    #[test]
    fn bad_ignore_rules_reports_only_broken_ones() {
        let rules = parse_ignore_textarea("([unclosed\nSECRET\\d+");
        assert_eq!(bad_ignore_rules(&rules), vec!["([unclosed"]);
        assert!(bad_ignore_rules(&parse_ignore_textarea("^ok$")).is_empty());
    }

    #[test]
    fn new_settings_fields_roundtrip_and_default() {
        let dir = test_dir("rumi-cfg-wave2");
        let path = dir.join("config.toml");
        // 旧配置文件（无新字段）→ 全部落默认值
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert_eq!(s.layout, DEFAULT_LAYOUT);
        assert!(!s.notify && !s.hotkey_paused && s.ignore_rules.is_empty());

        // 写入 → 读回
        let mut s2 = Settings::default();
        s2.layout = "natural_para".into();
        s2.ignore_rules = vec!["^水印$".into(), "SECRET".into()];
        s2.notify = true;
        s2.hotkey_paused = true;
        save_to(&path, &s2).unwrap();
        let (s3, w3) = load_from(&path);
        assert!(w3.is_none());
        assert_eq!(s3.layout, "natural_para");
        assert_eq!(s3.ignore_rules, vec!["^水印$", "SECRET"]);
        assert!(s3.notify && s3.hotkey_paused);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 忽略区域（P0）：旧配置无该字段 → 空；写入 → 读回逐条一致
    #[test]
    fn ignore_regions_roundtrip_and_default() {
        let dir = test_dir("rumi-cfg-ig");
        let path = dir.join("config.toml");
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert!(s.ignore_regions.is_empty());

        let mut s2 = Settings::default();
        s2.ignore_regions = vec![[0.25, 0.5, 0.75, 1.0], [0.0, 0.125, 0.5, 0.875]];
        save_to(&path, &s2).unwrap();
        let (s3, w3) = load_from(&path);
        assert!(w3.is_none());
        assert_eq!(s3.ignore_regions.len(), 2);
        assert_eq!(s3.ignore_regions[0], [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(s3.ignore_regions[1], [0.0, 0.125, 0.5, 0.875]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 忽略区域上限：填满 16 个成功，第 17 个拒绝且原列表不变
    #[test]
    fn ignore_regions_limit_rejected() {
        let mut v: Vec<[f32; 4]> = Vec::new();
        for i in 0..MAX_IGNORE_REGIONS {
            let r = [i as f32 / 100.0, 0.0, 0.5, 0.5];
            v = push_ignore_region(v, r).expect("上限内追加应成功");
        }
        assert_eq!(v.len(), MAX_IGNORE_REGIONS);
        let before = v.clone();
        assert!(push_ignore_region(v.clone(), [0.9, 0.9, 1.0, 1.0]).is_err());
        assert_eq!(v, before);
    }

    /// J2 新字段：旧配置文件（无 formula_mode / sound 段）→ 均为默认 false；
    /// 写 true → 读回逐字段一致（roundtrip）。
    #[test]
    fn formula_and_sound_roundtrip_and_default() {
        let dir = test_dir("rumi-cfg-j2");
        let path = dir.join("config.toml");
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert!(!s.formula_mode && !s.sound);

        let mut s2 = Settings::default();
        s2.formula_mode = true;
        s2.sound = true;
        save_to(&path, &s2).unwrap();
        let (s3, w3) = load_from(&path);
        assert!(w3.is_none());
        assert!(s3.formula_mode && s3.sound);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Wave3 默认浅色：新配置 theme == "light"；用户已存的 "dark" 不被覆盖
    ///（roundtrip 原样读回）。
    #[test]
    fn theme_defaults_light_and_respects_saved_value() {
        let dir = test_dir("rumi-cfg-theme");
        let path = dir.join("config.toml");
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert_eq!(s.theme, "light");

        let mut s2 = Settings::default();
        s2.theme = "dark".into();
        save_to(&path, &s2).unwrap();
        let (s3, w3) = load_from(&path);
        assert!(w3.is_none());
        assert_eq!(s3.theme, "dark");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Wave3 无边框窗口：旧配置（无 frameless 字段）→ 默认 true；
    /// 用户改成 false（出问题回退装饰模式）→ 读回保持 false。
    #[test]
    fn frameless_defaults_true_and_roundtrip() {        let dir = test_dir("rumi-cfg-frameless");
        let path = dir.join("config.toml");

        // 全新/缺失配置
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert!(s.frameless, "缺省应为无边框（DEFAULT_FRAMELESS=true）");

        // 旧配置文件：只有部分字段、无 frameless 段 → serde container default 补 true
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "hotkey = \"alt+q\"\nhttp_port = 1224\ntheme = \"dark\"\n").unwrap();
        let (s2, warn2) = load_from(&path);
        assert!(warn2.is_none());
        assert!(s2.frameless, "旧配置缺 frameless 字段应补默认 true");
        assert_eq!(s2.theme, "dark");

        // 显式 false（用户关闭回退装饰模式）→ 原样保留
        let mut s3 = Settings::default();
        s3.frameless = false;
        save_to(&path, &s3).unwrap();
        let (s4, warn4) = load_from(&path);
        assert!(warn4.is_none());
        assert!(!s4.frameless);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 体验批次关闭按钮行为：旧配置（无 close_to_tray 字段）→ 默认 true
    ///（最小化到托盘，与 Umi-OCR 一致）；用户显式选 false（点关闭直接退出）→
    /// 落盘读回保持 false。
    #[test]
    fn close_to_tray_defaults_true_and_roundtrip() {
        let dir = test_dir("rumi-cfg-close");
        let path = dir.join("config.toml");

        // 全新/缺失配置 → 默认 true
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert!(s.close_to_tray, "缺省应最小化到托盘（DEFAULT_CLOSE_TO_TRAY=true）");

        // 旧配置文件：只有部分字段、无 close_to_tray 段 → serde container default 补 true
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "hotkey = \"alt+q\"\nhttp_port = 1224\ntheme = \"dark\"\n").unwrap();
        let (s2, warn2) = load_from(&path);
        assert!(warn2.is_none());
        assert!(s2.close_to_tray, "旧配置缺 close_to_tray 字段应补默认 true");

        // 显式 false（点关闭直接退出）→ 原样保留
        let mut s3 = Settings::default();
        s3.close_to_tray = false;
        save_to(&path, &s3).unwrap();
        let (s4, warn4) = load_from(&path);
        assert!(warn4.is_none());
        assert!(!s4.close_to_tray);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// M 波选区方式：旧配置（无 picker_mode 字段）→ 默认 auto（跟随后端）；
    /// 三档存储串 roundtrip 一致；parse 的未知串回退 auto。
    #[test]
    fn picker_mode_defaults_roundtrip_and_parse() {
        let dir = test_dir("rumi-cfg-picker");
        let path = dir.join("config.toml");
        let (s, warn) = load_from(&path);
        assert!(warn.is_none());
        assert_eq!(s.picker_mode, DEFAULT_PICKER_MODE);
        assert_eq!(parse_picker_mode(&s.picker_mode), PickerMode::Auto);

        for (stored, want) in [
            ("native", PickerMode::Native),
            ("builtin", PickerMode::Builtin),
            ("auto", PickerMode::Auto),
        ] {
            let mut s2 = Settings::default();
            s2.picker_mode = stored.into();
            save_to(&path, &s2).unwrap();
            let (s3, w3) = load_from(&path);
            assert!(w3.is_none());
            assert_eq!(s3.picker_mode, stored, "{stored} 应原样落盘读回");
            assert_eq!(parse_picker_mode(&s3.picker_mode), want);
        }
        // 未知/空串 → auto（跟随后端）
        assert_eq!(parse_picker_mode("garbage"), PickerMode::Auto);
        assert_eq!(parse_picker_mode(""), PickerMode::Auto);
        std::fs::remove_dir_all(&dir).ok();
    }
}
