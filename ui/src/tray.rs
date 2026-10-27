//! tray —— 系统托盘（M3 任务书 §2 + Wave2-C P1 #10 + O 波托盘点击回归修复）。
//!
//! ## 调研结论（dioxus-desktop 0.7.10，本工作区 Cargo.lock 实际版本）
//!
//! dioxus-desktop **内置** tray-icon 0.21.3（菜单层是 muda 0.17）与 global-hotkey 0.7.0，
//! 且事件已桥进 tao/wry 事件循环（`UserWindowEvent::TrayMenuEvent/TrayIconEvent/
//! GlobalHotKeyEvent` 经 `EventLoopProxy` 派发）——**不需要**自接 crate / 事件循环整合：
//!
//! - `dioxus::desktop::trayicon::init_tray_icon(menu, icon)`：组件渲染期调用
//!   （内部 `provide_context`），构建 `TrayIconBuilder`，左键点图标不弹菜单
//!   （`with_menu_on_left_click(false)`）；
//! - 左键点托盘 = 显示主窗口：`Config::with_tray_icon_show_window_on_click`（默认 true）；
//! - 窗口关闭按钮 = 隐藏：`Config::with_close_behaviour(WindowCloseBehaviour::WindowHides)`；
//! - 菜单点击事件：`dioxus::desktop::use_tray_menu_event_handler`（组件内注册，
//!   回调在组件 scope 内执行，可安全触碰 Signal）。
//!
//! ## 修复记录（按发现时间序，两处独立根因都曾表现为"菜单不可用"）
//!
//! ### ① 菜单整体灰显（2026-10-27，busctl 实证）
//!
//! muda 的 `MenuItemBuilder`/`CheckMenuItemBuilder` 的 `enabled` 字段是
//! `#[derive(Default)]` 的 bool——**默认 false**。不显式 `.enabled(true)`，
//! 导出的 DBusMenu 每项 `enabled=false`，KDE 如实渲染成灰色。
//! dioxus 官方示例走 `MenuItem::new(text, enabled, ..)` 构造函数（显式传参）
//! 所以没踩到；我们走 Builder API 踩坑。已修（build_menu 全部 `.enabled(true)`
//! + 测试断言 is_enabled 守卫）。
//!
//! ### ② 点击无动作（2026-10-27，最小复现 + busctl 合成 Event 实证）
//!
//! **dioxus-desktop 0.7.10 上游 bug**：`App::new` 里先 `set_menubar_receiver()`
//! 注册 `muda::MenuEvent::set_event_handler`，再 `set_tray_icon_receiver()` 注册
//! `tray_icon::menu::MenuEvent::set_event_handler`——但 `tray_icon::menu` 是
//! `pub use muda::*`（同一个全局 OnceCell），后者 `.set()` **静默失败**。
//! 结果所有菜单事件（含托盘菜单点击）都路由进 `UserWindowEvent::MudaMenuEvent`
//! → `handle_menu_event` 只认 dioxus 内置 id → 托盘点击被丢弃；
//! `TrayMenuEvent` 永不产生，`use_tray_menu_event_handler` 永不触发。
//!
//! 修复（main.rs）：handler 挂 `use_muda_event_handler`。本应用无 menubar，
//! MudaMenuEvent 即托盘事件。实证手段：纯 gtk+tray-icon 最小复现（无 dioxus）
//! 合成 Event 能触发 → 排除底层（libayatana/libdbusmenu/muda/KDE）→ 锁定
//! dioxus 桥接；换 handler 后 rumi-ui 同一合成 Event 触发 `[tray] menu event`。
//!
//! ### O 波旧记录：禁止 `TrayIcon::set_menu` 整体替换菜单（仍然有效）
//!
//! D 波为"语言/暂停态变化刷新托盘"引入的 use_effect 会在**启动后立即**把
//! `init_tray_icon` 注册的菜单整体替换一遍（`TrayIcon::set_menu(Some(新菜单))`），
//! 用户实测出现"菜单可见但点击菜单项不触发动作"的功能回归。读 tray-icon 0.21.3
//! gtk 后端 + muda 0.17.2 + libappindicator 0.9 源码确认的机制：
//!
//! 1. `set_menu` 在 Linux 走 `AppIndicator::set_menu(>k::Menu)`，即
//!    `app_indicator_set_menu` 重挂 DBus 菜单（StatusNotifierItem 的
//!    com.canonical.dbusmenu 导出被整体重建）；
//! 2. 被替换的旧 muda `Menu` 在 `self.menu = menu` 时 drop，其 `Drop` 会
//!    `gtk_widget_destroy` 旧菜单树——替换期间托盘宿主（Plasma applet）看到的
//!    菜单对象经历"销毁 + 重建"，重建后的 DBus 菜单与 applet 已建立的
//!    ItemActivated 通路在部分宿主上不再回连，表现为菜单可见、点击无事件。
//!    而 dioxus 侧 `use_tray_menu_event_handler` 的注册（app.tick 的全局
//!    event_handlers slab）与 muda `MenuEvent` 全局通道本身与菜单对象无关，
//!    因此断点在"菜单对象替换"这一步，而非事件订阅注册时机。
//!
//! 修复策略（本文件）：**菜单只在启动时构建一次并常驻**，语言 / 热键暂停态 /
//! 热键提示变化时通过 muda 句柄 `MenuItem::set_text` / `CheckMenuItem::set_checked`
//! **原地刷新**（gtk 后端就是改现有 `gtk::MenuItem` 的 label / active，事件连线
//! 与 DBus 导出对象保持不变），全生命周期不再调用 `set_menu`。
//!
//! P1 #10 的"暂停/恢复热键"用 muda `CheckMenuItemBuilder`，勾选状态反映在菜单上。
//!
//! 已知限制（写入报告）：Linux 上 tray-icon 走 StatusNotifier/AppIndicator，
//! KDE 面板是否派发 `TrayIconEvent::Click` 取决于托盘宿主——左键显示窗口在
//! 部分 AppIndicator 实现上不生效，此时用户从菜单"打开主窗口"进入（功能等价兜底）。

use dioxus::desktop::trayicon::{
    init_tray_icon, menu::{CheckMenuItem, CheckMenuItemBuilder, Menu, MenuItem, MenuItemBuilder, MenuId, PredefinedMenuItem},
    Icon,
};

/// 菜单项 id（MenuEvent::id().0 匹配用；main.rs 的 use_tray_menu_event_handler
/// 分支按这 5 个常量匹配，与菜单对象上挂的 MenuId 必须严格一致——见 tests）
pub const ID_SHOW: &str = "rumi-show";
pub const ID_OCR: &str = "rumi-ocr";
/// P1 #10：跳批量页
pub const ID_BATCH: &str = "rumi-batch";
/// P1 #10：暂停/恢复热键切换（CheckMenuItem）
pub const ID_HOTKEY_TOGGLE: &str = "rumi-hotkey-toggle";
pub const ID_QUIT: &str = "rumi-quit";

/// 托盘菜单 + 各项句柄（O 波）：菜单对象由 [`setup`] 构建一次并交给
/// `init_tray_icon` 常驻，句柄（Rc 共享同一份 MenuChild）留在本结构里，
/// 语言/状态变化走 [`refresh`](Self::refresh) 原地更新。
/// Clone 是 Rc 句柄克隆（与托盘内部共享同一棵子项树），不是重建菜单。
#[derive(Clone)]
pub struct TrayMenu {
    /// 菜单本体（与 init_tray_icon 内部持有的是同一棵 Rc 树；测试枚举 ID 用）
    pub menu: Menu,
    show: MenuItem,
    ocr: MenuItem,
    batch: MenuItem,
    toggle: CheckMenuItem,
    quit: MenuItem,
}

impl TrayMenu {
    /// 语言 / 热键暂停态 / 热键提示变化时**原地**刷新词条与勾选态。
    /// gtk 后端 = 改现有 gtk::MenuItem 的 label / CheckMenuItem::set_active，
    /// 不销毁、不重挂菜单对象，事件连线与 DBus 导出保持稳定（见模块注释）。
    pub fn refresh(&self, hotkey: &str, hotkey_paused: bool) {
        use crate::i18n::{self, k};
        self.show.set_text(i18n::tt(k::TRAY_SHOW));
        let ocr_text = if hotkey_paused {
            i18n::tt(k::TRAY_OCR_PAUSED).to_string()
        } else {
            i18n::fill(i18n::tt(k::TRAY_OCR), hotkey)
        };
        self.ocr.set_text(ocr_text);
        self.batch.set_text(i18n::tt(k::TRAY_BATCH));
        self.toggle.set_text(i18n::tt(k::TRAY_PAUSE_HOTKEY));
        self.toggle.set_checked(hotkey_paused);
        self.quit.set_text(i18n::tt(k::TRAY_QUIT));
    }
}

/// 构建托盘菜单：打开主窗口 / 截图OCR(热键提示) / 批量识别 / 暂停热键(勾选态) / 退出。
/// 全部词条走 i18n（读全局原子语言，见 i18n::tt）；`hotkey_paused` = true 时勾选
/// "暂停热键"且 OCR 菜单项文案提示已暂停。语言/暂停态后续变化不再重建菜单，
/// 由 [`TrayMenu::refresh`] 原地刷新（O 波修复，见模块注释）。
///
/// ⚠️ muda builder 的 `enabled` 字段是 `#[derive(Default)]` 的 bool——**默认 false**
/// （builders/normal.rs:11-14，MenuItemBuilder/CheckMenuItemBuilder 同此）。
/// 不显式 `.enabled(true)`，DBusMenu 导出 `enabled=false`，KDE 托盘菜单整体灰显
/// （busctl 实证：修复前每项 "enabled" b false）。构造函数 `MenuItem::new(text,
/// enabled, ..)` 则要显式传参、无此坑——dioxus 官方示例走构造函数所以没踩到。
pub fn build_menu(hotkey: &str, hotkey_paused: bool) -> TrayMenu {
    use crate::i18n::{self, k};
    let show = MenuItemBuilder::new()
        .id(MenuId::new(ID_SHOW))
        .text(i18n::tt(k::TRAY_SHOW))
        .enabled(true)
        .build();
    let ocr_text = if hotkey_paused {
        i18n::tt(k::TRAY_OCR_PAUSED).to_string()
    } else {
        i18n::fill(i18n::tt(k::TRAY_OCR), hotkey)
    };
    let ocr = MenuItemBuilder::new()
        .id(MenuId::new(ID_OCR))
        .text(ocr_text)
        .enabled(true)
        .build();
    let batch = MenuItemBuilder::new()
        .id(MenuId::new(ID_BATCH))
        .text(i18n::tt(k::TRAY_BATCH))
        .enabled(true)
        .build();
    let toggle = CheckMenuItemBuilder::new()
        .id(MenuId::new(ID_HOTKEY_TOGGLE))
        .text(i18n::tt(k::TRAY_PAUSE_HOTKEY))
        .checked(hotkey_paused)
        .enabled(true)
        .build();
    let quit = MenuItemBuilder::new()
        .id(MenuId::new(ID_QUIT))
        .text(i18n::tt(k::TRAY_QUIT))
        .enabled(true)
        .build();
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &show,
        &PredefinedMenuItem::separator(),
        &ocr,
        &batch,
        &PredefinedMenuItem::separator(),
        &toggle,
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    TrayMenu { menu, show, ocr, batch, toggle, quit }
}

/// 32×32 RGBA 托盘图标：复用 icon.rs 的 256px 应用图标等比缩放（CatmullRom），
/// 与窗口图标同一视觉来源（原先此处是一套独立的手绘放大镜，形象不统一）。
fn make_icon() -> Icon {
    let rgba = crate::icon::tray_icon_rgba();
    Icon::from_rgba(rgba, 32, 32).expect("托盘图标构造失败")
}

/// App 组件渲染期调用**一次**：构建菜单并初始化托盘。返回的 [`TrayMenu`]
/// 由 App 持有（use_hook），语言/暂停态变化时调用 `refresh` 原地刷新；
/// 全生命周期不再 `TrayIcon::set_menu`（O 波根因，见模块注释）。
pub fn setup(hotkey: &str, hotkey_paused: bool) -> TrayMenu {
    let handles = build_menu(hotkey, hotkey_paused);
    // Menu 是 Rc 克隆语义：交给 dioxus 的这份与 handles.menu 共享同一棵子项树
    let _tray = init_tray_icon(handles.menu.clone(), Some(make_icon()));
    handles
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::desktop::trayicon::menu::MenuItemKind;

    /// 菜单中实际挂的 MenuId 序列（跳过无业务 ID 的分隔符等 Predefined 项）
    fn business_ids(handles: &TrayMenu) -> Vec<String> {
        handles
            .menu
            .items()
            .iter()
            .filter_map(|it| match it {
                MenuItemKind::MenuItem(i) => Some(i.id().0.clone()),
                MenuItemKind::Check(c) => Some(c.id().0.clone()),
                MenuItemKind::Icon(i) => Some(i.id().0.clone()),
                MenuItemKind::Submenu(s) => Some(s.id().0.clone()),
                MenuItemKind::Predefined(_) => None,
            })
            .collect()
    }

    /// O 波 #1 锚定测试：菜单对象上挂的 MenuId 必须与 main.rs
    /// use_tray_menu_event_handler 分支引用的 5 个 ID 常量完全一致（含顺序）。
    /// 两侧字符串此前靠手写同步，拼写断链 = "菜单可见但点击无反应"；
    /// handler 分支直接引用这里的常量（编译期防错），本测试保证菜单构建侧
    /// 不会写错/漏挂任何一个 ID。
    #[test]
    fn menu_item_ids_match_handler_constants() {
        let handles = build_menu("alt+q", false);
        assert_eq!(
            business_ids(&handles),
            vec![
                ID_SHOW.to_string(),
                ID_OCR.to_string(),
                ID_BATCH.to_string(),
                ID_HOTKEY_TOGGLE.to_string(),
                ID_QUIT.to_string(),
            ]
        );
        // 与 build_menu 的字段一一对应（防结构体字段接错项）
        assert_eq!(handles.show.id().0, ID_SHOW);
        assert_eq!(handles.ocr.id().0, ID_OCR);
        assert_eq!(handles.batch.id().0, ID_BATCH);
        assert_eq!(handles.toggle.id().0, ID_HOTKEY_TOGGLE);
        assert_eq!(handles.quit.id().0, ID_QUIT);

        // 灰菜单回归守卫：muda builder 的 enabled 默认 false（derive(Default)），
        // 不显式 .enabled(true) 会导致 DBusMenu 导出 enabled=false、托盘菜单整体
        // 灰显不可点（busctl 实证过）。任何一项误删 .enabled(true) 此处即失败。
        for name in ["show", "ocr", "batch", "toggle", "quit"] {
            let enabled = match name {
                "show" => handles.show.is_enabled(),
                "ocr" => handles.ocr.is_enabled(),
                "batch" => handles.batch.is_enabled(),
                "toggle" => handles.toggle.is_enabled(),
                _ => handles.quit.is_enabled(),
            };
            assert!(enabled, "托盘菜单项 {name} 不可用（缺 .enabled(true)？）");
        }
    }

    /// O 波 #1 修复行为：refresh 原地更新 文案 + 勾选态 + 语言，不重建菜单
    /// （菜单对象里的 MenuId 序列在 refresh 前后不变 = 菜单对象未替换）。
    #[test]
    fn refresh_updates_texts_checked_and_keeps_menu_identity() {
        crate::i18n::set_lang("zh_CN");
        let handles = build_menu("alt+q", false);
        assert!(!handles.toggle.is_checked());
        assert_eq!(handles.ocr.text(), "截图OCR（alt+q）");

        // 暂停 + 换热键 + 切英文：句柄原地刷新
        crate::i18n::set_lang("en_US");
        handles.refresh("ctrl+shift+a", true);
        assert!(handles.toggle.is_checked(), "暂停态应反映在勾选项上");
        assert_eq!(handles.ocr.text(), "Screenshot OCR (hotkey paused — use this menu)");
        assert_eq!(handles.show.text(), "Open main window");
        assert_eq!(handles.toggle.text(), "Pause hotkey");
        // 菜单 ID 序列不变（未发生整体替换）
        assert_eq!(
            business_ids(&handles),
            vec![
                ID_SHOW.to_string(),
                ID_OCR.to_string(),
                ID_BATCH.to_string(),
                ID_HOTKEY_TOGGLE.to_string(),
                ID_QUIT.to_string(),
            ]
        );

        // 恢复：非暂停态 OCR 项回到"热键提示"文案
        handles.refresh("ctrl+shift+a", false);
        assert!(!handles.toggle.is_checked());
        assert_eq!(handles.ocr.text(), "Screenshot OCR (ctrl+shift+a)");
        crate::i18n::set_lang("zh_CN");
    }
}
