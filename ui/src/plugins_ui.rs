//! plugins_ui —— 引擎切换页（M3b 交付 4）：
//!
//! - `plugins/` 目录扫描（[`plugins_worker::scan_plugins`]）
//! - 选中插件 → 起 worker（`PluginProcess::spawn` → `info`）拿 schema，
//!   按 Umi 配置 schema（规范 §2：group 容器 / 叶子 default 文本 / optionsList 下拉 / toolTip）
//!   动态渲染表单；保存进 settings
//! - "切换为此插件引擎"：init(全局值) + start(本地值) → settings.engine = "plugin:<名字>"
//! - 识别走 `runBytes`（Frame 先内存编码 PNG，免落盘；见 ocr_flow::recognize_frame）
//! - 语言变更 → [`respawn_for_lang`] 重启 worker 并重走 init+start（规范 §3）
//!
//! worker 单例：进程级 `Mutex<Option<WorkerBox>>`（与剪贴板同款模式）。
//! `BlockingPluginProcess` 内置 current-thread tokio runtime，可从任意 std 线程调用，
//! UI 里全部经 `run_blocking` + dioxus `spawn`，不占 UI 线程。
//!
//! CLI 级验证：`cargo test -p rumi-ui plugins_ui`（schema 解析 + 选项合并）。

use crate::settings;
use dioxus::prelude::*;
use plugins_worker::BlockingPluginProcess;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// 插件目录：`RUMI_PLUGINS_DIR` 优先，回退工作区布局（开发期），与 plugins-worker 定位
/// pyshim 的方式一致（CARGO_MANIFEST_DIR 编译期常量）。
pub fn plugins_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUMI_PLUGINS_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../plugins")
}

/// venv 存在则用 venv 解释器（venv::ensure 幂等但首次创建耗时，UI 侧不自动建），
/// 否则系统 python3。依赖缺失由 worker stderr 报出。
pub fn python_for_plugins() -> PathBuf {
    let venv = plugins_worker::venv::python_path(&venv_dir());
    if venv.is_file() {
        return venv;
    }
    PathBuf::from("python3")
}

fn venv_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("RUMI_VENV_DIR") {
        return PathBuf::from(d);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("rumi")
        .join("venv")
}

/// 活跃 worker 单例（引擎 = plugin:<name> 时存在）。
pub struct WorkerBox {
    pub name: String,
    pub proc: BlockingPluginProcess,
}

static WORKER: OnceLock<Mutex<Option<WorkerBox>>> = OnceLock::new();

fn worker_cell() -> &'static Mutex<Option<WorkerBox>> {
    WORKER.get_or_init(|| Mutex::new(None))
}

fn worker_lock() -> std::sync::MutexGuard<'static, Option<WorkerBox>> {
    worker_cell().lock().unwrap_or_else(|p| p.into_inner())
}

/// worker pid（设置页/插件页状态展示用）
pub fn active_worker_pid() -> Option<u32> {
    worker_lock().as_ref().and_then(|w| w.proc.id())
}

/// 丢弃当前 worker（kill_on_drop 回收 python 子进程）。切回 native 时调用。
pub fn drop_worker() {
    *worker_lock() = None;
}

/// 确保 worker 指向 `name`（不存在/名字不同则先丢弃再 spawn）。仅 spawn，不 init/start。
fn ensure_spawned(name: &str, dir: &std::path::Path) -> Result<(), String> {
    let mut guard = worker_lock();
    if let Some(w) = guard.as_ref() {
        if w.name == name {
            return Ok(());
        }
    }
    let lang = settings::get().lang;
    let proc = BlockingPluginProcess::spawn(&python_for_plugins(), dir, &lang)
        .map_err(|e| format!("启动 worker：{e}"))?;
    *guard = Some(WorkerBox {
        name: name.to_string(),
        proc,
    });
    Ok(())
}

/// 用已 spawn 的 worker 走 init + start（options 从 settings 取，按 schema 合并默认值后 coerce）。
/// `schema` 为该插件 info 返回的 global/local options（用于合并默认值）。
fn init_and_start(
    name: &str,
    global_schema: &serde_json::Value,
    local_schema: &serde_json::Value,
) -> Result<(), String> {
    let saved = settings::get().plugins.get(name).cloned().unwrap_or_default();
    let global = merge_coerce(&saved.global, global_schema);
    let local = merge_coerce(&saved.local, local_schema);

    let mut guard = worker_lock();
    let w = guard
        .as_mut()
        .filter(|w| w.name == name)
        .ok_or("worker 未就绪（应先 ensure_spawned）")?;
    w.proc
        .init(global)
        .map_err(|e| format!("init(Api)：{e}"))?;
    let start = w
        .proc
        .start(local)
        .map_err(|e| format!("start 引擎：{e}"))?;
    // 插件层结果 {"ok":bool,"error":str}：false 视为启动失败（worker 还活着，可继续改配置重启）
    let ok = start.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        let err = start
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("插件未回报错误信息");
        return Err(format!("引擎启动失败：{err}"));
    }
    Ok(())
}

/// 惰性起引擎：spawn（如缺）→ init+start。切换引擎按钮与首次插件识别都走这里。
pub fn ensure_engine_running(
    name: &str,
    global_schema: &serde_json::Value,
    local_schema: &serde_json::Value,
) -> Result<(), String> {
    ensure_spawned(name, &plugin_dir_of(name))?;
    init_and_start(name, global_schema, local_schema)
}

/// runBytes 识别：worker 未起则先 ensure_engine_running；WorkerDied 自动重启一次并重试。
/// 必须在 std 线程调用（BlockingPluginProcess 阻塞）。
pub fn run_bytes_recognize(
    name: &str,
    png: &[u8],
) -> Result<Vec<plugins_worker::OcrLine>, String> {
    // 无 info 缓存（进程刚重启就直接识别）→ 先补一次 info
    if cached_schema().map(|c| c.name != name).unwrap_or(true) {
        let info = fetch_info_blocking(&plugin_dir_of(name))?;
        cache_schema(info);
    }
    let cached = cached_schema().expect("上一步已填充缓存");
    let mut attempt = 0;
    loop {
        ensure_engine_running(name, &cached.global_options, &cached.local_options)?;
        let result = {
            let mut guard = worker_lock();
            match guard.as_mut().filter(|w| w.name == name) {
                Some(w) => Some(w.proc.run_bytes(png).map_err(|e| e.to_string())),
                None => return Err("worker 意外丢失".into()),
            }
        };
        match result {
            Some(Ok(v)) => {
                return plugins_worker::plugin_result_to_lines(&v)
                    .map_err(|e| format!("插件识别：{e}"));
            }
            _ => {
                // WorkerDied / 其他错误：第一次先重启引擎再试一次（规范 §3）
                attempt += 1;
                if attempt > 1 {
                    let msg = match result {
                        Some(Err(e)) => e,
                        _ => "未知错误".into(),
                    };
                    return Err(format!("插件识别失败：{msg}"));
                }
                *worker_lock() = None; // kill_on_drop 回收尸体
            }
        }
    }
}

/// settings.plugins 里记录的名字 → 目录（重新扫描对齐；插件被删时返回不存在路径，由 spawn 报错）
pub fn plugin_dir_of(name: &str) -> PathBuf {
    plugins_dir().join(name)
}

/// 语言变更：若有活跃 worker，用新 lang 重启并重走 init+start（schema 从 info 缓存取；
/// 无缓存则只换 spawn 参数，等下次识别时 ensure_engine_running 再 init+start）。
/// 在 std 线程调用；结果给设置页状态行展示。
pub fn respawn_for_lang(new_lang: &str) -> Result<(), String> {
    let cached = cached_schema();
    let name = {
        let guard = worker_lock();
        let Some(w) = guard.as_ref() else {
            return Ok(()); // 没起插件引擎，无需处理
        };
        w.name.clone()
    };

    // 用新 --lang 重新 spawn（旧进程 kill_on_drop）
    {
        let mut guard = worker_lock();
        if guard.as_ref().map(|w| w.name.as_str()) != Some(name.as_str()) {
            return Ok(()); // 期间引擎被切走，放弃
        }
        let proc = BlockingPluginProcess::spawn(&python_for_plugins(), &plugin_dir_of(&name), new_lang)
            .map_err(|e| format!("按新语言重启 worker：{e}"))?;
        *guard = Some(WorkerBox {
            name: name.clone(),
            proc,
        });
    }
    // 有本插件的 schema 缓存就立即 init+start，保证下次识别直接可用
    if let Some(c) = cached {
        if c.name == name {
            init_and_start(&name, &c.global_options, &c.local_options)?;
        }
    }
    Ok(())
}

/// ── schema 解析（Umi 配置 schema → 可渲染字段，规范 §2）────────────────────────

/// 表单里一个可编辑字段
#[derive(Debug, Clone, PartialEq)]
pub struct FieldView {
    pub key: String,
    pub title: String,
    pub tool_tip: String,
    pub kind: FieldKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// 文本（default 为初始值）
    Text { default: String },
    /// optionsList 下拉：[[value, label], ...]
    Select { options: Vec<(String, String)>, default: String },
}

/// info 返回的一个插件（全局/本地 schema 已缓存）
#[derive(Debug, Clone)]
pub struct PluginInfoView {
    pub name: String,
    pub group: String,
    pub global_options: serde_json::Value,
    pub local_options: serde_json::Value,
}

static SCHEMA_CACHE: OnceLock<Mutex<Option<PluginInfoView>>> = OnceLock::new();

fn schema_cache_lock() -> std::sync::MutexGuard<'static, Option<PluginInfoView>> {
    SCHEMA_CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap_or_else(|p| p.into_inner())
}

pub fn cache_schema(info: PluginInfoView) {
    *schema_cache_lock() = Some(info);
}

pub fn cached_schema() -> Option<PluginInfoView> {
    schema_cache_lock().as_ref().cloned()
}

/// 解析 Umi options schema：跳过 "title"/"type" 元键；
/// 叶子 `optionsList: [[v, label], ...]` → 下拉；其余有 `default` → 文本。
/// 容错：非对象、缺 title（用键名）、值类型怪异 → 跳过该键。
pub fn parse_schema(schema: &serde_json::Value) -> Vec<FieldView> {
    let Some(map) = schema.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, leaf) in map {
        if key == "title" || key == "type" {
            continue;
        }
        let Some(leaf) = leaf.as_object() else { continue };
        let title = leaf
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(key)
            .to_string();
        let tool_tip = leaf
            .get("toolTip")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let kind = if let Some(list) = leaf.get("optionsList").and_then(|v| v.as_array()) {
            let options = list
                .iter()
                .filter_map(|pair| {
                    let p = pair.as_array()?;
                    Some((
                        p.first()?.as_str()?.to_string(),
                        p.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    ))
                })
                .collect::<Vec<_>>();
            if options.is_empty() {
                continue;
            }
            let default = leaf
                .get("default")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| options[0].0.clone());
            FieldKind::Select { options, default }
        } else {
            let default = match leaf.get("default") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                Some(serde_json::Value::Bool(b)) => b.to_string(),
                _ => String::new(),
            };
            FieldKind::Text { default }
        };
        out.push(FieldView {
            key: key.clone(),
            title,
            tool_tip,
            kind,
        });
    }
    out
}

/// 表单值 ↔ 默认值合并：schema 字段为骨架，settings 保存值优先，缺省取 default。
/// 返回 (字段, 当前值) 序列（渲染顺序稳定：parse_schema 已按对象序）。
pub fn merge_form(
    fields: &[FieldView],
    saved: &BTreeMap<String, String>,
) -> Vec<(FieldView, String)> {
    fields
        .iter()
        .map(|f| {
            let default = match &f.kind {
                FieldKind::Text { default } => default.clone(),
                FieldKind::Select { default, .. } => default.clone(),
            };
            let val = saved.get(&f.key).cloned().unwrap_or(default);
            (f.clone(), val)
        })
        .collect()
}

/// 表单值 → worker init/start 的 JSON。Umi 语义要求 init/start 收到"填满默认值"的
/// 完整 dict（`Api.__init__` 直接 `globalArgd[key]` 取键），因此**schema 里每个叶子键
/// 都会输出**：已保存值优先，未保存的键按叶子类型补 default（见 settings::coerce）。
pub fn merge_coerce(
    values: &BTreeMap<String, String>,
    schema: &serde_json::Value,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(map) = schema.as_object() {
        for (key, leaf) in map {
            if key == "title" || key == "type" {
                continue;
            }
            let stored = values.get(key).map(String::as_str).unwrap_or("");
            out.insert(key.clone(), settings::coerce(stored, leaf));
        }
    }
    serde_json::Value::Object(out)
}

/// std 线程里拉取一个插件的 info：spawn worker → info → 缓存 schema → 返回视图。
pub fn fetch_info_blocking(dir: &std::path::Path) -> Result<PluginInfoView, String> {
    let lang = settings::get().lang;
    let mut proc = BlockingPluginProcess::spawn(&python_for_plugins(), dir, &lang)
        .map_err(|e| format!("启动 worker：{e}"))?;
    let info = proc.info().map_err(|e| format!("读取插件 info：{e}"))?;
    let entry = plugins_worker::PluginEntry::from_info(&info);
    // 临时进程用完即弃（kill_on_drop）；真正启用时再按 ensure_engine_running 起长期 worker
    drop(proc);
    if entry.name.is_empty() {
        return Err("插件 info 未返回 name（__init__.py 的 PluginInfo 缺 name？）".into());
    }
    Ok(PluginInfoView {
        name: entry.name,
        group: entry.group,
        global_options: entry.global_options,
        local_options: entry.local_options,
    })
}

// ── UI：引擎切换页 ──────────────────────────────────────────────────────────
// rsx 循环体里不用 let / 元组解构（M1 实证），先把数据揉成渲染结构再进 rsx。

/// 表单一行字段的渲染数据
#[derive(Clone, PartialEq)]
struct FieldRow {
    key: String,
    title: String,
    tooltip: String,
    value: String,
    is_select: bool,
    options: Vec<OptRow>,
}

#[derive(Clone, PartialEq)]
struct OptRow {
    value: String,
    label: String,
}

fn rows_of(fields: &[FieldView], saved: &BTreeMap<String, String>) -> Vec<FieldRow> {
    merge_form(fields, saved)
        .into_iter()
        .map(|(f, val)| FieldRow {
            is_select: matches!(f.kind, FieldKind::Select { .. }),
            options: match f.kind {
                FieldKind::Select { options, .. } => options
                    .into_iter()
                    .map(|(value, label)| OptRow { value, label })
                    .collect(),
                FieldKind::Text { .. } => Vec::new(),
            },
            key: f.key,
            title: f.title,
            tooltip: f.tool_tip,
            value: val,
        })
        .collect()
}

/// 单字段编辑器：文本框 / 下拉；toolTip 挂在 label 的 title 上。
#[component]
fn FieldEditor(row: FieldRow, on_edit: Callback<(String, String)>) -> Element {
    let key = row.key.clone();
    let key_sel = row.key.clone();
    let val = row.value.clone();
    rsx! {
        div { class: "opt",
            label { class: "opt-label", title: "{row.tooltip}", "{row.title}" }
            if row.is_select {
                select {
                    onchange: move |e| on_edit.call((key_sel.clone(), e.value())),
                    for opt in row.options.iter() {
                        option { value: "{opt.value}", selected: opt.value == val, "{opt.label}" }
                    }
                }
            } else {
                input {
                    value: "{val}",
                    placeholder: "{row.key}",
                    oninput: move |e| on_edit.call((key.clone(), e.value())),
                }
            }
        }
    }
}

/// 引擎切换页：插件列表 + 选中插件的 global/local options 动态表单 +
/// 保存 / 切换引擎 / 切回 native。
#[component]
pub fn PluginsPage(
    mut page_status: Signal<String>,
    mut engine_text: Signal<String>,
) -> Element {
    let mut names = use_signal(|| -> Vec<String> {
        let mut list: Vec<String> = plugins_worker::scan_plugins(&plugins_dir())
            .into_iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        list.sort();
        list
    });
    let mut selected = use_signal(|| 0usize);
    let mut loaded = use_signal(|| String::new());
    let mut fields_g = use_signal(Vec::<FieldView>::new);
    let mut fields_l = use_signal(Vec::<FieldView>::new);
    let mut form_g = use_signal(BTreeMap::<String, String>::new);
    let mut form_l = use_signal(BTreeMap::<String, String>::new);

    // 拉取 info（std 线程起临时 worker 读 schema，用完即弃）
    let mut load_info = move |idx: usize| {
        let list = names();
        let Some(name) = list.get(idx).cloned() else {
            page_status.set("plugins/ 目录里没有可用插件（含 __init__.py 的一级子目录）".into());
            return;
        };
        page_status.set(format!("读取 {name} 的 info / 配置 schema …"));
        let dir = plugin_dir_of(&name);
        let rx = crate::ocr_flow::run_blocking(move || fetch_info_blocking(&dir));
        spawn(async move {
            match rx.await {
                Ok(Ok(info)) => {
                    page_status.set(format!("✅ 已读取 {}（组：{}）", info.name, info.group));
                    cache_schema(info.clone());
                    let saved = settings::get()
                        .plugins
                        .get(&info.name)
                        .cloned()
                        .unwrap_or_default();
                    fields_g.set(parse_schema(&info.global_options));
                    fields_l.set(parse_schema(&info.local_options));
                    form_g.set(saved.global);
                    form_l.set(saved.local);
                    loaded.set(info.name);
                }
                Ok(Err(e)) => {
                    loaded.set(String::new());
                    page_status.set(format!("❌ {e}"));
                }
                Err(_) => page_status.set("❌ info 线程意外退出".into()),
            }
        });
    };

    // 选中变化（含首次挂载后的首跑）→ 自动加载对应 schema
    use_effect(move || {
        let idx = selected();
        if !names().is_empty() {
            let current = loaded();
            let want = names().get(idx).cloned().unwrap_or_default();
            if current != want {
                load_info(idx);
            }
        }
    });

    let on_edit_g = Callback::new(move |(k, v): (String, String)| {
        form_g.write().insert(k, v);
    });
    let on_edit_l = Callback::new(move |(k, v): (String, String)| {
        form_l.write().insert(k, v);
    });

    let names_v = names();
    let selected_v = selected();
    let loaded_v = loaded();
    let rows_g = rows_of(&fields_g(), &form_g());
    let rows_l = rows_of(&fields_l(), &form_l());
    let pid = active_worker_pid();
    let engine_now = engine_text();

    // 保存配置（写 settings.plugins.<名字>）
    let save_cfg = move |mut status: Signal<String>| {
        let name = loaded();
        if name.is_empty() {
            status.set("尚未加载插件".into());
            return;
        }
        let g = form_g();
        let l = form_l();
        match settings::update(|st| {
            let po = st.plugins.entry(name.clone()).or_default();
            po.global = g.clone();
            po.local = l.clone();
            Ok(())
        }) {
            Ok(()) => status.set(format!("✅ 已保存 {name} 的配置")),
            Err(e) => status.set(format!("❌ {e}")),
        }
    };

    rsx! {
        div { class: "page",
            div { class: "page-head",
                span { class: "page-title", "插件 / 引擎切换" }
                span { class: "chip", "当前引擎 {engine_now}" }
                if let Some(p) = pid {
                    span { class: "hint", "worker pid: {p}" }
                }
                button {
                    onclick: move |_| {
                        let mut list: Vec<String> =
                            plugins_worker::scan_plugins(&plugins_dir())
                                .into_iter()
                                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                                .collect();
                        list.sort();
                        names.set(list);
                        page_status.set("已刷新插件列表".into());
                    },
                    "刷新列表" }
            }
            crate::page_status_line { status: page_status }

            div { class: "plugin-list",
                for (idx, name) in names_v.iter().enumerate() {
                    button {
                        key: "{name}",
                        class: if idx == selected_v { "plug plug-sel" } else { "plug" },
                        onclick: move |_| selected.set(idx),
                        "{name}" }
                }
                if names_v.is_empty() {
                    div { class: "empty-state",
                        crate::NavIcon { name: crate::NavIconName::Plugins, size: 34 }
                        div { class: "empty-title", "没有可用插件" }
                        div { class: "empty-sub",
                            "plugins/ 下没有插件。把 Umi 插件目录（含 __init__.py）放进来后点“刷新列表”。" }
                    }
                }
            }

            if !loaded_v.is_empty() {
                div { class: "schema",
                    div { class: "schema-head", "{loaded_v} · 全局选项（global_options）" }
                    for row in rows_g.iter().cloned() {
                        FieldEditor { key: "{row.key}", row, on_edit: on_edit_g }
                    }
                    if rows_g.is_empty() {
                        div { class: "hint", "（无全局选项）" }
                    }
                    div { class: "schema-head", "{loaded_v} · 会话选项（local_options）" }
                    for row in rows_l.iter().cloned() {
                        FieldEditor { key: "{row.key}", row, on_edit: on_edit_l }
                    }
                    if rows_l.is_empty() {
                        div { class: "hint", "（无会话选项）" }
                    }

                    div { class: "row",
                        button {
                            onclick: move |_| save_cfg(page_status),
                            "保存配置" }
                        button {
                            class: "primary",
                            onclick: move |_| {
                                let name = loaded();
                                if name.is_empty() { return; }
                                page_status.set(format!(
                                    "切换到插件引擎 {name}：保存 options → 启动 worker → init → start …"));
                                // Signal 非 Send：起线程前把表单值与 schema 缓存取出
                                let g = form_g();
                                let l = form_l();
                                let cached = cached_schema();
                                let name_ui = name.clone(); // 线程结束后回填状态行用
                                let rx = crate::ocr_flow::run_blocking(move || {
                                    settings::update(|st| {
                                        let po = st.plugins.entry(name.clone()).or_default();
                                        po.global = g.clone();
                                        po.local = l.clone();
                                        Ok::<(), String>(())
                                    })?;
                                    let (gs, ls) = match &cached {
                                        Some(c) if c.name == name => {
                                            (c.global_options.clone(), c.local_options.clone())
                                        }
                                        _ => (serde_json::Value::Null, serde_json::Value::Null),
                                    };
                                    ensure_engine_running(&name, &gs, &ls)?;
                                    settings::update(|st| {
                                        st.engine = format!("plugin:{name}");
                                        Ok::<(), String>(())
                                    })?;
                                    Ok::<(), String>(())
                                });
                                spawn(async move {
                                    match rx.await {
                                        Ok(Ok(())) => {
                                            engine_text.set(format!("plugin:{name_ui}"));
                                            page_status.set(format!(
                                                "✅ 当前引擎已切换为 plugin:{name_ui}（识别走 runBytes）"));
                                        }
                                        Ok(Err(e)) => page_status.set(format!("❌ 切换失败：{e}")),
                                        Err(_) => page_status.set("❌ 切换线程意外退出".into()),
                                    }
                                });
                            },
                            "切换为此插件引擎" }
                        button {
                            class: "secondary",
                            disabled: engine_now == settings::ENGINE_NATIVE,
                            onclick: move |_| match settings::update(|st| {
                                st.engine = settings::ENGINE_NATIVE.into();
                                Ok(())
                            }) {
                                Ok(()) => {
                                    crate::plugins_ui::drop_worker();
                                    engine_text.set(settings::ENGINE_NATIVE.into());
                                    page_status.set("✅ 已切回 native 引擎".into());
                                }
                                Err(e) => page_status.set(format!("❌ {e}")),
                            },
                            "切回 native 引擎" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_schema_fields() {
        let schema = json!({
            "title": "阿巴阿巴OCR",
            "type": "group",
            "api_key": {"title": "Api密钥", "default": "", "toolTip": "密钥提示"},
            "threshold": {"title": "阈值", "default": 0.6},
            "language": {"title": "语言", "optionsList": [["zh_CN", "简体中文"], ["en_US", "English"]]},
            "weird": "不是对象的叶子应跳过"
        });
        let fields = parse_schema(&schema);
        assert_eq!(fields.len(), 3);
        let by_key = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .unwrap_or_else(|| panic!("字段 {k} 缺失"))
                .clone()
        };
        // 注：serde_json 无 preserve_order 特性时对象键按字典序遍历，顺序不保证插入序
        let api = by_key("api_key");
        assert_eq!(api.title, "Api密钥");
        assert_eq!(api.tool_tip, "密钥提示");
        assert_eq!(api.kind, FieldKind::Text { default: String::new() });

        let threshold = by_key("threshold");
        assert_eq!(threshold.kind, FieldKind::Text { default: "0.6".into() });

        match by_key("language").kind {
            FieldKind::Select { options, default } => {
                assert_eq!(options.len(), 2);
                assert_eq!(options[0], ("zh_CN".into(), "简体中文".into()));
                assert_eq!(default, "zh_CN");
            }
            other => panic!("预期下拉，实际 {other:?}"),
        }
    }

    #[test]
    fn merge_form_saved_overrides_default() {
        let fields = vec![FieldView {
            key: "api_key".into(),
            title: "密钥".into(),
            tool_tip: String::new(),
            kind: FieldKind::Text { default: String::new() },
        }];
        let mut saved = BTreeMap::new();
        saved.insert("api_key".to_string(), "sk-9".to_string());
        let merged = merge_form(&fields, &saved);
        assert_eq!(merged[0].1, "sk-9");
        let merged2 = merge_form(&fields, &BTreeMap::new());
        assert_eq!(merged2[0].1, "");
    }

    #[test]
    fn merge_coerce_types() {
        let schema = json!({
            "type": "group",
            "threshold": {"default": 0.6},
            "api_key": {"default": ""},
        });
        let mut vals = BTreeMap::new();
        vals.insert("threshold".to_string(), "0.8".to_string());
        let v = merge_coerce(&vals, &schema);
        assert_eq!(v["threshold"], json!(0.8));
        // 未保存的键也要出现（Umi 语义：填满默认值的完整 dict），按叶子类型补默认
        assert_eq!(v["api_key"], json!(""));
        // schema 外的键不出现
        assert!(v.get("missing").is_none());

        // 空存储 + 数字默认 → 用默认数值
        let v2 = merge_coerce(&BTreeMap::new(), &schema);
        assert_eq!(v2["threshold"], json!(0.6));

        // 已保存文本叶子即使长得像数字也保持字符串
        let mut vals3 = BTreeMap::new();
        vals3.insert("api_key".to_string(), "12345".to_string());
        let v3 = merge_coerce(&vals3, &schema);
        assert_eq!(v3["api_key"], json!("12345"));

        // bool 叶子
        let schema_b = json!({"flag": {"default": true}});
        let mut vals_b = BTreeMap::new();
        vals_b.insert("flag".to_string(), "false".to_string());
        assert_eq!(merge_coerce(&vals_b, &schema_b)["flag"], json!(false));
    }

    /// 端到端冒烟（需 python3 + pyshim + plugins/demo_AbaOCR，M2 环境应已具备）：
    /// 走 UI 的真实调用链 fetch_info → 缓存 schema → run_bytes_recognize（内部
    /// spawn → init → start → runBytes）→ 语言 respawn。
    /// 运行：`cargo test -p rumi-ui -- --ignored --nocapture`
    #[test]
    #[ignore = "需 python3 环境；手动冒烟运行：cargo test -p rumi-ui -- --ignored"]
    fn worker_smoke_demo_plugin() {
        let dir = plugin_dir_of("demo_AbaOCR");
        assert!(
            dir.join("__init__.py").is_file(),
            "plugins/demo_AbaOCR 不存在（{}）",
            dir.display()
        );

        // 1) info / schema
        let info = fetch_info_blocking(&dir).expect("fetch_info_blocking");
        assert_eq!(info.name, "demo_AbaOCR");
        assert_eq!(info.group, "ocr");
        cache_schema(info);

        // 2) 4x4 白色 PNG（内存编码，模拟 Frame→PNG）
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        // 3) runBytes 识别（demo 插件对任意输入返回“阿巴阿巴阿巴”）
        let lines = run_bytes_recognize("demo_AbaOCR", &png).expect("run_bytes_recognize");
        assert!(!lines.is_empty(), "demo 插件应返回至少一行");
        assert!(lines[0].text.contains("阿巴阿巴"), "实际: {:?}", lines[0].text);

        // 4) 语言变更 → respawn worker --lang，识别仍可用
        respawn_for_lang("en_US").expect("respawn_for_lang");
        let lines2 = run_bytes_recognize("demo_AbaOCR", &png).expect("run after respawn");
        assert!(!lines2.is_empty());

        // 5) 收尾：丢弃 worker（kill_on_drop 回收 python）
        drop_worker();
    }
}
