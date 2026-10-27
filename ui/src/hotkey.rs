//! hotkey —— 全局热键（M3 任务书 §1：默认 Alt+Q 触发"区域截图"，可配置）。
//!
//! ## 调研结论（global-hotkey 0.7.0 + dioxus-desktop 0.7.10，Wayland 可用性）
//!
//! - dioxus-desktop 内置桥接：`use_global_shortcut(hotkey, handler)` 在组件内注册，
//!   事件经 tao 事件循环（`UserWindowEvent::GlobalHotKeyEvent`）回 `ShortcutRegistry`
//!   再调用 handler；注册失败返回 `Err(ShortcutRegistryError)`，**不会 panic**。
//! - global-hotkey 0.7 Linux 后端是 **X11 grab**（platform_impl/x11，x11rb），
//!   没有 Wayland/KDE kglobalaccel 实现：
//!   * 纯 Wayland（无 XWayland）：`GlobalHotKeyManager::new()` 只 spawn 一个事件线程，
//!     连不上 X 也不报错；`register()` 的 reply 通道断开被 `if let Ok(result) = rx.recv()`
//!     吞掉 → **静默假成功、永不触发**。所以"注册成功"不可信，必须先做环境探测
//!     （`XDG_SESSION_TYPE`/`WAYLAND_DISPLAY` + `DISPLAY`）。
//!   * Wayland + XWayland（KDE Plasma 默认开启）：注册成功、X11 grab 生效，但按键
//!     只有走到 XWayland（焦点在 X11 窗口，或组合键未被 Wayland 合成器截获）才触发。
//!     状态行按会话给出相应说明；托盘菜单是功能等价的兜底触发路径。
//! - 另一个坑：`use_global_shortcut` 对 `&str` 走 `HotKey::from_str(..).unwrap()`——
//!   配置串非法会在渲染期 panic。[`validate`] 先按 global-hotkey 的解析规则挡一遍。
//!
//! 热键配置改后**重启生效**（M3 验收 #5 允许）；因此"是否注册"的决策在启动时确定一次，
//! 用子组件承载 hook（避免运行时切换分支破坏 hook 顺序）。

use crate::settings;
use dioxus::desktop::{use_global_shortcut, HotKeyState};
use dioxus::prelude::*;

const MODIFIERS: &[&str] = &[
    "alt", "ctrl", "control", "shift", "super", "meta", "cmd", "command", "option",
];

/// 会话信息 → 已注册状态行要叠加的说明（None = 纯 X11 会话，无附加说明）
fn session_note() -> Option<&'static str> {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let wayland = session.eq_ignore_ascii_case("wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if wayland {
        Some("Wayland 会话：经 XWayland 生效，焦点在原生 Wayland 窗口时可能不触发，可用托盘菜单兜底")
    } else {
        None
    }
}

/// 环境探测核心（纯函数，可测）：Wayland 会话且无 XWayland → 判定不可用。
fn availability(session: &str, has_wayland_display: bool, has_display: bool) -> Option<String> {
    let wayland = session.eq_ignore_ascii_case("wayland") || has_wayland_display;
    if wayland && !has_display {
        return Some("热键不可用（Wayland 限制，无 XWayland），可用托盘菜单触发".into());
    }
    None
}

/// 环境探测：Wayland 会话且无 XWayland → 判定不可用（register 会假成功，不能信）。
/// Some(原因) = 不可用。
pub fn probe_unavailable() -> Option<String> {
    availability(
        &std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    )
}

/// 校验配置串（global-hotkey 的 FromStr 失败会 unwrap panic，先挡掉）。
/// 规则：`+` 分段；最后一段是键（单字母/数字、F1..F24、或白名单词），其余是修饰键白名单。
pub fn validate(hotkey: &str) -> Result<(), String> {
    let parts: Vec<&str> = hotkey.split('+').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!("空段：{hotkey:?}"));
    }
    let (mods, keys) = parts.split_at(parts.len() - 1);
    let key = keys[0];
    let key_ok = {
        let up = key.to_uppercase();
        let single = up.chars().count() == 1 && up.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
        let fnkey = up.starts_with('F')
            && up.len() <= 3
            && up[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n));
        single || fnkey
    };
    if !key_ok {
        // 与 global-hotkey parse_key 的宽严一致：单词键再放行一批常见写法
        const WORDS: &[&str] = &[
            "space", "tab", "enter", "escape", "esc", "backspace", "delete", "insert", "home",
            "end", "pageup", "pagedown", "left", "right", "up", "down", "comma", "period",
            "semicolon", "quote", "minus", "equal",
        ];
        if !WORDS.contains(&key.to_lowercase().as_str()) {
            return Err(format!("无法识别的键：{key:?}"));
        }
    }
    for m in mods {
        if !MODIFIERS.contains(&m.to_lowercase().as_str()) {
            return Err(format!("无法识别的修饰键：{m:?}"));
        }
    }
    Ok(())
}

/// 启动时确定一次的注册计划（Skip = 显示原因；Register = 进子组件注册）
#[derive(Clone, PartialEq)]
enum Plan {
    Skip(String),
    Register(String),
}

fn plan() -> Plan {
    if let Some(reason) = probe_unavailable() {
        return Plan::Skip(reason);
    }
    let hk = settings::get().hotkey;
    if let Err(e) = validate(&hk) {
        return Plan::Skip(format!("热键配置无效（{e}），可用托盘菜单触发"));
    }
    Plan::Register(hk)
}

/// 状态行内嵌组件：探测→注册→就地显示结果（不写 Signal，渲染期无副作用）。
#[component]
pub fn HotkeyBridge(on_trigger: Callback) -> Element {
    let plan = use_hook(plan);
    match plan {
        Plan::Skip(reason) => rsx! {
            span { class: "hk hk-bad", title: "托盘菜单可触发同样的截图流程", "{reason}" }
        },
        Plan::Register(hk) => rsx! {
            HotkeyRegistered { hotkey: hk, on_trigger }
        },
    }
}

/// 实际注册子组件：`use_global_shortcut` 的 init 闭包只在挂载时执行一次，
/// 结果存 hook 里，重渲染只读不重注册。
#[component]
fn HotkeyRegistered(hotkey: String, on_trigger: Callback) -> Element {
    let res = use_global_shortcut(hotkey.as_str(), move |state| {
        // global-hotkey 按下/松开各发一次，只在按下沿触发
        if state == HotKeyState::Pressed {
            on_trigger.call(());
        }
    });
    let (cls, msg) = match &res {
        Ok(_) => match session_note() {
            Some(note) => ("hk hk-warn", format!("全局热键 {hotkey} 已生效（{note}）")),
            None => ("hk hk-ok", format!("全局热键 {hotkey} 已生效")),
        },
        Err(e) => (
            "hk hk-bad",
            format!("热键 {hotkey} 注册失败（{e:?}），可用托盘菜单触发"),
        ),
    };
    rsx! {
        span { class: "{cls}", title: "热键触发与主界面按钮走同一流程", "{msg}" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_umi_default() {
        assert!(validate("alt+q").is_ok());
        assert!(validate("Ctrl + Shift + A").is_ok());
        assert!(validate("F9").is_ok());
        assert!(validate("super+Space").is_ok());
    }

    #[test]
    fn validate_rejects_garbage() {
        assert!(validate("").is_err());
        assert!(validate("alt+").is_err());
        assert!(validate("alt+q+win").is_err()); // 多个主键 → 尾段 win 不是合法修饰键
        assert!(validate("banana+q").is_err());
        assert!(validate("alt+F99").is_err());
    }

    #[test]
    fn probe_pure_wayland_unavailable() {
        // 纯 Wayland（无 XWayland）→ 不可用；X11 与 XWayland → 可用
        assert!(availability("wayland", false, false).is_some());
        assert!(availability("", true, false).is_some());
        assert!(availability("wayland", false, true).is_none());
        assert!(availability("x11", false, true).is_none());
        assert!(availability("tty", false, false).is_none());
    }
}
