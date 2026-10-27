//! picker —— M 波选择器 UI 组件。
//!
//! ## FrozenPicker（内置冻结帧选择器）
//!
//! 主管裁决的形态：**主窗口临时全屏方案**（规避 dioxus 多窗口 API 风险）。
//! 流程在 [`crate::ocr_flow::capture_region_builtin`]：抓全屏帧（抓帧前隐藏主窗，
//! 冻结帧不含自己）→ 主窗转全屏 → App 检测 `ctx.picker` 非空后**整页切换**为
//! 本组件（侧栏/标题栏/内容全部不渲染，等价"盖住一切"的专属遮罩）→ 拖选矩形
//! → 松手裁剪进识别管线 / ESC 取消。
//!
//! 交互骨架复用识别页预览划选（main.rs 注释的事件选型）：wrapper 上
//! mousedown/move/up + `MouseData::element_coordinates()`（相对 stage 元素，
//! img 设 pointer-events:none 后恒为 stage 坐标）+ `onmounted` 采集显示尺寸。
//! stage 铺满全屏窗口、img `object-fit:fill`——全屏窗口与抓帧屏幕同屏同比例，
//! fill 即 1:1；即便多屏比例不符，fill 的线性映射仍保证裁剪坐标正确。
//!
//! 主题：顶栏文字 --fg/--panel、框线 --primary，深浅共用一套选择器样式。
//!
//! ## WindowPickerModal（窗口选择列表面）
//!
//! xcap 平台（X11/Win/mac，caps.window_list=true）"活动窗口"按钮的替代路径：
//! list_windows 枚举 → 点击行 → window_by_id 抓帧 → 识别。Wayland 后端
//! caps 全 false 时按钮隐藏、本组件不出现（安全模型，如实上报）。

use dioxus::html::{input_data::MouseButton, Key};
use dioxus::prelude::*;
use rumi_capture::{Capturer, WindowInfo};

use crate::i18n;
use crate::ocr_flow::{self, CaptureCtx};

/// 冻结帧选择器（全屏态专属组件；App 在 `ctx.picker` 非空时整页渲染它）。
#[component]
pub fn FrozenPicker(ctx: CaptureCtx, url: String) -> Element {
    // drag/stage_px 的外层绑定不直接 set（闭包内重绑后用），cur/stage_mounted 直接 set
    let drag = use_signal(|| Option::<(f64, f64)>::None);
    let mut cur = use_signal(|| (0.0f64, 0.0f64));
    // stage 显示尺寸（= 全屏窗口客户区 CSS px）；onmounted 初采 + mousedown 重采
    let stage_px = use_signal(|| (0.0f64, 0.0f64));
    let mut stage_mounted = use_signal(|| Option::<MountedEvent>::None);

    let drag_v = drag();
    let cur_v = cur();
    let sel_rect: Option<(f64, f64, f64, f64)> = drag_v.map(|(x0, y0)| {
        let (x1, y1) = cur_v;
        (x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
    });
    // 顶栏实时尺寸：选区显示尺寸 × (自然/显示) 比例 = OCR 实际拿到的像素
    let stage_v = stage_px();
    let frame_dims = ocr_flow::picker_frame_dims();
    let size_label = match (sel_rect, frame_dims, stage_v) {
        (Some((_, _, rw, rh)), Some((nw, nh)), (sw, sh)) if sw >= 1.0 && sh >= 1.0 => {
            format!("{} × {}", (rw * nw as f64 / sw).round() as u32, (rh * nh as f64 / sh).round() as u32)
        }
        _ => String::new(),
    };

    rsx! {
        div {
            class: "frozen-stage",
            tabindex: "0",
            // 挂载即抢焦点：ESC 键随时可退出选择态
            onmounted: move |e: MountedEvent| async move {
                stage_mounted.set(Some(e.clone()));
                let _ = e.set_focus(true).await;
            },
            onkeydown: move |ev: KeyboardEvent| {
                if ev.is_composing() { return; }
                if ev.key() == Key::Escape {
                    ocr_flow::cancel_builtin_picker(ctx);
                }
            },
            // 拖选骨架与识别页划选同构：mousedown 重采显示尺寸后开始，move 钳制，up/出界提交
            onmousedown: move |e: MouseEvent| {
                if e.trigger_button() != Some(MouseButton::Primary) {
                    return;
                }
                let p = e.element_coordinates();
                let mounted = stage_mounted();
                let mut stage_px = stage_px;
                let mut drag = drag;
                let mut cur = cur;
                spawn(async move {
                    let mut usable = false;
                    if let Some(m) = mounted {
                        if let Ok(r) = m.get_client_rect().await {
                            let (w, h) = (r.size.width, r.size.height);
                            stage_px.set((w, h));
                            usable = w >= 1.0;
                        }
                    }
                    if usable {
                        drag.set(Some((p.x, p.y)));
                        cur.set((p.x, p.y));
                    }
                });
            },
            onmousemove: move |e: MouseEvent| {
                if drag().is_none() { return; }
                let p = e.element_coordinates();
                let (w, h) = stage_px();
                cur.set((p.x.clamp(0.0, w), p.y.clamp(0.0, h)));
            },
            onmouseup: move |_| commit_frozen_selection(drag, cur, stage_px, ctx),
            onmouseleave: move |_| {
                // 拖出窗口：钳到边缘并提交（无指针捕获时的兜底）
                if drag().is_some() {
                    let (w, h) = stage_px();
                    let (x, y) = cur();
                    cur.set((x.clamp(0.0, w), y.clamp(0.0, h)));
                    commit_frozen_selection(drag, cur, stage_px, ctx);
                }
            },
            img { src: "{url}" }
            if let Some((rx, ry, rw, rh)) = sel_rect {
                div {
                    class: "sel-rect",
                    style: "left: {rx}px; top: {ry}px; width: {rw}px; height: {rh}px;",
                }
            }
            div { class: "frozen-topbar",
                span { "{i18n::tt(i18n::k::PICKER_HINT)}" }
                if !size_label.is_empty() {
                    span { class: "frozen-dim", "{size_label}" }
                }
            }
        }
    }
}

/// 拖选提交：清框 → 裁剪/识别尾段在 [`ocr_flow::finish_builtin_region`]（busy
/// 与取消语义都在那边统一处理；选区过小按取消处理，不识别不报错）。
///
/// 必须用 `spawn_forever`（根作用域）：任务执行到一半 `picker.set(None)` 会把
/// 本组件（FrozenPicker）卸载，普通 `spawn` 挂在组件作用域上会被连带取消，
/// 识别尾段就此中断——这正是 dioxus-core spawn 文档里写的卸载清理语义。
fn commit_frozen_selection(
    mut drag: Signal<Option<(f64, f64)>>,
    cur: Signal<(f64, f64)>,
    stage_px: Signal<(f64, f64)>,
    ctx: CaptureCtx,
) {
    let Some((x0, y0)) = drag() else {
        return;
    };
    let (x1, y1) = cur();
    drag.set(None);
    let disp = stage_px();
    dioxus::core::spawn_forever(async move {
        ocr_flow::finish_builtin_region(ctx, (x0, y0, x1, y1), disp).await;
    });
}

/// 窗口选择列表（M 波任务 3）：xcap 平台"活动窗口"的替代路径。
/// `loading` = 枚举中；`on_pick` = 选中回调（MainTab 里抓帧识别）。
#[component]
pub fn WindowPickerModal(
    mut open: Signal<bool>,
    windows: Vec<WindowInfo>,
    loading: bool,
    on_pick: Callback<WindowInfo>,
) -> Element {
    let close_word = "✕";
    rsx! {
        div {
            class: "modal-backdrop",
            // 点遮罩空白处关闭；卡片内点击不冒泡
            onmousedown: move |_| open.set(false),
            tabindex: "0",
            onmounted: move |e: MountedEvent| async move {
                let _ = e.set_focus(true).await;
            },
            onkeydown: move |ev: KeyboardEvent| {
                if ev.key() == Key::Escape {
                    open.set(false);
                }
            },
            div {
                class: "modal-card",
                onmousedown: move |e: MouseEvent| e.stop_propagation(),
                div { class: "modal-head",
                    span { "{i18n::tt(i18n::k::WINPICK_TITLE)}" }
                    button {
                        class: "btn-sm",
                        title: "{i18n::tt(i18n::k::PICKER_CANCEL)}",
                        onclick: move |_| open.set(false),
                        "{close_word}"
                    }
                }
                if loading {
                    div { class: "hint", style: "padding: 14px;", "{i18n::tt(i18n::k::STA_PROBING)}" }
                } else if windows.is_empty() {
                    div { class: "hint", style: "padding: 14px;", "{i18n::tt(i18n::k::WINPICK_EMPTY)}" }
                } else {
                    div { class: "win-list",
                        for w in windows {
                            button {
                                class: "win-row",
                                key: "{w.id}",
                                title: "{w.title}",
                                onclick: move |_| on_pick.call(w.clone()),
                                span { class: "win-title", "{w.title}" }
                                span { class: "win-sub", "id {w.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// MainTab 的窗口选择状态束：按钮点击 → 打开模态 + 枚举；选中 → 抓帧识别。
/// 识别尾段与捕获按钮同管线（busy/状态行/历史全部统一）。
#[derive(Clone, Copy)]
pub struct WindowPicker {
    pub open: Signal<bool>,
    pub list: Signal<Vec<WindowInfo>>,
    pub loading: Signal<bool>,
}

impl WindowPicker {
    /// 打开模态并枚举窗口（Wayland 后端会枚举失败 → 关模态 + 状态行报错；
    /// 正常情况下按钮已被 caps 隐藏，这里只是兜底）。
    pub fn open(mut self, mut status: Signal<String>) {
        self.open.set(true);
        self.loading.set(true);
        self.list.set(Vec::new());
        // spawn_forever：枚举结果要可靠落盘到信号（模态自身/宿主页都可能已卸载）
        dioxus::core::spawn_forever(async move {
            let r = match Capturer::shared().await {
                Ok(c) => c.list_windows().await,
                Err(e) => Err(e),
            };
            match r {
                Ok(v) => {
                    self.list.set(v);
                    self.loading.set(false);
                }
                Err(e) => {
                    self.open.set(false);
                    self.loading.set(false);
                    status.set(i18n::fill(i18n::tt(i18n::k::WINPICK_LOAD_FAIL), e));
                }
            }
        });
    }
}

/// 窗口选择器选中 → 抓帧 → 识别（与捕获按钮同尾段；busy/状态行统一）。
/// 本机 Wayland 无法真实走通（caps 隐藏按钮），编译级交付 + 真机阶段验收。
pub fn capture_window(ctx: CaptureCtx, w: WindowInfo) {
    let CaptureCtx { mut status, mut busy, .. } = ctx;
    if busy() {
        return;
    }
    busy.set(true);
    let label = i18n::fill(i18n::tt(i18n::k::WIN_LABEL_TPL), &w.title);
    status.set(i18n::fill(i18n::tt(i18n::k::STA_CAPTURING), &label));
    let id = w.id;
    // spawn_forever：点选后模态立即关闭（宿主组件可能一并切换/卸载），
    // 挂在组件作用域的普通 spawn 会被取消，识别尾段中断
    dioxus::core::spawn_forever(async move {
        let r = match Capturer::shared().await {
            Ok(c) => c.window_by_id(id).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(frame) => match ocr_flow::frame_to_data_url(&frame) {
                Ok(url) => {
                    status.set(i18n::filln(
                        i18n::tt(i18n::k::STA_CAP_OK),
                        &[&label, &frame.width, &frame.height],
                    ));
                    ocr_flow::recognize_captured(ctx, frame, url, label).await;
                }
                Err(e) => {
                    status.set(format!("{}{e}", i18n::tt(i18n::k::ERR_MARK)));
                    busy.set(false);
                }
            },
            Err(e) => {
                status.set(i18n::filln(i18n::tt(i18n::k::STA_CAP_FAIL), &[&label, &e]));
                busy.set(false);
            }
        }
    });
}
