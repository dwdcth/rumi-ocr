//! rumi-ui：Rumi OCR 的 Dioxus 0.7 桌面壳。
//!
//! M0：探测后端 → 三种捕获 → base64 data-URL 内嵌显示。
//! M1：捕获 → oar-ocr 识别 → Umi 式"左图右记录"结果面板（逐行点击复制 / 全部复制）。
//! M3b：系统托盘 + 全局热键 + config.toml 持久化 + 引擎切换（native / plugin:<名字>）。
//! Wave1-A（P0 打磨）：左侧图标导航栏 + CSS 变量设计系统 + 设置页控件正确化
//!      + 窗口几何记忆 + 256px"识"字窗口图标 + 空状态与复制反馈。
//! Wave2-C（P1 识别交互 + P2 接入）：结果行 textarea 可编辑（#6）、识别历史
//!      记录栏（#7）、粘贴识别按钮 + 窗口内 Ctrl+V（#8）、预览图拖选复制
//!      （#9）、托盘 批量识别/暂停热键/完成通知（#10）、排版解析下拉（#11）、
//!      设置页忽略规则（#12）、批量页二维码工具卡（#13）、i18n 词条表（#15）。
//!
//! 线程模型：Dioxus Signal 非 Send；捕获与 OCR 重活放 std 线程，
//! 结果经 futures oneshot 通道回 dioxus spawn 内 set 回 UI。
//!
//! 托盘/热键调研结论（详见 tray.rs / hotkey.rs 头注释）：
//! dioxus-desktop 0.7.10 内置 tray-icon 0.21.3 与 global-hotkey 0.7.0，
//! 事件已桥进 tao 事件循环（UserWindowEvent），无需自接 crate / 事件循环；
//! 关闭按钮 = 隐藏由 `WindowCloseBehaviour::WindowHides` 提供，
//! 左键点托盘显示窗口由 `with_tray_icon_show_window_on_click`（默认 true）提供。
//!
//! P1 #8/#9 事件选型（读 dioxus-html 0.7.10 源码确认的写法）：
//! - 键盘：rsx `onkeydown`，事件数据 `KeyboardEvent = Event<KeyboardData>`，
//!   `e.key()` 返回 `keyboard_types::Key`（Ctrl+V 即
//!   `Key::Character("v")`），修饰键走 `ModifiersInteraction::modifiers()`
//!   返回 `keyboard_types::Modifiers` 位集（`Modifiers::CONTROL`）；
//!   dioxus-desktop 经 `serializeKeyboardEvent`（key/alt_key/ctrl_key/...）
//!   同步回传，桌面端语义与 Web 一致。
//! - 划选：预览图外层 wrapper 上挂 `onmousedown/onmousemove/onmouseup/
//!   onmouseleave`，坐标取 `MouseData::element_coordinates()`（相对目标节点
//!   padding 缘，img 设 pointer-events:none 后恒为 wrapper 坐标，滚动/缩放安全）；
//!   显示尺寸由 wrapper `onresize`（ResizeObserver：布局定型/换图/窗口
//!   resize 均触发，事件自带 border-box 尺寸）唯一维护，mousedown 按下时经
//!   句柄 `get_client_rect()` 现场重采作划选基准（webview 里
//!   getBoundingClientRect 的 CSS 像素，与鼠标坐标同空间），
//!   自然像素坐标 = 元素坐标 × (自然宽 / 显示宽)。

mod batch;
mod hotkey;
mod i18n;
mod icon;
mod logger;
mod ocr_flow;
mod picker;
mod plugins_ui;
mod settings;
mod tray;

use dioxus::desktop::{
    use_muda_event_handler, use_tray_icon_event_handler, use_wry_event_handler, Config, LogicalSize,
    WindowBuilder, WindowCloseBehaviour, WindowEvent,
};
use dioxus::html::{Key, Modifiers};
use dioxus::prelude::*;
use dioxus::desktop::tao::event::Event as TaoEvent;
use hotkey::HotkeyBridge;
use ocr_flow::{CaptureCtx, EngineState, Kind, UiLine};
use rumi_capture::{Caps, Capturer};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// tab 索引（rsx 里用 usize 比较，避免枚举比较的手写 PartialEq 噪音）。
/// RUMI_START_TAB 调试变量沿用此编号：1=插件 2=设置 3=批量。
const TAB_MAIN: usize = 0;
const TAB_PLUGINS: usize = 1;
const TAB_SETTINGS: usize = 2;
const TAB_BATCH: usize = 3;

/// 窗口默认 / 最小尺寸（逻辑像素；最小尺寸见 P0 #4）
const DEFAULT_WIN: (f64, f64) = (1000.0, 760.0);
const MIN_WIN: (f64, f64) = (720.0, 520.0);

/// 进程级唯一剪贴板。Wayland 下数据源需常驻才能被粘贴，故不随用随弃（M1 原样）。
fn clipboard() -> Result<MutexGuard<'static, arboard::Clipboard>, String> {
    static CLIP: OnceLock<Result<Mutex<arboard::Clipboard>, String>> = OnceLock::new();
    match CLIP.get_or_init(|| arboard::Clipboard::new().map(Mutex::new).map_err(|e| e.to_string())) {
        Ok(m) => m.lock().map_err(|_| "剪贴板锁被毒化".to_string()),
        Err(e) => Err(e.clone()),
    }
}

fn copy_text(text: &str) -> Result<(), String> {
    clipboard()?
        .set_text(text.to_string())
        .map_err(|e| format!("写入剪贴板: {e}"))
}

/// 选区图像写入剪贴板（P1 #9）：RGBA8 → arboard ImageData。
fn copy_image(rgba: Vec<u8>, w: u32, h: u32) -> Result<(), String> {
    clipboard()?
        .set_image(arboard::ImageData {
            width: w as usize,
            height: h as usize,
            bytes: std::borrow::Cow::Owned(rgba),
        })
        .map_err(|e| format!("写入剪贴板图像: {e}"))
}

/// 把主窗口从托盘里唤回（托盘菜单"打开主窗口"、热键/托盘触发截图时都会调用）
fn show_main_window() {
    let dc = dioxus::desktop::window();
    dc.window.set_visible(true);
    dc.window.set_focus();
}

/// 读取当前窗口几何并写进 settings（关闭隐藏前 / 托盘退出前调用）。
///
/// 咨询结论（2026-10-27，Wayland/GDK 整数缩放错配）后的口径：
/// - **持久化统一存逻辑尺寸**：`inner_size()` 返回物理像素（GDK 在分数缩放屏上
///   按整数 scale=2 出 buffer），save/restore 全程 LogicalSize 才跨缩放稳定；
/// - 最大化/全屏中的几何不是普通态几何，**跳过保存**；
/// - Wayland 客户端拿不到自身位置（outer_position 恒 0/垃圾值），x/y 不更新，
///   沿用上次持久化值且启动时不还原（由合成器摆放）。
fn save_window_state() {
    let win = &dioxus::desktop::window().window;
    if win.is_maximized() || win.fullscreen().is_some() {
        return;
    }
    let size = win.inner_size();
    let scale = win.scale_factor();
    if scale <= 0.0 {
        return;
    }
    let (lw, lh) = (
        (size.width as f64 / scale).round() as u32,
        (size.height as f64 / scale).round() as u32,
    );
    if lw < 300 || lh < 200 {
        return; // 最小化中的假值等兜底
    }
    let prev = settings::get().window;
    let st = settings::WindowState {
        x: prev.map(|p| p.x).unwrap_or(0),
        y: prev.map(|p| p.y).unwrap_or(0),
        w: lw,
        h: lh,
    };
    let _ = settings::update(|s| {
        s.window = Some(st);
        Ok(())
    });
    logger::info("window", format!("[window] 几何已保存(逻辑) {g:?}", g = st));
}

fn main() {
    // 启动读配置（解析警告在设置页显示）
    let _ = settings::init();
    // 初始化运行日志（体验批次）：文件落盘，设置页可查看 / 复制 / 导出
    logger::init();
    // 全局语言原子同步（i18n::tt 在 spawn 闭包里取词的来源）
    i18n::set_lang(&settings::get().lang);
    // 历史缓存目录超 50 个文件删最旧（LRU；识别侧内存上限同 HISTORY_CAP）
    let hist_dir = ocr_flow::history_dir();
    let _ = std::fs::create_dir_all(&hist_dir);
    ocr_flow::prune_history_dir(&hist_dir, ocr_flow::HISTORY_CAP);
    let saved = settings::get().window;
    let frameless = settings::get().frameless;
    let mut builder = WindowBuilder::new()
        .with_title("Rumi OCR")
        .with_inner_size(LogicalSize::new(DEFAULT_WIN.0, DEFAULT_WIN.1))
        .with_min_inner_size(LogicalSize::new(MIN_WIN.0, MIN_WIN.1))
        .with_resizable(true);
    // Wave3 无边框窗口：去掉系统装饰，UI 顶部渲染自定义 TitleBar（拖拽/最小化/
    // 隐藏到托盘）。与热键同口径——**启动时读 settings，改动需重启生效**；设置页
    // 提供 frameless 开关，出问题可关闭回退系统装饰。undecorated 窗口的边缘
    // resize 不需特殊处理：KWin 等合成器对 undecorated 窗口同样提供边缘拖拽。
    if frameless {
        builder = builder.with_decorations(false);
    }
    if let Some(w) = saved {
        // 持久化统一为逻辑尺寸（save_window_state 注释）；只还原尺寸——Wayland 下
        // 客户端位置请求会被合成器忽略（xdg-shell 无定位协议），由 KWin 自行摆放。
        // 越界兜底：旧版本存过物理值（如 2000×1520），或换过显示器后超屏——
        // 超出合理逻辑范围就回退默认尺寸，避免每次启动都拿到放不下的几何。
        let sane = (400..=2560).contains(&w.w) && (300..=1600).contains(&w.h);
        let (rw, rh) = if sane { (w.w, w.h) } else { (DEFAULT_WIN.0 as u32, DEFAULT_WIN.1 as u32) };
        builder = builder.with_inner_size(LogicalSize::new(rw, rh));
    }
    let cfg = Config::default()
        .with_window(builder)
        .with_icon(icon::window_icon())
        // 关闭行为恒为 Hides（隐藏）；"直接退出"模式在 wry CloseRequested 处理器里
        // 动态读 settings 决定（改设置即时生效，无需重启）。
        .with_close_behaviour(WindowCloseBehaviour::WindowHides)
        // 单击托盘不弹窗（菜单走右键）；显示主窗口 = 双击托盘 / 菜单项（见 App 内 handler）
        .with_tray_icon_show_window_on_click(false);
    LaunchBuilder::new().with_cfg(cfg).launch(App);
}

// ── 设计系统（P0 #2）────────────────────────────────────────────────────────
//
// 全部颜色 / 间距 / 圆角 / 字号走 CSS 自定义属性；深浅主题只换 :root 变量表，
// 规则本体共用。按钮三级：primary（填充主色）/ secondary（默认描边）/ danger（红），
// 带 :hover/:active/:focus-visible/:disabled 全态；旧 area/win 杂色 class 已废除。

/// 主题变量表（除变量外深浅无差异）。
///
/// Wave3 对比度审计（WCAG，正文对所用底色 ≥4.5:1）后调整的值：
/// - `--fg-faint`：提示文字（空状态/历史空态/`.hint`）——深色旧值 #68719b 对
///   --bg 仅 3.89:1、浅色旧值 #8b91ad 仅 2.85:1，即用户截图红框"提示文字发灰"
///   的根因；两套分别提到 #8e97bd（6.4:1）/ #656c8c（4.7:1）。
/// - `--ok` `--warn` `--danger`（浅色）：作为状态文字对白底原为 3.4 / 3.6 / 4.4，
///   加深到 #128058 / #8f6314 / #cc3d31（≥4.9:1）。
/// - `--primary`（浅色）：#4f6ef7 上白字仅 4.3:1、做导航高亮文字对底 3.9:1，
///   深化为 #3f5be0（≥5.1:1）；`--primary-strong` 同步让 hover 仍更深一档。
/// 成对补齐的语义变量：`--danger-overlay` / `--danger-overlay-soft`（忽略区域
/// 编辑态/拖拽态半透明红，深浅各自取自家 --danger 的色相）、`--img-bg`
/// （透明 PNG 预览衬底，两套同用白色保证二维码可扫）。
/// 现代化润色批次成对补齐：`--edge`（无边框窗口内描边色，body 用 inset
/// box-shadow 画出窗口边界）、`--shadow`（卡片微投影）。--shadow 的值把颜色
/// 写在长度前（rgba() 0 1px 3px，与 0 1px 3px rgba() 等价）：变量表审计测试
/// 要求每个值以颜色字面量开头，纯长度开头会被判为非颜色。
fn theme_vars(theme: &str) -> &'static str {
    if theme == "light" {
        "--bg:#f3f5fa; --panel:#ffffff; --panel-2:#eef1f9; --secondary:#eef1f9; --border:#c9d0e4; --hover:#e9edf8;
         --fg:#1c2340; --fg-dim:#5a6180; --fg-faint:#656c8c;
         --primary:#3f5be0; --primary-strong:#2f4bd0; --primary-soft:rgba(63,91,224,.10);
         --on-primary:#ffffff; --danger:#cc3d31; --danger-soft:rgba(204,61,49,.08);
         --danger-overlay:rgba(204,61,49,.22); --danger-overlay-soft:rgba(204,61,49,.15);
         --img-bg:#ffffff; --ok:#128058; --warn:#8f6314;
         --edge:#a9b4d4; --shadow:rgba(23,32,64,.07) 0 1px 3px;"
    } else {
        "--bg:#0f1322; --panel:#161c31; --panel-2:#1d2440; --secondary:#1d2440; --border:#272e4e; --hover:#202848;
         --fg:#e8eaf6; --fg-dim:#9aa3c7; --fg-faint:#8e97bd;
         --primary:#5b7bfa; --primary-strong:#7391ff; --primary-soft:rgba(91,123,250,.16);
         --on-primary:#ffffff; --danger:#e06c5f; --danger-soft:rgba(224,108,95,.14);
         --danger-overlay:rgba(224,108,95,.25); --danger-overlay-soft:rgba(224,108,95,.18);
         --img-bg:#ffffff; --ok:#3fc98f; --warn:#d8a13a;
         --edge:#414e86; --shadow:rgba(0,0,0,.35) 0 1px 3px;"
    }
}

fn css(theme: &str) -> String {
    let vars = theme_vars(theme);
    // 原生控件（number spinner / checkbox / select 弹层 / 滚动条）的 UA 渲染随主题：
    // 深色下没有 color-scheme:dark 时控件按浅色渲染（黑字白底，视觉割裂）
    let scheme = if theme == "light" { "light" } else { "dark" };
    format!(
        r#"
:root {{ {vars};
        --radius:12px; --radius-s:8px; --sp1:8px; --sp2:14px; --sp3:22px;
        --fs-s:12px; --fs-m:14px; --fs-l:17px; color-scheme: {scheme}; }}
* {{ box-sizing: border-box; }}
html, body {{ height: 100%; }}
/* width 必须显式：body 是 flex row 容器（Wave3 遗留），#main 作为 flex item
   缺省按内容收缩（shrink-to-fit）——窗口宽 > 内容 max-content（本机实测
   ≈1550.64px，工具栏全展开宽）时 #main 停在内容宽，右侧空白、标题栏按钮
   "消失"，曾误判为 Wayland 最大化上游 bug（完整复盘见 docs/开发记录.md）。 */
#main {{ width: 100%; height: 100%; }}
body {{ margin: 0; font-family: system-ui, "Noto Sans CJK SC", "Segoe UI", sans-serif;
       background: var(--bg); color: var(--fg); font-size: var(--fs-m); line-height: 1.5;
       display: flex; flex-direction: row; overflow: hidden;
       /* 无边框窗口的内描边 = 窗口边界：浅色模式下与白色桌面壁纸可分辨 */
       }}

/* ── 原生表单控件深色审计（M 波用户报障修复）────────────────
   select 闭合态样式正确，但弹开的 option 列表走 UA 默认（WebKitGTK 深色下
   深字深底不可见）——显式声明 option/optgroup 的底色与字色；
   checkbox/number spinner 由 color-scheme 驱动，勾选强调色统一 --primary。 */
select option, select optgroup {{ background: var(--panel); color: var(--fg); }}
input[type="checkbox"] {{ accent-color: var(--primary); }}

/* ── O 波 #3：checkbox + label 组合统一对齐 ───────────────────
   WebKitGTK 原生 checkbox 参与基线对齐（且此前设置页 .opt input 规则给
   checkbox 也吃了 flex:1 被拉伸），表现为"公式/顺带扫二维码/通知"等勾选项
   与文字错位。统一用 .chk 组合：inline-flex 垂直居中 + 固定 15px 方块 +
   行高对齐；新增 checkbox 一律挂在 .chk 组合下（有静态测试锚定） */
label.chk {{ display: inline-flex; align-items: center; gap: 6px; cursor: pointer; line-height: 1.4; }}
label.chk input[type="checkbox"] {{ width: 15px; height: 15px; margin: 0; flex: none; }}

/* ── O 波 #4：设置页控件统一宽度 ───────────────────────────────
   语言/主题/选区方式下拉与 热键/端口 输入统一 200px，标签列 190px 已对齐，
   值列左缘整齐；忽略规则 textarea 保持通栏 */
.ctl {{ width: 200px; }}

/* ── 布局骨架：标题栏（frameless 时）+ 左侧图标导航 + 右侧内容（P0 #1 / Wave3）─ */
.shell {{ display: flex; flex-direction: column; width: 100%; height: 100vh; }}
.shell-body {{ display: flex; flex: 1; min-height: 0; width: 100%; }}
.sidebar {{ flex: 0 0 148px; display: flex; flex-direction: column; gap: 2px;
           background: var(--panel); border-right: 1px solid var(--border);
           padding: 10px 8px; overflow-y: auto; }}
.brand {{ display: flex; align-items: center; gap: 8px; font-weight: 700;
         font-size: var(--fs-l); padding: 4px 8px 14px; white-space: nowrap; }}
.brand-mark {{ width: 24px; height: 24px; border-radius: 7px;
              background: linear-gradient(135deg, var(--primary), var(--primary-strong));
              color: var(--on-primary); display: flex; align-items: center; justify-content: center;
              font-size: 13px; font-weight: 700; flex: 0 0 auto; }}
.content {{ flex: 1; min-width: 0; overflow-y: auto; padding: var(--sp2) var(--sp3);
           display: flex; flex-direction: column; }}

/* ── 自定义标题栏（Wave3 无边框窗口）────────────────────────
   高 40px、横贯侧栏+内容区：左 = 拖拽区（应用名，onmousedown 起 GTK 移动），
   右 = 最小化 / 隐藏到托盘 图形按钮（13px）。按钮走设计系统：--panel 底、
   --hover 反馈；关闭钮 hover 用 --danger。文字不可选中避免拖拽时反蓝。 */
.titlebar {{ flex: 0 0 40px; height: 40px; display: flex; align-items: stretch;
            background: var(--panel); border-bottom: 1px solid var(--border);
            user-select: none; -webkit-user-select: none; }}
.tb-drag {{ flex: 1; display: flex; align-items: center; padding: 0 16px;
           font-size: var(--fs-s); color: var(--fg-dim); cursor: default; }}
.tb-name {{ font-weight: 600; letter-spacing: .2px; }}
.tb-actions {{ display: flex; align-items: stretch; }}
button.tb-btn {{ width: 46px; padding: 0; border: none; border-radius: 0;
                background: transparent; color: var(--fg-dim); font-size: 13px;
                display: flex; align-items: center; justify-content: center;
                transition: background .15s, color .15s; }}
button.tb-btn:hover:not(:disabled) {{ background: var(--hover); color: var(--fg); border: none; }}
button.tb-btn:active:not(:disabled) {{ transform: none; }}
button.tb-btn.tb-close:hover:not(:disabled) {{ background: var(--danger); color: var(--on-primary); }}

/* 运行日志卡片（体验批次）：头部行 + 只读等宽文本区 */
.log-card {{ margin-bottom: var(--sp2); }}
.log-head {{ display: flex; align-items: center; gap: 12px; }}
.log-head .grow {{ flex: 1; min-width: 0; }}
.log-view {{ margin-top: 10px; font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
            font-size: 12px; line-height: 1.5; white-space: pre; overflow-x: auto; }}

/* ── 字号层级：h1 → .page-title → 正文 → .hint（P0 #2）────── */
h1 {{ font-size: 20px; font-weight: 700; margin: 0 0 var(--sp2); }}
.page-title {{ font-size: var(--fs-l); font-weight: 600; }}
.hint {{ font-size: var(--fs-s); color: var(--fg-faint); }}

/* ── 按钮体系：secondary（默认）/ primary / danger 全态 ────── */
button {{ font: inherit; font-size: var(--fs-m); padding: 7px 16px; border-radius: var(--radius-s);
         border: 1px solid var(--border); background: var(--secondary); color: var(--fg);
         cursor: pointer;
         transition: background .15s, border-color .15s, color .15s, box-shadow .18s; }}
button.secondary {{ background: var(--secondary); }}
button:hover:not(:disabled) {{ background: var(--hover); border-color: var(--fg-faint); }}
button:active:not(:disabled) {{ transform: translateY(1px); }}
button:focus-visible {{ outline: 2px solid var(--primary); outline-offset: 1px; }}
button:disabled {{ opacity: .5; cursor: not-allowed; }}
button.primary {{ background: var(--primary); border-color: var(--primary); color: var(--on-primary); }}
button.primary:hover:not(:disabled) {{ background: var(--primary-strong); border-color: var(--primary-strong);
                                     box-shadow: 0 2px 10px var(--primary-soft); }}
button.danger {{ background: transparent; border-color: var(--danger); color: var(--danger); }}
button.danger:hover:not(:disabled) {{ background: var(--danger-soft); border-color: var(--danger); color: var(--danger); }}
button.btn-sm {{ padding: 3px 10px; font-size: var(--fs-s); }}

/* 导航项（button 之上覆盖；写在按钮规则后，特异性更高） */
button.nav-item {{ background: transparent; border-color: transparent; color: var(--fg-dim);
                  display: flex; align-items: center; gap: 10px; width: 100%;
                  padding: 9px 11px; text-align: left; border-radius: var(--radius-s); }}
button.nav-item svg {{ flex: 0 0 auto; }}
button.nav-item:hover:not(:disabled) {{ background: var(--hover); border-color: transparent; color: var(--fg); }}
button.nav-item.active,
button.nav-item.active:hover:not(:disabled) {{ background: var(--primary-soft); border-color: transparent;
                                              color: var(--primary); font-weight: 600; }}

/* ── 输入控件 ─────────────────────────────────────────────── */
input, select, textarea {{ font: inherit; font-size: var(--fs-m); padding: 7px 10px; border-radius: var(--radius-s);
      border: 1px solid var(--border); background: var(--bg); color: var(--fg); min-width: 0;
      transition: border-color .15s; }}
input:focus-visible, select:focus-visible, textarea:focus-visible {{ outline: 2px solid var(--primary); outline-offset: -1px; }}
textarea {{ resize: vertical; line-height: 1.45; }}
.formrow {{ display: flex; align-items: flex-start; gap: 10px; padding: 8px 0; }}
.formrow > label {{ flex: 0 0 190px; font-size: var(--fs-m); padding-top: 7px; }}
.formrow .field {{ flex: 1; display: flex; flex-direction: column; gap: 4px; min-width: 0; }}
.opt {{ display: flex; align-items: center; gap: 10px; padding: 6px 0; }}
.opt-label {{ flex: 0 0 220px; font-size: var(--fs-m); overflow: hidden;
             text-overflow: ellipsis; white-space: nowrap; }}
.opt input, .opt select {{ flex: 1; }}

/* ── 卡片 / 引擎状态 / 徽标 ───────────────────────────────── */
.card {{ background: var(--panel); border: 1px solid var(--border);
        border-radius: var(--radius); padding: 12px 16px; box-shadow: var(--shadow); }}
.engine-card {{ display: flex; align-items: center; gap: 12px; margin-bottom: var(--sp2); }}
.engine-card .grow {{ flex: 1; min-width: 0; }}
.engine-card-title {{ font-weight: 600; }}
.engine-card-sub {{ font-size: var(--fs-s); color: var(--fg-dim); word-break: break-all; }}
.dot {{ width: 9px; height: 9px; border-radius: 50%; background: var(--fg-faint); flex: 0 0 auto; }}
.dot.ok {{ background: var(--ok); }}
.dot.warn {{ background: var(--warn); }}
.dot.bad {{ background: var(--danger); }}
.chip {{ display: inline-flex; align-items: center; gap: 6px; font-size: var(--fs-s);
        color: var(--fg-dim); background: var(--panel-2); border: 1px solid var(--border);
        border-radius: 999px; padding: 2px 10px; }}

/* ── 状态行 / 页脚 ────────────────────────────────────────── */
.row {{ display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }}
.status {{ font-size: var(--fs-s); color: var(--fg-dim); min-height: 18px;
          margin: var(--sp1) 0; word-break: break-all; }}
.page-status {{ font-size: var(--fs-s); color: var(--fg-dim); margin: 4px 0 10px;
               min-height: 18px; word-break: break-all; }}
.page-status.ok {{ color: var(--ok); }}
.page-status.bad {{ color: var(--danger); }}
.hk {{ font-size: var(--fs-s); }}
.hk-ok {{ color: var(--ok); }}
.hk-warn {{ color: var(--warn); }}
.hk-bad {{ color: var(--danger); }}
.warnline {{ font-size: var(--fs-s); color: var(--warn); word-break: break-all; }}
.footer {{ margin-top: auto; padding-top: var(--sp2); font-size: var(--fs-s);
          color: var(--fg-faint); display: flex; gap: 12px; flex-wrap: wrap; align-items: center; }}

/* ── 页面骨架 / 识别结果 ──────────────────────────────────── */
.page {{ width: 100%; max-width: 980px; }}
/* 宽窗口自适应：识别页/批量页解除上限铺满（预览与结果行随宽伸展）；
   设置/插件页保留 980 上限（表单超宽可读性差） */
.page.page-wide {{ max-width: none; }}
/* 窗口边缘描边覆盖层：body 的 inset shadow 会被子元素不透明背景盖住，必须用
   fixed 顶层覆盖层实现（pointer-events:none 不拦截任何交互） */
.win-frame {{ position: fixed; inset: 0; border: 2px solid var(--edge);
             pointer-events: none; z-index: 999; }}
.page-head {{ display: flex; align-items: center; gap: 12px; margin-bottom: var(--sp2); flex-wrap: wrap; }}
.results {{ display: flex; gap: 10px; margin-top: var(--sp1); width: 100%; align-items: stretch; }}
.shot-pane {{ flex: 0 0 42%; display: flex; align-items: flex-start; justify-content: center;
             background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius);
             overflow: auto; padding: 6px; }}
.shot-pane img {{ max-width: 100%; height: auto; display: block; border-radius: 4px; }}
/* P1 #9：预览划选。wrapper 紧贴 img（inline-block），坐标系 = 图像显示区 */
.shot-wrap {{ position: relative; display: inline-block; max-width: 100%;
             cursor: crosshair; user-select: none; -webkit-user-select: none; }}
.shot-wrap img {{ pointer-events: none; }}
.sel-rect {{ position: absolute; border: 1.5px dashed var(--primary); background: var(--primary-soft);
            pointer-events: none; border-radius: 2px; }}
.sel-hint {{ font-size: var(--fs-s); color: var(--fg-faint); margin-top: 4px; }}
/* 忽略区域（P0 坐标版防水印）：编辑态红色半透明覆盖 + ✕ 删除角标；
   常态细虚线常显、不响应交互；拖选中的临时框 */
.ig-rect {{ position: absolute; pointer-events: none; }}
.ig-rect.editing {{ pointer-events: auto; border: 1.5px solid var(--danger);
                   background: var(--danger-overlay); border-radius: 2px; }}
.ig-rect.idle {{ border: 1px dashed var(--danger); opacity: .75; }}
/* 文字定位高亮（点结果行）：主题色描边 + 柔和填充，不抢图像交互 */
.loc-rect {{ position: absolute; pointer-events: none; border: 2px solid var(--primary);
            background: var(--primary-soft); border-radius: 3px;
            box-shadow: 0 0 0 1px rgba(255,255,255,.6), 0 0 6px var(--primary-soft); }}
.ig-drag {{ position: absolute; border: 1.5px dashed var(--danger);
           background: var(--danger-overlay-soft); pointer-events: none; border-radius: 2px; }}
button.ig-del {{ position: absolute; top: -9px; right: -9px; width: 18px; height: 18px;
                border-radius: 50%; background: var(--danger); color: var(--on-primary);
                border: 1px solid var(--bg); font-size: 11px; line-height: 1; padding: 0;
                display: flex; align-items: center; justify-content: center; }}
button.ig-del:hover:not(:disabled) {{ background: var(--danger); border-color: var(--bg);
                                    color: var(--on-primary); transform: scale(1.12); }}
button.ig-del:active:not(:disabled) {{ transform: scale(.95); }}
.lines-pane {{ flex: 1; min-width: 0; display: flex; flex-direction: column;
              background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); }}
.lines-head {{ display: flex; justify-content: space-between; align-items: center; gap: 8px;
              padding: 8px 10px; border-bottom: 1px solid var(--border);
              font-size: var(--fs-s); color: var(--fg-dim); }}
.lines-actions {{ display: flex; align-items: center; gap: 8px; }}
ul.lines {{ list-style: none; margin: 0; padding: 4px; overflow-y: auto; max-height: 62vh; }}
/* P1 #6：结果行 textarea 化（保留 score 徽标与行复制按钮） */
li.line {{ position: relative; display: flex; align-items: flex-start; gap: 8px;
          padding: 5px 9px; border-radius: var(--radius-s); }}
li.line:hover {{ background: var(--hover); }}
li.line .row-no {{ color: var(--fg-faint); font-size: var(--fs-s); flex: 0 0 auto;
                 min-width: 1.7em; text-align: right; padding-top: 6px; }}
/* 行首数字 = 定位按钮（可定位的行）：指针 + 悬停主题色，明示可点 */
.row-no.locatable {{ cursor: pointer; user-select: none; }}
.row-no.locatable:hover {{ color: var(--primary); font-weight: 600; }}
li.line .line-edit {{ flex: 1; min-width: 0; padding: 4px 8px; font-size: var(--fs-m); }}
li.line .score {{ color: var(--fg-faint); font-size: var(--fs-s); white-space: nowrap;
                flex: 0 0 auto; padding-top: 8px; }}
li.line > button {{ flex: 0 0 auto; }}

/* 复制成功：目标行内"已复制 ✓"1.2s 渐隐（P0 #5） */
.copied-chip {{ position: absolute; right: 52px; top: 50%; transform: translateY(-50%);
               font-size: var(--fs-s); color: var(--ok); background: var(--bg);
               border: 1px solid var(--ok); border-radius: 999px; padding: 0 8px;
               pointer-events: none; animation: copied-fade 1.2s ease forwards; z-index: 2; }}
.copied-chip.inline {{ position: static; transform: none; }}
@keyframes copied-fade {{ 0% {{ opacity: 1; }} 55% {{ opacity: 1; }} 100% {{ opacity: 0; }} }}

/* ── 空状态（P0 #5）───────────────────────────────────────── */
.empty-state {{ border: 2px dashed var(--border); border-radius: var(--radius);
               padding: 44px 24px; display: flex; flex-direction: column; align-items: center;
               gap: 8px; color: var(--fg-faint); margin-top: var(--sp2); text-align: center; }}
.empty-state svg {{ opacity: .8; margin-bottom: 4px; }}
.empty-title {{ font-weight: 500; color: var(--fg); }}
.empty-sub {{ font-size: var(--fs-s); max-width: 440px; }}
.empty-state button {{ margin-top: var(--sp1); }}

/* ── 历史记录（P1 #7）────────────────────────────────────── */
.hist-card {{ margin-top: var(--sp2); }}
.hist-head {{ display: flex; justify-content: space-between; align-items: center; gap: 8px;
             margin-bottom: 6px; }}
.hist-title {{ font-weight: 600; font-size: var(--fs-m); }}
.hist-list {{ display: flex; flex-direction: column; gap: 3px; }}
button.hist-row {{ display: flex; align-items: center; gap: 8px; width: 100%; text-align: left;
                 background: transparent; border-color: transparent; padding: 5px 8px;
                 font-size: var(--fs-s); border-radius: var(--radius-s); }}
button.hist-row:hover {{ background: var(--hover); border-color: transparent; }}
.hist-time {{ color: var(--fg-faint); flex: 0 0 auto; }}
.hist-summary {{ flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
               white-space: nowrap; }}
.hist-count {{ color: var(--fg-dim); flex: 0 0 auto; }}
.hist-empty {{ font-size: var(--fs-s); color: var(--fg-faint); padding: 4px 2px; }}

/* ── 插件页 ───────────────────────────────────────────────── */
.plugin-list {{ display: flex; gap: 8px; flex-wrap: wrap; margin: var(--sp1) 0 var(--sp2); }}
button.plug {{ background: var(--panel); }}
button.plug-sel {{ background: var(--primary); border-color: var(--primary); color: var(--on-primary); }}
.schema {{ background: var(--panel); border: 1px solid var(--border);
          border-radius: var(--radius); padding: 4px 16px 14px; }}
.schema-head {{ font-weight: 600; color: var(--fg-dim); margin: 12px 0 4px; }}

/* ── 批量页 ───────────────────────────────────────────────── */
.file-list {{ display: flex; flex-direction: column; gap: 4px; margin-top: var(--sp1); }}
.file-row {{ display: flex; justify-content: space-between; align-items: center; gap: 10px;
            padding: 5px 6px 5px 10px; font-size: var(--fs-s); border-radius: var(--radius-s);
            background: var(--panel); border: 1px solid var(--border); }}
.file-row span {{ word-break: break-all; }}
.batch-out {{ margin-top: var(--sp1); padding: 10px 14px; background: var(--panel);
             border: 1px solid var(--border); border-radius: var(--radius); }}
.batch-out .err {{ color: var(--danger); }}
.qr-tag {{ color: var(--ok); margin-left: 6px; }}

/* ── M 波：内置冻结帧选择器（主窗口临时全屏）────────────────
   stage 铺满视口盖住一切 UI（App 在选择态整页只渲染该组件）；img fill 铺满，
   pointer-events:none 保证坐标恒为 stage 坐标；顶栏浮于帧上但不拦截拖选。 */
.frozen-stage {{ position: fixed; inset: 0; z-index: 100; background: var(--bg);
                cursor: crosshair; user-select: none; -webkit-user-select: none; }}
.frozen-stage img {{ width: 100%; height: 100%; object-fit: fill; display: block;
                    pointer-events: none; }}
.frozen-topbar {{ position: absolute; top: 0; left: 0; right: 0;
                 display: flex; align-items: center; gap: 16px; padding: 7px 14px;
                 background: var(--panel); color: var(--fg);
                 border-bottom: 1px solid var(--border); font-size: var(--fs-s);
                 pointer-events: none; }}
.frozen-dim {{ color: var(--fg-dim); font-variant-numeric: tabular-nums; }}

/* ── M 波：窗口选择列表面（xcap 平台"活动窗口"路径）───────── */
.modal-backdrop {{ position: fixed; inset: 0; z-index: 60; background: rgba(0,0,0,.45);
                  display: flex; align-items: center; justify-content: center; }}
.modal-card {{ background: var(--panel); border: 1px solid var(--border);
              border-radius: var(--radius); width: min(520px, 86vw); max-height: 70vh;
              display: flex; flex-direction: column;
              box-shadow: 0 18px 48px rgba(0,0,0,.35); }}
.modal-head {{ display: flex; justify-content: space-between; align-items: center; gap: 10px;
              padding: 10px 14px; border-bottom: 1px solid var(--border); font-weight: 600; }}
.win-list {{ overflow-y: auto; padding: 6px; display: flex; flex-direction: column; gap: 3px; }}
button.win-row {{ display: flex; flex-direction: column; align-items: stretch; gap: 1px;
                 text-align: left; background: transparent; border-color: transparent;
                 padding: 7px 10px; border-radius: var(--radius-s); min-width: 0; }}
button.win-row:hover {{ background: var(--hover); border-color: transparent; }}
.win-title {{ font-size: var(--fs-m); overflow: hidden; text-overflow: ellipsis;
             white-space: nowrap; }}
.win-sub {{ font-size: var(--fs-s); color: var(--fg-faint); }}

/* ── 滚动条（现代化润色）：细圆角滑块、hover 提亮、轨道透明 ──────
   thumb 用 2px 透明边框 + background-clip: content-box 做出"悬浮细条"观感；
   hover 只换 background-color，保留 clip 形状不变。 */
::-webkit-scrollbar {{ width: 10px; height: 10px; }}
::-webkit-scrollbar-thumb {{ background: var(--border); border-radius: 5px;
                            border: 2px solid transparent; background-clip: content-box; }}
::-webkit-scrollbar-thumb:hover {{ background-color: var(--fg-faint); }}
::-webkit-scrollbar-track {{ background: transparent; }}
"#
    )
}

// ── 导航（P0 #1）────────────────────────────────────────────────────────────

/// 导航图标种类（SVG path 数据见 [`icon_paths`]）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NavIconName {
    /// 识别：取景框 + 圆点
    Recognize,
    /// 批量：叠层文档
    Batch,
    /// 插件：积木
    Plugins,
    /// 设置：齿轮
    Settings,
}

/// 线性统一（stroke 1.8 / round join）的 24×24 图标 path 集
fn icon_paths(name: NavIconName) -> &'static [&'static str] {
    match name {
        NavIconName::Recognize => &[
            "M4 8.5V6a2 2 0 0 1 2-2h2.5",
            "M15.5 4H18a2 2 0 0 1 2 2v2.5",
            "M20 15.5V18a2 2 0 0 1-2 2h-2.5",
            "M8.5 20H6a2 2 0 0 1-2-2v-2.5",
            "M8.8 12a3.2 3.2 0 1 0 6.4 0a3.2 3.2 0 1 0 -6.4 0",
        ],
        NavIconName::Batch => &[
            "M9.5 4.5H17a2 2 0 0 1 2 2V15",
            "M7 8.5h6a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2z",
        ],
        NavIconName::Plugins => &[
            "M9.6 3.5h4.8a1.6 1.6 0 0 1 1.6 1.6v4.8a1.6 1.6 0 0 1-1.6 1.6H9.6a1.6 1.6 0 0 1-1.6-1.6V5.1a1.6 1.6 0 0 1 1.6-1.6z",
            "M5.1 13h4.8a1.6 1.6 0 0 1 1.6 1.6v4.8a1.6 1.6 0 0 1-1.6 1.6H5.1a1.6 1.6 0 0 1-1.6-1.6v-4.8A1.6 1.6 0 0 1 5.1 13z",
            "M14.1 13h4.8a1.6 1.6 0 0 1 1.6 1.6v4.8a1.6 1.6 0 0 1-1.6 1.6h-4.8a1.6 1.6 0 0 1-1.6-1.6v-4.8a1.6 1.6 0 0 1 1.6-1.6z",
        ],
        NavIconName::Settings => &[
            "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z",
            "M15 12a3 3 0 1 0-6 0a3 3 0 1 0 6 0",
        ],
    }
}

/// 内联 SVG 图标（stroke=currentColor，随导航高亮变色）
#[component]
pub fn NavIcon(name: NavIconName, size: u32) -> Element {
    let px = size.to_string();
    let paths = icon_paths(name);
    rsx! {
        svg {
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 24 24",
            width: "{px}",
            height: "{px}",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            for d in paths.iter() {
                path { d: "{d}" }
            }
        }
    }
}

/// 左侧导航栏：品牌 + 四项（识别/批量/插件/设置）+ 底部说明位
#[component]
fn Sidebar(mut tab: Signal<usize>, lang: Signal<String>) -> Element {
    let lang_v = lang();
    let t = |k: &'static str| i18n::ts(&lang_v, k);
    let tab_v = tab();
    rsx! {
        nav { class: "sidebar",
            div { class: "brand",
                span { class: "brand-mark", "识" }
                "Rumi OCR"
            }
            NavItem { label: t(i18n::k::NAV_RECOGNIZE).to_string(), icon: NavIconName::Recognize,
                active: tab_v == TAB_MAIN, on_click: move |_| tab.set(TAB_MAIN) }
            NavItem { label: t(i18n::k::NAV_BATCH).to_string(), icon: NavIconName::Batch,
                active: tab_v == TAB_BATCH, on_click: move |_| tab.set(TAB_BATCH) }
            NavItem { label: t(i18n::k::NAV_PLUGINS).to_string(), icon: NavIconName::Plugins,
                active: tab_v == TAB_PLUGINS, on_click: move |_| tab.set(TAB_PLUGINS) }
            NavItem { label: t(i18n::k::NAV_SETTINGS).to_string(), icon: NavIconName::Settings,
                active: tab_v == TAB_SETTINGS, on_click: move |_| tab.set(TAB_SETTINGS) }
        }
    }
}

#[component]
fn NavItem(label: String, icon: NavIconName, active: bool, on_click: Callback) -> Element {
    let cls = if active { "nav-item active" } else { "nav-item" };
    rsx! {
        button { class: "{cls}", onclick: move |_| on_click.call(()),
            NavIcon { name: icon, size: 18 }
            span { "{label}" }
        }
    }
}

// ── 自定义标题栏（Wave3 无边框窗口；体验批次加最大化/还原 + 动态关闭行为）────
//
// 仅 settings.frameless（默认开）时渲染，置于界面最顶、横贯侧栏+内容区。
// API 结论（读 ~/.cargo/registry dioxus-desktop 0.7.10 / tao 0.34.8 源码确认）：
// - 拖拽：DesktopContext::drag()（desktop_context.rs:156）= tao
//   `Window::drag_window()`（window.rs:1305，GTK 下 gtk_window_begin_move_drag）
//   并带全屏守卫；官方注释即要求在 onmousedown 里调用。DesktopContext 没有
//   drag_window 方法，须走 `.drag()` 或 `.window.drag_window()`，本处用前者。
// - 双击标题栏拖拽区 / 最大化还原按钮：tao `Window::is_maximized()` 探测 +
//   `set_maximized(to)` 切换，以窗口实际状态回写信号驱动按钮图形与 tooltip
//   （titlebar_toggle_maximize，双击与按钮共用）。
// - 最小化：tao `Window::set_minimized(true)`（window.rs:955）。
// - 关闭按钮行为动态（close_to_tray ? 隐藏 : 退出）：onclick 现场读 settings，
//   设置页改完即时生效、无需重启；托盘模式 = 存几何后 set_visible(false)
//   （与启动时注册的 WindowCloseBehaviour::WindowHides 同口径），退出模式 =
//   存几何后 exit(0)（与托盘菜单"退出"同口径）。tooltip 只是提示，不锁定行为。
// 边缘 resize 依赖合成器（KWin 对 undecorated 窗口同样提供边缘拖拽），
// 不做特殊处理。

/// 标题栏最小化：tao Window::set_minimized(true)（任务栏/托盘唤回）
fn titlebar_minimize() {
    dioxus::desktop::window().window.set_minimized(true);
}

/// 最大化/还原切换（双击拖拽区与按钮共用）。
/// Wayland 实证（KDE/tao 0.34.8/wry 0.53.5，详见 docs/开发记录.md）：
/// - `set_maximized` 在 undecorated 窗口上触发 webview 渲染失同步（布局整体
///   错乱+输入失效，focus/显隐 nudge 均无法修复）——不可用；
/// - `set_fullscreen(Borderless)` 渲染正常，但输入命中区与渲染错位；进入后
///   追加 `set_focus()` nudge 修复输入区（tao#1218 同类问题的上游经验）。
/// 状态信号**不在这里乐观回写**：由 App 的 wry Resized 处理器同步。
/// 最大化/还原切换（双击拖拽区与按钮共用）。
/// 状态信号**不在这里乐观回写**：最大化是合成器异步确认的，
/// 由 App 的 wry Resized 处理器读 `is_maximized()` 同步给 TitleBar 的按钮图形。
fn titlebar_toggle_maximize() {
    let w = &dioxus::desktop::window().window;
    let to = !w.is_maximized();
    logger::info("window", format!("[window] 最大化切换 -> {to}"));
    w.set_maximized(to);
}

/// 标题栏关闭：行为随设置动态读取（改完即生效）——托盘模式存几何后隐藏；
/// 退出模式存几何后直接退进程（与托盘菜单"退出"同口径）
fn titlebar_close() {
    save_window_state();
    if settings::get().close_to_tray {
        dioxus::desktop::window().window.set_visible(false);
    } else {
        logger::info("window", "[window] 关闭按钮 → 退出程序".into());
        std::process::exit(0);
    }
}

#[component]
fn TitleBar(maximized: Signal<bool>) -> Element {
    // tooltip 随设置/状态走；行为本身在 onclick 里动态读取，不受 tooltip 状态影响
    let close_tip = if settings::get().close_to_tray {
        i18n::tt(i18n::k::TB_HIDE).to_string()
    } else {
        i18n::tt(i18n::k::TB_CLOSE).to_string()
    };
    let max_tip = if maximized() {
        i18n::tt(i18n::k::TB_RESTORE).to_string()
    } else {
        i18n::tt(i18n::k::TB_MAXIMIZE).to_string()
    };
    rsx! {
        div { class: "titlebar",
            div {
                class: "tb-drag",
                // 左键按下即开始拖动窗口；双击 = 最大化/还原（桌面惯例）
                onmousedown: move |e: MouseEvent| {
                    if e.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary) {
                        return;
                    }
                    dioxus::desktop::window().drag();
                },
                ondoubleclick: move |_| titlebar_toggle_maximize(),
                span { class: "tb-name", "Rumi OCR" }
            }
            div { class: "tb-actions",
                button { class: "tb-btn", title: "{i18n::tt(i18n::k::TB_MINIMIZE)}",
                    onclick: move |_| titlebar_minimize(),
                    "—" }
                button { class: "tb-btn", title: "{max_tip}",
                    onclick: move |_| titlebar_toggle_maximize(),
                    if maximized() { "❐" } else { "▢" } }
                button { class: "tb-btn tb-close", title: "{close_tip}",
                    onclick: move |_| titlebar_close(),
                    "✕" }
            }
        }
    }
}

// ── App ─────────────────────────────────────────────────────────────────────

#[component]
fn App() -> Element {
    let theme = use_signal(|| settings::get().theme);
    // P2 #15：UI 语言信号（settings.lang 初始值），保存设置即时生效
    let lang = use_signal(|| settings::get().lang);
    let mut tab = use_signal(|| {
        // 调试/巡场用：RUMI_START_TAB=1/2/3 直接打开指定页（发布版无副作用）
        std::env::var("RUMI_START_TAB")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(TAB_MAIN)
    });
    let shot = use_signal(|| Option::<String>::None);
    let shot_size = use_signal(|| (0u32, 0u32));
    let mut status = use_signal(|| i18n::tt(i18n::k::STATUS_READY).to_string());
    let backend = use_signal(|| i18n::tt(i18n::k::STA_PROBING).to_string());
    // M 波：能力位以结构持有（活动窗口按钮的三态行为、冻结帧选择器自动接管都以它驱动）；
    // caps_text 是页脚展示串（成功=能力位明细，失败=探测错误原文）
    let caps = use_signal(|| Option::<Caps>::None);
    let caps_text = use_signal(|| "…".to_string());
    let engine_st = use_signal(|| EngineState::Init);
    let lines = use_signal(Vec::<UiLine>::new);
    // P1 #6：结果文本可编辑版；P1 #7：历史记录（含磁盘缓存缺失标记）
    let texts = use_signal(Vec::<String>::new);
    // 排版热重排原料：最近一次识别的原始行 + 管线类别。
    // 识别完成时与 lines/texts 同步写入；切换排版下拉对它重 derive_display，
    // 已有结果立即按新排版重解析（Umi 同款体验，无需重新截图识别）。
    let raw = use_signal(|| None::<(Vec<rumi_ocr::OcrLine>, ocr_flow::ProcKind)>);
    // 文字定位：点结果行 → 预览图高亮对应区域（Some((行号, 图像像素 bbox))）
    let locate = use_signal(|| None::<(usize, [f32; 4])>);
    let history = use_signal(Vec::<ocr_flow::HistoryItem>::new);
    let hist_missing = use_signal(std::collections::BTreeSet::<u64>::new);
    let busy = use_signal(|| false);
    let engine_text = use_signal(|| settings::get().engine);
    let hotkey_hint = use_signal(|| settings::get().hotkey.clone());
    // P1 #10：热键暂停态（托盘勾选项持久化在 settings.hotkey_paused）
    let mut hotkey_paused = use_signal(|| settings::get().hotkey_paused);
    let page_status = use_signal(|| "选择插件自动读取其配置 schema；可保存 options、切换引擎".to_string());
    // M 波：内置冻结帧选择器激活态（Some(全屏帧 data-url) = 主窗口全屏拖选态）
    let picker = use_signal(|| Option::<String>::None);
    // 窗口最大化态（TitleBar 按钮图形用）：由 wry Resized 事件读 is_maximized 同步
    // ——最大化是合成器异步确认的，不在点击处理器里乐观回写（咨询结论）
    let mut win_maximized = use_signal(|| false);

    let ctx = CaptureCtx {
        shot,
        shot_size,
        status,
        engine_st,
        lines,
        texts,
        raw,
        locate,
        history,
        hist_missing,
        busy,
        picker,
    };

    // 热键的"截图OCR"触发：与主界面按钮同流程；热键暂停时只提示（托盘 OCR 不受影响）
    let trigger_region = Callback::new(move |_: ()| {
        if hotkey_paused() {
            status.set(i18n::tt(i18n::k::HOTKEY_PAUSED_MSG).to_string());
            return;
        }
        show_main_window();
        ocr_flow::capture_and_ocr(ctx, Kind::Region);
    });

    // 挂载后：探测后端（M0 原样）+ 预热 native OCR 引擎（后台线程，含首次模型下载；
    // 即使当前引擎是插件，预热也无害且便于切回）
    let mut backend = backend;
    let mut caps = caps;
    let mut caps_text = caps_text;
    let mut engine_st = engine_st;
    use_hook(move || {
        // 缩放系数进日志（排障关键值：GDK 在分数缩放屏上按整数 scale 出 buffer，
        // inner_size 是物理像素——几何换算/持久化口径见 save_window_state 注释）
        logger::info(
            "window",
            format!("[window] scale_factor={}", dioxus::desktop::window().window.scale_factor()),
        );
        spawn(async move {
            match Capturer::shared().await {
                Ok(c) => {
                    let caps_v = c.caps();
                    backend.set(c.id().to_string());
                    caps.set(Some(caps_v));
                    caps_text.set(format_caps(caps_v));
                }
                Err(e) => {
                    backend.set(i18n::tt(i18n::k::NO_BACKEND).to_string());
                    // 能力位保持 None（UI 按"能力未知"降级）；错误文本进页脚
                    caps.set(None);
                    caps_text.set(format!("{e}"));
                }
            }
        });
        spawn(async move {
            let rx = ocr_flow::run_blocking(|| rumi_ocr::OcrEngine::global().warmup());
            match rx.await {
                Ok(Ok(())) => engine_st.set(EngineState::Ready),
                Ok(Err(e)) => engine_st.set(EngineState::Error(e.to_string())),
                Err(_) => engine_st.set(EngineState::Error(
                    i18n::tt(i18n::k::ENG_THREAD_LOST).to_string(),
                )),
            }
        });
        // O 波 #6：公式引擎后台预热（settings.formula_mode 开时与文字引擎并列）。
        // 避免用户开公式后首次识别才触发 ~230MB 模型下载导致长卡顿；预热成功后
        // 打上 STRUCTURED_WARM 标记（首跑提示不再出现）。状态行只提示"预热中"，
        // 完成/失败仅在状态行仍停留在本条预热提示时回写——绝不覆盖用户已经开始的
        // 捕获/识别状态消息；预热失败不致命（首次公式识别会兜底再构建）。
        if settings::get().formula_mode {
            spawn(async move {
                let warming = i18n::tt(i18n::k::STA_FORMULA_WARMING).to_string();
                status.set(warming.clone());
                let rx = ocr_flow::run_blocking(|| rumi_ocr::OcrEngine::global().warmup_structured());
                match rx.await {
                    Ok(Ok(())) => {
                        ocr_flow::mark_structured_warm();
                        if status() == warming {
                            status.set(i18n::tt(i18n::k::STA_FORMULA_WARMED).to_string());
                        }
                    }
                    Ok(Err(e)) => {
                        if status() == warming {
                            status.set(format!("{}{e}", i18n::tt(i18n::k::ERR_MARK)));
                        }
                    }
                    Err(_) => {} // 预热线程丢失：静默，首次识别兜底
                }
            });
        }
    });

    // 托盘：组件渲染期初始化**一次**（dioxus 内置桥接，见 tray.rs 头注释）。
    // P1 #10：菜单带 批量识别 + 暂停热键勾选项。
    // O 波 #1 修复：菜单只在此构建一次并常驻，语言/暂停态变化经 TrayMenu::refresh
    // 原地刷新词条与勾选态（D 波的"use_effect 里 TrayIcon::set_menu 整体重建"已被
    // 移除——重建导致 Linux/AppIndicator 侧点击事件断链，根因记录在 tray.rs）。
    let hk_for_menu = hotkey_hint();
    let paused_for_menu = hotkey_paused();
    let tray_menu = use_hook(move || tray::setup(&hk_for_menu, paused_for_menu));
    // ⚠️ 这里用 use_muda_event_handler 而非 use_tray_menu_event_handler——
    // dioxus-desktop 0.7.10 上游 bug：App::new 先注册 menubar 的 muda 全局
    // handler、再注册 tray 的（tray_icon::menu 就是 muda 的再导出，同一个
    // OnceCell），后者 set() 静默失败 → TrayMenuEvent 永不产生，托盘点击全部
    // 被路由进 MudaMenuEvent 且无人处理。本应用无 menubar，MudaMenuEvent 即
    // 托盘事件，故直接挂在 muda handler 上（busctl 合成 Event + 最小复现实证）。
    use_muda_event_handler(move |ev| {
        // 排障插桩：托盘菜单事件是否送达（KDE→dbusmenu Event→gtk→muda→tao 链路）
        logger::info("tray", format!("[tray] menu event: id={}", ev.id().0));
        match ev.id().0.as_str() {
            tray::ID_SHOW => show_main_window(),
            tray::ID_OCR => {
                show_main_window();
                ocr_flow::capture_and_ocr(ctx, Kind::Region);
            }
            tray::ID_BATCH => {
                show_main_window();
                tab.set(TAB_BATCH);
            }
            tray::ID_HOTKEY_TOGGLE => {
                let new_paused = !settings::get().hotkey_paused;
                let _ = settings::update(|s| {
                    s.hotkey_paused = new_paused;
                    Ok(())
                });
                hotkey_paused.set(new_paused);
                // 菜单文案/勾选由下方 use_effect 的 refresh 原地更新
            }
            // 托盘退出：先按关闭同样口径存窗口几何；worker 是 kill_on_drop 的 python
            // 子进程，父进程退出后 stdin 管道关闭，worker 主循环读 EOF 自行退出
            tray::ID_QUIT => {
                save_window_state();
                std::process::exit(0);
            }
            _ => {}
        }
    });

    // 托盘图标交互：双击 = 显示主窗口；单击不动作（右键出菜单）。
    // 注意：此事件仅在 Windows/macOS 生效——tray-icon 0.21.3 的 GTK 后端
    // 没有图标事件管道（源码确认，见 tray.rs 注释）；Linux 走菜单首项。
    use_tray_icon_event_handler(move |ev| {
        if let dioxus::desktop::trayicon::TrayIconEvent::DoubleClick { button, .. } = ev {
            if *button == dioxus::desktop::trayicon::MouseButton::Left {
                logger::info("tray", "[tray] 双击 → 显示主窗口".into());
                show_main_window();
            }
        }
    });

    // 托盘菜单 i18n + 原地刷新：语言 / 热键暂停态 / 热键提示任一变化都刷新词条。
    // 词条在 tray::refresh 里经 i18n::tt 读全局原子语言，这里先同步原子再刷新，
    // 保证组件路径（ts/Signal）与线程路径（tt/原子）看到同一份语言。
    let menu_for_effect = tray_menu.clone();
    let hotkey_for_menu_effect = hotkey_hint;
    let paused_for_menu_effect = hotkey_paused;
    let lang_for_menu_effect = lang;
    use_effect(move || {
        i18n::set_lang(&lang_for_menu_effect());
        // O 波 #1：只改现有菜单项的文字/勾选，不替换菜单对象（根因见 tray.rs）
        menu_for_effect.refresh(&hotkey_for_menu_effect(), paused_for_menu_effect());
    });

    // 关闭按钮：存几何 + 按设置决定去向。dioxus 对 CloseRequested 的默认处理
    // （WindowHides → set_visible(false)）在 launch.rs 的事件 match 里，而 wry
    // 事件 handler 在其之前的 app.tick() 派发——"直接退出"模式在这里 exit(0)，
    // 隐藏动作来不及执行进程就退了；托盘模式则随之正常隐藏。
    // Resized：最大化/还原是合成器异步确认的，按钮图形状态在这里同步（单窗口
    // 应用直接读全局 window()，无需对 window_id 分流）。
    use_wry_event_handler(move |ev, _| {
        match ev {
            TaoEvent::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                save_window_state();
                if !settings::get().close_to_tray {
                    logger::info("window", "[window] 关闭请求 → 退出程序".into());
                    std::process::exit(0);
                }
            }
            TaoEvent::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                // 按钮图形状态跟踪：最大化是异步确认的，以窗口实况为准
                let m = dioxus::desktop::window().window.is_maximized();
                if win_maximized() != m {
                    win_maximized.set(m);
                }
            }
            _ => {}
        }
    });

    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let tab_v = tab();
    let theme_v = theme();
    let css_text = css(&theme_v);
    let paused_now = hotkey_paused();
    // Wave3：frameless 是启动口径（改动重启生效），渲染体直接读 settings 即可，
    // 不做热切换。
    let frameless_now = settings::get().frameless;
    // M 波：能力位快照给 MainTab（活动窗口按钮三态）+ 页脚展示串
    let caps_v = caps();
    let caps_display = caps_text();
    // M 波：冻结帧选择器激活 → 整页只渲染选择器组件（遮住侧栏/标题栏以外的一切）
    let picker_active = picker();

    rsx! {
        style { "{css_text}" }
        if let Some(url) = picker_active {
            picker::FrozenPicker { ctx, url }
        } else {
        div { class: "shell",
            if frameless_now {
                TitleBar { maximized: win_maximized }
            }
            div { class: "shell-body",
                Sidebar { tab, lang }
                div { class: "content",
                if tab_v == TAB_MAIN {
                    MainTab { ctx, engine_text, caps: caps_v, tab, lang }
                }
                if tab_v == TAB_PLUGINS {
                    plugins_ui::PluginsPage { page_status, engine_text }
                }
                if tab_v == TAB_SETTINGS {
                    SettingsPage { page_status, theme, hotkey_hint, engine_text, engine_st: engine_st(), tab, lang }
                }
                if tab_v == TAB_BATCH {
                    batch::BatchPage { busy, lang }
                }
                div { class: "footer",
                    span { "{i18n::filln(i18n::tt(i18n::k::FOOTER_BACKEND), &[&backend, &caps_display, &session])}" }
                    if paused_now {
                        span { class: "hk hk-warn", "{i18n::tt(i18n::k::FOOTER_PAUSED)}" }
                    }
                    HotkeyBridge { on_trigger: trigger_region }
                }
                }
            }
            // 窗口边缘描边（浅色壁纸下窗口边界可分辨的关键）：fixed 覆盖层画在
            // 一切内容之上——body 的 inset box-shadow 会被 titlebar/sidebar 的
            // 不透明背景盖住，画了也白画（实测踩坑）
            div { class: "win-frame" }
        }
        }
    }
}

/// Caps → 页脚能力串（成功探测时；失败时页脚显示探测错误原文）
fn format_caps(c: Caps) -> String {
    format!(
        "fullscreen={} region_native={} active_window={} picker={} win_list={}",
        c.fullscreen, c.region_native, c.active_window, c.builtin_picker_needs, c.window_list
    )
}

/// 结果面板里一行的预渲染数据（rsx 内避免元组索引/复杂模式）
#[derive(Clone, PartialEq)]
struct RowView {
    no: usize,
    text: String,
    score: String,
    /// 该行是否刚复制成功（显示渐隐"已复制 ✓"角标）
    flash: bool,
    /// 复制序号：作为角标 key，重复复制同一行时强制重挂载、动画重放
    tick: u64,
    /// 定位框（图像像素 [x0,y0,x1,y1]；None = 该行无坐标，如插件占位/历史回填）
    box_px: Option<[f32; 4]>,
}

/// 结果行 textarea 的行数估计（P1 #6"行高自适应"）：
/// 按每行约 46 列折行累加，钳制在 1..=6 行内。
fn est_rows(text: &str) -> usize {
    let n: usize = text
        .lines()
        .map(|l| {
            let cols = l.chars().count();
            (((cols as f64) / 46.0).ceil() as usize).max(1)
        })
        .sum();
    n.clamp(1, 6)
}

/// 识别页：捕获/粘贴按钮行 + 排版解析下拉 + 状态行 + 结果面板（可编辑）
/// + 预览划选 + 历史记录。空状态占位见 P0 #5。
/// M 波：`caps` 驱动"活动窗口"按钮三态——active_window=原路；window_list=
/// 弹"选择窗口"列表面（xcap 平台）；都没有（Wayland portal/grim）=按钮隐藏。
#[component]
fn MainTab(
    ctx: CaptureCtx,
    engine_text: String,
    caps: Option<Caps>,
    mut tab: Signal<usize>,
    lang: Signal<String>,
) -> Element {
    let CaptureCtx {
        shot,
        shot_size,
        mut status,
        engine_st,
        lines,
        texts,
        history,
        hist_missing,
        busy,
        locate,
        ..
    } = ctx;

    // 复制反馈：("row-<no>" | "all" | "crop", 序号)；只做行内渐隐角标，不挤占全局 status
    let mut copied = use_signal(|| (String::new(), 0u64));
    // P1 #9 划选：拖选起点 / 当前点（wrapper 元素坐标，CSS px）；None = 未在拖
    let mut drag = use_signal(|| Option::<(f64, f64)>::None);
    let mut cur = use_signal(|| (0.0f64, 0.0f64));
    // 预览图显示尺寸（CSS px；0 = 未知，禁用划选）。wrapper 的 onresize
    // （ResizeObserver）唯一维护——布局定型/换图/窗口 resize 都会更新；
    // mousedown 按下时经挂载句柄现场重采作划选基准（见 onmousedown 注释）。
    let mut img_px = use_signal(|| (0.0f64, 0.0f64));
    // wrapper 的挂载句柄（Event<MountedData>，Rc 克隆即可存 Signal）：
    // mousedown 处理器借它重新 get_client_rect（MountedData 本体不可 Clone，事件包装可）
    let mut wrap_mounted = use_signal(|| Option::<MountedEvent>::None);
    // P2 #11：排版解析（持久化在 settings.layout，改动即写盘）
    let mut layout_sel = use_signal(move || settings::get().layout.clone());
    // J2：公式模式开关（持久化在 settings.formula_mode，native 引擎分流到
    // 结构化管线；改动即写盘，冷启动首跑提示在 ocr_flow::hint_formula_init）
    let mut formula_sel = use_signal(|| settings::get().formula_mode);
    // 忽略区域（P0 坐标版防水印）：编辑模式开关 + 区域列表镜像
    //（初始读 settings；每次增删都 settings::update 写盘 + 同步本信号触发重渲染）
    let mut edit_regions = use_signal(|| false);
    let ig_regions = use_signal(|| settings::get().ignore_regions.clone());
    // M 波任务 3：窗口选择列表面（xcap 平台"活动窗口"路径；caps 驱动显隐）。
    // list/loading 的外层绑定不直接 set（WindowPicker::open 内部用字段访问）
    let mut win_open = use_signal(|| false);
    let win_list = use_signal(Vec::<rumi_capture::WindowInfo>::new);
    let win_loading = use_signal(|| false);
    let win_picker = picker::WindowPicker { open: win_open, list: win_list, loading: win_loading };

    let lang_v = lang();
    let t = |k: &'static str| i18n::ts(&lang_v, k);

    let captured_lines = lines();
    let captured_texts = texts();
    let line_count = captured_texts.len();
    let joined = captured_texts.join("\n");
    let engine_label = engine_st().label();
    let busy_flag = busy();
    let (flash_target, flash_tick) = copied();
    let flash_all = flash_target == "all";
    let flash_crop = flash_target == "crop";

    // 进行中按钮文字反馈（P0 #5）
    let btn_full = if busy_flag { t(i18n::k::BTN_RECOGNIZING).to_string() } else { t(i18n::k::BTN_FULLSCREEN).to_string() };
    let btn_region = if busy_flag { t(i18n::k::BTN_RECOGNIZING).to_string() } else { t(i18n::k::BTN_REGION).to_string() };
    let btn_window = if busy_flag { t(i18n::k::BTN_RECOGNIZING).to_string() } else { t(i18n::k::BTN_WINDOW).to_string() };

    let engine_dot = match engine_st() {
        EngineState::Ready => "ok",
        EngineState::Init | EngineState::Busy => "warn",
        EngineState::Error(_) => "bad",
    };

    let rows: Vec<RowView> = captured_texts
        .iter()
        .enumerate()
        .map(|(idx, txt)| RowView {
            no: idx + 1,
            text: txt.clone(),
            score: captured_lines.get(idx).map(|l| l.score.clone()).unwrap_or_default(),
            flash: flash_target == format!("row-{}", idx + 1),
            tick: flash_tick,
            box_px: captured_lines.get(idx).and_then(|l| l.box_px),
        })
        .collect();

    // P1 #9 划选矩形（元素坐标 → 覆盖层 left/top/w/h）
    let drag_v = drag();
    let cur_v = cur();
    let sel_rect: Option<(f64, f64, f64, f64)> = drag_v.map(|(x0, y0)| {
        let (x1, y1) = cur_v;
        (x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
    });
    let img_size_unknown = img_px().0 < 1.0;

    // P2 #11：排版下拉当前值
    let layout_v = layout_sel();
    // J2：公式开关当前值
    let formula_on = formula_sel();

    // 忽略区域（P0）：编辑态 + 归一化 → 预览显示坐标的覆盖层视图
    let edit_on = edit_regions();
    let ig_views = ignore_region_views(ig_regions(), img_px());
    // M 波任务 3："活动窗口"按钮三态（caps 驱动）
    // - active_window → 原路（KDE Spectacle）
    // - window_list   → 弹"选择窗口"列表面（xcap：X11/Win/mac）
    // - 两者皆无（Wayland portal/grim，安全模型）→ 按钮隐藏
    let win_btn: Option<bool> = caps.and_then(|c| {
        if c.active_window {
            Some(false)
        } else if c.window_list {
            Some(true)
        } else {
            None
        }
    });
    let win_rows = win_list();
    let win_loading_v = win_loading();
    let win_open_v = win_open();
    let ig_label = if edit_on { "忽略区域 ✓" } else { "忽略区域" };
    let ig_style = if edit_on {
        "border-color: var(--danger); color: var(--danger); background: var(--danger-soft);"
    } else {
        ""
    };
    // 两种拖选互斥：编辑态拖选生成忽略框（红），常态拖选划选复制（蓝）
    let drag_cls = if edit_on { "ig-drag" } else { "sel-rect" };

    // P1 #7：历史记录视图数据
    let history_v = history();
    let hist_empty = history_v.is_empty();
    let hist_views: Vec<HistView> = history_v
        .iter()
        .enumerate()
        .map(|(i, it)| HistView {
            idx: i,
            at: it.at,
            age: ocr_flow::history_age(&lang_v, it.at, ocr_flow::now_secs()),
            summary: it.summary.clone(),
            count: it.count,
        })
        .collect();

    rsx! {
        div {
            class: "page page-wide",
            // P1 #8：识别页窗口内 Ctrl+V → 粘贴识别；ESC 取消划选（P1 #9）
            // 或关闭窗口选择列表面（M 波任务 3）
            tabindex: "0",
            onkeydown: move |ev: KeyboardEvent| {
                if ev.is_composing() { return; }
                if ev.key() == Key::Escape {
                    if win_open() {
                        win_open.set(false);
                    }
                    drag.set(None);
                    return;
                }
                if ev.modifiers().contains(Modifiers::CONTROL) && ev.key() == Key::Character("v".into()) {
                    if busy() { return; }
                    ev.prevent_default();
                    ocr_flow::paste_and_ocr(ctx);
                }
            },
            div { class: "page-head",
                span { class: "page-title", "{t(i18n::k::PAGE_RECOGNIZE)}" }
                span { class: "chip",
                    span { class: "dot {engine_dot}" }
                    "{i18n::fill(t(i18n::k::MAIN_ENGINE), engine_text.as_str())} · {engine_label}"
                }
                button { class: "btn-sm", onclick: move |_| tab.set(TAB_PLUGINS),
                    "{t(i18n::k::BTN_SWITCH_ENGINE)}" }
            }

            div { class: "row",
                button { disabled: busy_flag,
                    onclick: move |_| ocr_flow::capture_and_ocr(ctx, Kind::Full),
                    "{btn_full}" }
                button { class: "primary", disabled: busy_flag,
                    onclick: move |_| ocr_flow::capture_and_ocr(ctx, Kind::Region),
                    "{btn_region}" }
                // M 波任务 3：caps 驱动三态——原路 / 选择窗口列表面 / 隐藏
                if let Some(use_list) = win_btn {
                    button { disabled: busy_flag,
                        onclick: move |_| {
                            if use_list {
                                win_picker.open(status);
                            } else {
                                ocr_flow::capture_and_ocr(ctx, Kind::ActiveWindow);
                            }
                        },
                        "{btn_window}" }
                }
                button { disabled: busy_flag, title: "Ctrl+V",
                    onclick: move |_| ocr_flow::paste_and_ocr(ctx),
                    "{t(i18n::k::BTN_PASTE)}" }
                // 忽略区域（P0 坐标版防水印）：编辑模式开关。on 时预览图拖选生成
                // 红色忽略框（识别前置白），与划选复制互斥；off 时框细虚线常显
                button { class: "btn-sm", style: "{ig_style}",
                    title: "Umi 式防水印：拖选要忽略的区域，识别前把框内像素置白（持久化，粘贴图同样生效）",
                    onclick: move |_| edit_regions.set(!edit_regions()),
                    "{ig_label}" }
                // P2 #11：排版解析下拉（Umi"文本后处理"）。
                // 注意：select 不写 value 属性（非标准，webkit 行为不定），
                // 选中态只由 option 的 selected 驱动（dioxus 规范写法）。
                // 公式模式下排版不参与（结构化管线按版面分块），禁用防困惑。
                label { class: "chip", style: "gap: 6px; margin-left: auto;",
                    "{t(i18n::k::LAYOUT_LABEL)}"
                    select {
                        disabled: formula_on,
                        style: "padding: 2px 6px; font-size: var(--fs-s);",
                        title: if formula_on { "公式模式下排版解析不适用（结构化管线已按版面分块）" } else { "" },
                        onchange: move |e| {
                            let v = e.value();
                            if let Err(e) = settings::update(|s| { s.layout = v.clone(); Ok(()) }) {
                                status.set(format!("❌ 排版设置保存失败: {e}"));
                            }
                            layout_sel.set(v);
                            // 热重排：已有识别结果按新排版即时重解析（原料 =
                            // 最近一次识别的原始行），无需重新截图识别。
                            // lines 与 texts 必须同刷（可编辑文本/复制全部取 texts）。
                            // 定位高亮同步清空：重排后行号↔框对应已变。
                            let ocr_flow::CaptureCtx { raw, mut lines, mut texts, mut locate, .. } = ctx;
                            if let Some((raw, kind)) = raw() {
                                let ls = ocr_flow::derive_display(
                                    &raw, kind, &ocr_flow::OcrOpts::from_settings(),
                                );
                                lines.set(ls.clone());
                                texts.set(ls.iter().map(|l| l.text.clone()).collect());
                            }
                            locate.set(None);
                        },
                        option { value: ocr_flow::layout_key(rumi_ocr::postprocess::Layout::ByLine),
                            selected: layout_v == ocr_flow::layout_key(rumi_ocr::postprocess::Layout::ByLine),
                            "{t(i18n::k::LAYOUT_BY_LINE)}" }
                        option { value: ocr_flow::layout_key(rumi_ocr::postprocess::Layout::MergePara),
                            selected: layout_v == ocr_flow::layout_key(rumi_ocr::postprocess::Layout::MergePara),
                            "{t(i18n::k::LAYOUT_MERGE)}" }
                        option { value: ocr_flow::layout_key(rumi_ocr::postprocess::Layout::NaturalPara),
                            selected: layout_v == ocr_flow::layout_key(rumi_ocr::postprocess::Layout::NaturalPara),
                            "{t(i18n::k::LAYOUT_NATURAL)}" }
                    }
                }
                // J2：公式模式开关（排版下拉旁）。开 → native 识别分流到结构化
                // 管线（版面 + 公式行→LaTeX）；排版后处理对 structured 输出不应用，
                // 忽略区域/忽略规则照常。首次识别自动下载模型，状态行有提示；
                // 启动时若开关已开会在后台预热（App 挂载逻辑，O 波 #6）。
                label { class: "chip chk", style: "cursor: pointer; gap: 6px;",
                    title: "{t(i18n::k::FORMULA_HINT)}",
                    input {
                        r#type: "checkbox",
                        checked: formula_on,
                        onchange: move |e| {
                            let on = e.checked();
                            let _ = settings::update(|s| { s.formula_mode = on; Ok(()) });
                            formula_sel.set(on);
                        },
                    }
                    "{t(i18n::k::FORMULA_TOGGLE)}"
                }
            }

            div { class: "status", "{status}" }

            if let Some(src) = shot() {
                div { class: "results",
                    div { class: "shot-pane",
                        div {
                            class: "shot-wrap",
                            // P1 #9：拖选矩形（左键按下开始，移动更新，松开/移出结束）
                            onmousedown: move |e: MouseEvent| {
                                if e.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary) {
                                    return;
                                }
                                // 打磨：onmounted 的显示尺寸在窗口 resize/换图重排后是旧值，
                                // 每次按下先重新采集，拿到新尺寸再真正开始拖选。
                                // move/up 沿用本次 mousedown 采集到的快照——拖选中途 resize
                                // 属极端场景，不处理（保持坐标空间一致，中途换基准反而更糟）。
                                let p = e.element_coordinates();
                                let mounted = wrap_mounted();
                                let mut img_px = img_px;
                                let mut drag = drag;
                                let mut cur = cur;
                                spawn(async move {
                                    let mut usable = false;
                                    if let Some(m) = mounted {
                                        if let Ok(r) = m.get_client_rect().await {
                                            let (w, h) = (r.size.width, r.size.height);
                                            img_px.set((w, h));
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
                                let (w, h) = img_px();
                                cur.set((p.x.clamp(0.0, w), p.y.clamp(0.0, h)));
                            },
                            onmouseup: move |_| {
                                // 忽略区域编辑态：拖选生成忽略框；否则划选复制（两拖选互斥旁路）
                                if edit_regions() {
                                    commit_ignore_region(drag, cur, img_px, ig_regions, status);
                                } else {
                                    finish_area_copy(drag, cur, img_px, shot, shot_size, copied, status);
                                }
                            },
                            onmouseleave: move |_| {
                                // 拖出图像：钳制到边缘并直接完成（无指针捕获时的兜底）
                                if drag().is_some() {
                                    let (w, h) = img_px();
                                    let (x, y) = cur();
                                    cur.set((x.clamp(0.0, w), y.clamp(0.0, h)));
                                    if edit_regions() {
                                        commit_ignore_region(drag, cur, img_px, ig_regions, status);
                                    } else {
                                        finish_area_copy(drag, cur, img_px, shot, shot_size, copied, status);
                                    }
                                }
                            },
                            // 显示尺寸采集：onmounted 时 img 尚未解码，inline-block
                            // wrapper 还是 0×0（文字定位框曾被这初值卡在尺寸守卫）；
                            // 真实尺寸由 onresize（ResizeObserver）跟踪——布局定型/
                            // 换图/窗口 resize 都会触发并随事件带上新尺寸。
                            // onmounted 仍存句柄供 mousedown 重采，rect 兜底不支持时用。
                            // 显示尺寸采集：wrapper 的 onresize（ResizeObserver）是唯一
                            // 权威来源——布局定型/换图/窗口 resize 都会随事件带上新尺寸。
                            // onmounted 只存句柄供 mousedown 重采，绝不能在这里写 img_px：
                            // 其 get_client_rect 是异步查询，快照取于挂载瞬间（img 未解码
                            // = 0×0），结果却晚于 resize 事件到达，会把已采到的真实尺寸
                            // 覆盖回 0（文字定位框曾死在这一步，实测见 ui_run.out）。
                            onmounted: move |e: MountedEvent| async move {
                                wrap_mounted.set(Some(e.clone()));
                            },
                            onresize: move |e: ResizeEvent| {
                                if let Ok(s) = e.get_border_box_size() {
                                    img_px.set((s.width, s.height));
                                }
                            },
                            img { src: "{src}" }
                            if let Some((rx, ry, rw, rh)) = sel_rect {
                                div {
                                    class: "{drag_cls}",
                                    style: "left: {rx}px; top: {ry}px; width: {rw}px; height: {rh}px;",
                                }
                            }
                            // 忽略区域覆盖层：编辑态红框 + ✕ 删除；常态细虚线常显
                            for v in ig_views {
                                IgnoreRegionBox { key: "{v.idx}", view: v, editing: edit_on, regions: ig_regions }
                            }
                            // 文字定位高亮：点结果行 → 该行/该段 bbox（图像像素 →
                            // 按显示尺寸归一化），蓝框常显；再点同号取消
                            if let Some(v) = locate_view(locate(), shot_size(), img_px()) {
                                LocateBox { v }
                            }
                        }
                        div { class: "sel-hint",
                            if edit_on {
                                span { "忽略区域编辑中：拖选添加红色忽略框，点框角 ✕ 删除；完成后关闭开关" }
                            } else if flash_crop {
                                span { class: "copied-chip inline", key: "{flash_tick}",
                                    "{t(i18n::k::SEL_COPIED)}" }
                            } else if img_size_unknown {
                                "{t(i18n::k::SEL_UNAVAILABLE)}"
                            } else {
                                "{t(i18n::k::SEL_HINT)}"
                            }
                        }
                    }
                    div { class: "lines-pane",
                        div { class: "lines-head",
                            span { "{i18n::fill(t(i18n::k::MAIN_RESULTS_HEAD), line_count)}" }
                            div { class: "lines-actions",
                                if flash_all {
                                    span { class: "copied-chip inline", key: "{flash_tick}",
                                        "{t(i18n::k::BTN_COPIED)}" }
                                }
                                button { class: "btn-sm",
                                    disabled: busy_flag || captured_texts.is_empty(),
                                    onclick: move |_| match copy_text(&joined) {
                                        Ok(()) => copied.set(("all".into(), flash_tick + 1)),
                                        Err(e) => status.set(format!("{}{e}", i18n::tt(i18n::k::ERR_MARK))),
                                    },
                                    "{t(i18n::k::BTN_COPY_ALL)}" }
                            }
                        }
                        if captured_texts.is_empty() {
                            div { class: "hint", style: "padding: 10px 12px;",
                                "{t(i18n::k::MAIN_NO_TEXT)}" }
                        } else {
                            ul { class: "lines",
                                for view in rows {
                                    OcrRow { key: "{view.no}", view, texts, copied, status, locate, lang }
                                }
                            }
                            div { class: "hint", style: "padding: 4px 12px 8px;",
                                "{t(i18n::k::MAIN_EDITABLE_HINT)}" }
                        }
                    }
                }
            } else {
                div { class: "empty-state",
                    NavIcon { name: NavIconName::Recognize, size: 34 }
                    div { class: "empty-title", "{t(i18n::k::MAIN_EMPTY_TITLE)}" }
                    div { class: "empty-sub", "{t(i18n::k::MAIN_EMPTY_SUB)}" }
                }
            }

            // ── P1 #7：历史记录 ─────────────────────────────────
            if shot().is_some() || !hist_empty {
                HistoryPanel {
                    views: hist_views,
                    empty: hist_empty,
                    shot,
                    shot_size,
                    lines,
                    texts,
                    history,
                    hist_missing,
                    status,
                    lang,
                }
            }

            // ── M 波任务 3：窗口选择列表面（xcap 平台"活动窗口"路径）──
            if win_open_v {
                picker::WindowPickerModal {
                    open: win_open,
                    windows: win_rows,
                    loading: win_loading_v,
                    on_pick: move |w: rumi_capture::WindowInfo| {
                        win_open.set(false);
                        picker::capture_window(ctx, w);
                    },
                }
            }
        }
    }
}

/// 历史条目列表行的预渲染数据（rsx 内不写复杂表达式）
#[derive(Clone, PartialEq)]
struct HistView {
    idx: usize,
    /// 条目时间戳（缓存缺失标记的键，与 idx 解耦：插入/删除不串位）
    at: u64,
    age: String,
    summary: String,
    count: usize,
}

/// P1 #7 历史记录面板：点击回填预览图与结果（懒加载磁盘完整图，ocr_flow::restore_history），
/// 可删单条 / 清空（内存条目 + 对应缓存文件一起删；上限在 ocr_flow 侧裁剪）。
/// `hist_missing`：磁盘缓存图被清的条目键（item.at）——行内提示并禁用回填。
#[component]
fn HistoryPanel(
    views: Vec<HistView>,
    empty: bool,
    mut shot: Signal<Option<String>>,
    mut shot_size: Signal<(u32, u32)>,
    mut lines: Signal<Vec<UiLine>>,
    mut texts: Signal<Vec<String>>,
    mut history: Signal<Vec<ocr_flow::HistoryItem>>,
    mut hist_missing: Signal<std::collections::BTreeSet<u64>>,
    mut status: Signal<String>,
    // J2：传 Signal（Copy）而非渲染体里的 lang_v String——MainTab 渲染体里
    // lang_v 还被 t 闭包借用，move 进组件会撞 E0505（新 rustc 借用分析口径）
    lang: Signal<String>,
) -> Element {
    let lang_v = lang();
    let t = |k: &'static str| i18n::ts(&lang_v, k);
    let hint = t(i18n::k::HIST_RESTORE_HINT).to_string();
    // onclick 闭包是 'static 的，不能借用渲染体里的 lang_v——用到词先在渲染体算好
    let cleared_word = t(i18n::k::HIST_CLEARED).to_string();
    rsx! {
        div { class: "card hist-card",
            div { class: "hist-head",
                span { class: "hist-title", "{t(i18n::k::HIST_TITLE)}" }
                if !empty {
                    button { class: "btn-sm",
                        onclick: move |_| {
                            // 清空历史 = 删内存条目 + 删对应缓存文件 + 清缺失标记
                            let stale: Vec<ocr_flow::HistoryItem> = history();
                            history.set(Vec::new());
                            hist_missing.set(std::collections::BTreeSet::new());
                            status.set(cleared_word.clone());
                            spawn(async move {
                                let _ = ocr_flow::run_blocking(move || {
                                    for it in &stale {
                                        if let Some(p) = &it.png_path {
                                            ocr_flow::remove_history_png(p);
                                        }
                                    }
                                })
                                .await;
                            });
                        },
                        "{t(i18n::k::HIST_CLEAR)}" }
                }
            }
            if empty {
                div { class: "hist-empty", "{t(i18n::k::HIST_EMPTY)}" }
            } else {
                div { class: "hist-list",
                    for v in views {
                        div { style: "display: flex; align-items: center; gap: 4px;",
                            button {
                                class: "hist-row",
                                title: "{hint}",
                                // 缓存图缺失的条目：禁用回填（行尾有行内提示）
                                disabled: hist_missing().contains(&v.at),
                                onclick: move |_| {
                                    if hist_missing().contains(&v.at) { return; }
                                    if let Some(it) = history.read().get(v.idx).cloned() {
                                        ocr_flow::restore_history(
                                            it, hist_missing, shot, shot_size, lines, texts, status,
                                        );
                                    }
                                },
                                span { class: "hist-time", "{v.age}" }
                                span { class: "hist-summary", "{v.summary}" }
                                span { class: "hist-count", "{i18n::fill(t(i18n::k::HIST_LINES), v.count)}" }
                            }
                            if hist_missing().contains(&v.at) {
                                span { class: "hint", "{t(i18n::k::HIST_MISSING)}" }
                            }
                            button { class: "danger btn-sm",
                                title: "{t(i18n::k::HIST_DEL_TIP)}",
                                onclick: move |_| {
                                    // 单条删除 = 删内存条目 + 删对应缓存文件 + 清该条缺失标记
                                    let item = history.read().get(v.idx).cloned();
                                    history.with_mut(|list| {
                                        if v.idx < list.len() {
                                            list.remove(v.idx);
                                        }
                                    });
                                    if let Some(it) = item {
                                        hist_missing.with_mut(|s| {
                                            s.remove(&it.at);
                                        });
                                        if let Some(p) = it.png_path {
                                            spawn(async move {
                                                let _ = ocr_flow::run_blocking(move || {
                                                    ocr_flow::remove_history_png(&p);
                                                })
                                                .await;
                                            });
                                        }
                                    }
                                },
                                "✕" }
                        }
                    }
                }
            }
        }
    }
}

/// 结果面板里的单行记录（P1 #6）：textarea 可编辑 + score 徽标 + 行复制按钮，
/// 复制成功时行内"已复制 ✓"1.2s 渐隐。textarea 内 Ctrl+V 走原生粘贴，
/// 不再冒泡触发整页"粘贴识别"。
#[component]
fn OcrRow(
    view: RowView,
    mut texts: Signal<Vec<String>>,
    mut copied: Signal<(String, u64)>,
    mut status: Signal<String>,
    mut locate: Signal<Option<(usize, [f32; 4])>>,
    lang: Signal<String>,
) -> Element {
    let lang_v = lang();
    let t = |k: &'static str| i18n::ts(&lang_v, k);
    let no = view.no;
    let idx = no - 1;
    let rows = est_rows(&view.text);
    // 复制的是编辑后文本：textarea 编辑 → texts[idx] 更新 → 本组件随父重渲染拿到新值
    let copy_text_v = view.text.clone();
    let copied_word = t(i18n::k::BTN_COPIED).to_string();
    // 文字定位：行首数字即定位按钮（textarea 占满行身，整行 onclick 大部分
    // 区域点不到——P 波实测反馈，改为显式点击目标）；同号再点取消
    let locate_cls = if view.box_px.is_some() { "row-no locatable" } else { "row-no" };
    let locate_box = view.box_px;
    let locate_tip = if view.box_px.is_some() { t(i18n::k::LOCATE_TIP).to_string() } else { String::new() };
    rsx! {
        li {
            key: "{no}",
            class: "line",
            span {
                class: "{locate_cls}",
                title: "{locate_tip}",
                onclick: move |_| {
                    // 交接诊断插桩：点击是否触发 + 信号状态（追踪定位链路，落运行日志）
                    logger::info("locate", format!("[locate] click no={no} box={locate_box:?} prev={:?}", locate()));
                    let Some(b) = locate_box else { return; };
                    let off = matches!(locate(), Some((n, _)) if n == no);
                    locate.set(if off { None } else { Some((no, b)) });
                },
                "{no}" }
            textarea {
                class: "line-edit",
                rows: "{rows}",
                value: "{view.text}",
                oninput: move |e| {
                    let v = e.value();
                    texts.with_mut(|list| {
                        if idx < list.len() {
                            list[idx] = v;
                        }
                    });
                },
                onkeydown: move |e: KeyboardEvent| {
                    if e.modifiers().contains(Modifiers::CONTROL) && e.key() == Key::Character("v".into()) {
                        e.stop_propagation();
                    }
                },
            }
            span { class: "score", "{view.score}" }
            // 标准尺寸按钮（非 btn-sm）：行复制是高频操作，太小难点中
            button { title: "{t(i18n::k::COPY_ROW_TIP)}",
                onclick: move |_| match copy_text(&copy_text_v) {
                    Ok(()) => copied.set((format!("row-{no}"), view.tick + 1)),
                    Err(e) => status.set(i18n::fill(i18n::tt(i18n::k::COPY_FAIL), e)),
                },
                "⧉ 复制" }
            if view.flash {
                span { class: "copied-chip", key: "{view.tick}", "{copied_word}" }
            }
        }
    }
}

/// P1 #9 划选结束：显示坐标 → 原图像素坐标 → 解码裁剪 → 写剪贴板（std 线程）。
/// 选区太小（<4px）视为普通点击，不裁剪。
#[allow(clippy::too_many_arguments)]
fn finish_area_copy(
    mut drag: Signal<Option<(f64, f64)>>,
    cur: Signal<(f64, f64)>,
    img_px: Signal<(f64, f64)>,
    shot: Signal<Option<String>>,
    shot_size: Signal<(u32, u32)>,
    copied: Signal<(String, u64)>,
    status: Signal<String>,
) {
    let Some((x0, y0)) = drag() else {
        return;
    };
    let (x1, y1) = cur();
    drag.set(None);
    let (disp_w, disp_h) = img_px();
    let (nat_w, nat_h) = shot_size();
    if disp_w < 1.0 || disp_h < 1.0 || nat_w == 0 || nat_h == 0 {
        return;
    }
    let sx = nat_w as f64 / disp_w;
    let sy = nat_h as f64 / disp_h;
    let ix0 = ((x0.min(x1) * sx).floor() as i64).clamp(0, nat_w as i64) as u32;
    let iy0 = ((y0.min(y1) * sy).floor() as i64).clamp(0, nat_h as i64) as u32;
    let ix1 = ((x1.max(x0) * sx).ceil() as i64).clamp(0, nat_w as i64) as u32;
    let iy1 = ((y1.max(y0) * sy).ceil() as i64).clamp(0, nat_h as i64) as u32;
    if ix1.saturating_sub(ix0) < 4 || iy1.saturating_sub(iy0) < 4 {
        return; // 视为普通点击
    }
    let Some(url) = shot() else {
        return;
    };
    let mut copied = copied;
    let mut status = status;
    spawn(async move {
        let rx = ocr_flow::run_blocking(move || crop_png_to_clipboard(&url, ix0, iy0, ix1, iy1));
        match rx.await {
            Ok(Ok(())) => {
                let (_, tick) = copied();
                copied.set(("crop".into(), tick + 1));
            }
            Ok(Err(e)) => status.set(format!("{}{e}", i18n::tt(i18n::k::ERR_MARK))),
            Err(_) => status.set(i18n::tt(i18n::k::CROP_THREAD_LOST).to_string()),
        }
    });
}

/// 从预览 data-url 解码 PNG → 按原图像素坐标裁剪 → RGBA 写剪贴板（std 线程调用）。
fn crop_png_to_clipboard(url: &str, x: u32, y: u32, x1: u32, y1: u32) -> Result<(), String> {
    use base64::Engine;
    let b64 = url
        .strip_prefix("data:image/png;base64,")
        .ok_or("预览不是 PNG data-url")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("解码预览: {e}"))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("解码预览图像: {e}"))?
        .to_rgba8();
    let (iw, ih) = img.dimensions();
    let (x, y) = (x.min(iw), y.min(ih));
    let (x1, y1) = (x1.clamp(x, iw), y1.clamp(y, ih));
    if x1 <= x || y1 <= y {
        return Err("选区为空".into());
    }
    let crop = image::imageops::crop_imm(&img, x, y, x1 - x, y1 - y).to_image();
    let (cw, ch) = crop.dimensions();
    copy_image(crop.into_raw(), cw, ch)
}

// ── 忽略区域（P0 坐标版防水印）─────────────────────────────────────────────
//
// 数据与上限在 settings（ignore_regions / MAX_IGNORE_REGIONS），置白在
// ocr_flow::apply_ignore_regions（识别帧），本节只管识别页编辑交互：
// 工具行开关 → 编辑态拖选新增红框 / 点 ✕ 删除 → settings 写盘 + 信号同步。

/// 忽略区域覆盖层的预渲染视图（rsx 内不做元组索引/换算）
#[derive(Clone, Copy, PartialEq)]
struct IgView {
    idx: usize,
    left: f64,
    top: f64,
    w: f64,
    h: f64,
}

/// 归一化忽略区域 → 预览显示坐标覆盖层视图（渲染体纯函数）。
/// 显示尺寸未知（<1px）时返回空，避免 0 除与满屏误框。
fn ignore_region_views(regions: Vec<[f32; 4]>, disp: (f64, f64)) -> Vec<IgView> {
    let (dw, dh) = disp;
    if dw < 1.0 || dh < 1.0 {
        return Vec::new();
    }
    regions
        .iter()
        .enumerate()
        .map(|(i, r)| IgView {
            idx: i,
            left: r[0] as f64 * dw,
            top: r[1] as f64 * dh,
            w: ((r[2] - r[0]).max(0.0) as f64 * dw).max(2.0),
            h: ((r[3] - r[1]).max(0.0) as f64 * dh).max(2.0),
        })
        .collect()
}

/// 文字定位框的显示几何：图像像素 bbox → 显示 CSS px（left/top/w/h）。
/// 原图尺寸或显示尺寸无效（<1px，未挂载/未知）时 None = 不渲染。
fn locate_view(
    loc: Option<(usize, [f32; 4])>,
    size: (u32, u32),
    disp: (f64, f64),
) -> Option<(f64, f64, f64, f64)> {
    let (_, b) = loc?;
    let (sw, sh) = size;
    let (dw, dh) = disp;
    let out = (|| {
        if sw == 0 || sh == 0 || dw < 1.0 || dh < 1.0 {
            return None;
        }
        let (sw, sh) = (sw as f64, sh as f64);
        Some((
            b[0] as f64 / sw * dw,
            b[1] as f64 / sh * dh,
            ((b[2] - b[0]).max(1.0) as f64 / sw * dw).max(3.0),
            ((b[3] - b[1]).max(1.0) as f64 / sh * dh).max(3.0),
        ))
    })();
    // 交接诊断插桩：定位激活时的渲染几何（None = 尺寸守卫未过，落运行日志）
    logger::info("locate", format!("[locate] view b={b:?} size={size:?} disp={disp:?} -> {out:?}"));
    out
}

/// 文字定位高亮框：主题色描边 + 半透明填充；pointer-events:none，
/// 不抢划选复制/忽略编辑的鼠标交互。
#[component]
fn LocateBox(v: (f64, f64, f64, f64)) -> Element {
    let style = format!(
        "left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px;",
        v.0, v.1, v.2, v.3
    );
    rsx! { div { class: "loc-rect", style: "{style}" } }
}

/// 忽略区域覆盖框：editing = 红色半透明 + 右上 ✕ 删除角标（点击删框并写盘）；
/// 常态 = 细虚线常显、不响应任何交互（pointer-events:none，无删除角标）。
#[component]
fn IgnoreRegionBox(
    view: IgView,
    editing: bool,
    mut regions: Signal<Vec<[f32; 4]>>,
) -> Element {
    let idx = view.idx;
    let cls = if editing { "ig-rect editing" } else { "ig-rect idle" };
    let style = format!(
        "left: {:.1}px; top: {:.1}px; width: {:.1}px; height: {:.1}px;",
        view.left, view.top, view.w, view.h
    );
    rsx! {
        div { class: "{cls}", style: "{style}",
            if editing {
                button {
                    class: "ig-del",
                    title: "删除该忽略区域",
                    // 点击不得冒泡成预览图拖选（两种拖选互斥）
                    onmousedown: move |e: MouseEvent| e.stop_propagation(),
                    onclick: move |_| delete_ignore_region(&mut regions, idx),
                    "✕"
                }
            }
        }
    }
}

/// 拖选结束（忽略区域编辑态）：显示坐标 → 归一化坐标（0..1，相对整图），
/// 新增进 settings 并写盘。选区太小（<4px）视为普通点击不生成；
/// 超上限被 settings::push_ignore_region 拒绝时 status 行内提示。
#[allow(clippy::too_many_arguments)]
fn commit_ignore_region(
    mut drag: Signal<Option<(f64, f64)>>,
    cur: Signal<(f64, f64)>,
    img_px: Signal<(f64, f64)>,
    mut regions: Signal<Vec<[f32; 4]>>,
    mut status: Signal<String>,
) {
    let Some((x0, y0)) = drag() else {
        return;
    };
    let (x1, y1) = cur();
    drag.set(None);
    let (dw, dh) = img_px();
    if dw < 1.0 || dh < 1.0 {
        return;
    }
    if (x1 - x0).abs() < 4.0 || (y1 - y0).abs() < 4.0 {
        return; // 视为普通点击
    }
    // 显示坐标 → 归一化（min/max + 0..1 钳制，语义同 ocr_flow::apply_ignore_regions）
    let nx0 = ((x0.min(x1) / dw) as f32).clamp(0.0, 1.0);
    let ny0 = ((y0.min(y1) / dh) as f32).clamp(0.0, 1.0);
    let nx1 = ((x1.max(x0) / dw) as f32).clamp(0.0, 1.0);
    let ny1 = ((y1.max(y0) / dh) as f32).clamp(0.0, 1.0);
    match settings::push_ignore_region(regions(), [nx0, ny0, nx1, ny1]) {
        Ok(new) => {
            let saved = settings::update(|s| {
                s.ignore_regions = new.clone();
                Ok(())
            });
            regions.set(new.clone());
            if saved.is_ok() {
                status.set(format!("已添加忽略区域（共 {} 个）", new.len()));
            } else {
                status.set(format!("{}忽略区域写盘失败", i18n::tt(i18n::k::ERR_MARK)));
            }
        }
        Err(e) => status.set(format!("{}{e}", i18n::tt(i18n::k::ERR_MARK))),
    }
}

/// 删除一个忽略区域：改 settings 写盘 + 同步信号（越界索引静默忽略）
fn delete_ignore_region(regions: &mut Signal<Vec<[f32; 4]>>, idx: usize) {
    let mut v = regions();
    if idx >= v.len() {
        return;
    }
    v.remove(idx);
    let _ = settings::update(|s| {
        s.ignore_regions = v.clone();
        Ok(())
    });
    regions.set(v);
}

/// 设置页"保存 → 生效"的纯函数层（O 波 #2 可测）：把表单值落到 Settings 快照，
/// 返回需要同步回 UI 信号的 `(主题, 语言)`。主题/语言是仅有的两个"即时生效"字段：
/// 调用方保存成功后把返回值 set 进 theme/lang 信号——theme 驱动 `css()` 重算
/// （热切换），lang 驱动整页重渲染 + i18n 全局原子同步。旧实现这段桥散落在
/// save 闭包里不可测，现收拢为纯函数。
fn apply_settings_form(
    st: &mut settings::Settings,
    hotkey: &str,
    port: u16,
    lang: &str,
    theme: &str,
    ignore_rules: Vec<String>,
    notify: bool,
    sound: bool,
    frameless: bool,
    picker_mode: &str,
) -> (String, String) {
    st.hotkey = hotkey.trim().to_string();
    st.http_port = port;
    st.lang = lang.to_string();
    st.theme = theme.to_string();
    st.ignore_rules = ignore_rules;
    st.notify = notify;
    st.sound = sound;
    st.frameless = frameless;
    st.picker_mode = picker_mode.to_string();
    (theme.to_string(), lang.to_string())
}

/// 设置页：引擎状态卡 + 热键 / HTTP 端口 / 语言 / 主题 / 忽略规则 / 完成通知；
/// 保存写 config.toml。热键与端口重启生效（M3 验收 #5 即按"重启生效"验收）；
/// 语言即时生效（保存桥，P2 #15）；主题在下拉切换时即生效（O 波 #2 热切换），
/// 保存路径同样同步。
#[component]
fn SettingsPage(
    mut page_status: Signal<String>,
    mut theme: Signal<String>,
    mut hotkey_hint: Signal<String>,
    engine_text: String,
    engine_st: EngineState,
    mut tab: Signal<usize>,
    mut lang: Signal<String>,
) -> Element {
    let lang_v = lang();
    let t = |k: &'static str| i18n::ts(&lang_v, k);
    let s = settings::get();
    let mut hotkey = use_signal(move || s.hotkey.clone());
    let mut port = use_signal(move || s.http_port.to_string());
    let mut lang_sel = use_signal(move || s.lang.clone());
    let mut theme_sel = use_signal(move || s.theme.clone());
    // P2 #12：忽略规则文本域（每行一条正则）
    let mut ignore_text = use_signal(move || s.ignore_rules.join("\n"));
    // P1 #10：识别完成通知开关
    let mut notify_sel = use_signal(move || s.notify);
    // J2 #4：识别完成提示音开关
    let mut sound_sel = use_signal(move || s.sound);
    // Wave3：无边框窗口开关（自定义标题栏；改动重启生效）
    let mut frameless_sel = use_signal(move || s.frameless);
    // M 波：选区方式三选（跟随后端 / 原生 / 内置；捕获时读取，保存即生效）
    let mut picker_sel = use_signal(move || s.picker_mode.clone());
    // 关闭按钮行为（即时生效：onchange 即写盘，无需点保存）
    let mut close_sel = use_signal(move || {
        if s.close_to_tray { "tray".to_string() } else { "quit".to_string() }
    });
    // 运行日志：查看开关 + 尾部内容镜像
    let mut log_show = use_signal(|| false);
    let mut log_text = use_signal(String::new);
    let log_path_str = logger::log_path().display().to_string();
    let cfg_path = settings::config_path().display().to_string();
    let load_warn = settings::load_warning();
    let pid = plugins_ui::active_worker_pid();
    let notify_v = notify_sel();
    let sound_v = sound_sel();
    let ignore_v = ignore_text();

    // 引擎状态卡（P0 #3）：native 显示引擎生命周期，plugin 显示 worker 运行态
    struct EngineCardView {
        dot: &'static str,
        title: String,
        sub: String,
    }
    let engine_is_plugin = engine_text.starts_with("plugin:");
    let card = if engine_is_plugin {
        match pid {
            Some(p) => EngineCardView {
                dot: "ok",
                title: engine_text.clone(),
                sub: i18n::fill(i18n::tt(i18n::k::ENGINE_RUNNING), p),
            },
            None => EngineCardView {
                dot: "warn",
                title: engine_text.clone(),
                sub: i18n::tt(i18n::k::ENGINE_CONFIGURED).to_string(),
            },
        }
    } else {
        let (dot, sub) = match &engine_st {
            EngineState::Ready => ("ok", i18n::tt(i18n::k::ENG_READY).to_string()),
            EngineState::Init => ("warn", i18n::tt(i18n::k::ENG_CARD_INIT).to_string()),
            EngineState::Busy => ("warn", i18n::tt(i18n::k::ENG_BUSY).to_string()),
            EngineState::Error(e) => ("bad", i18n::fill(i18n::tt(i18n::k::ENG_ERROR), e)),
        };
        EngineCardView {
            dot,
            title: "native".into(),
            sub,
        }
    };

    let save = move |_| {
        let hotkey_v = hotkey();
        let port_v = port();
        let lang_v2 = lang_sel();
        let theme_v2 = theme_sel();
        let old_lang = settings::get().lang;
        let ignore_rules = settings::parse_ignore_textarea(&ignore_text());
        let bad = settings::bad_ignore_rules(&ignore_rules);
        let Ok(port_n) = port_v.trim().parse::<u16>() else {
            page_status.set(i18n::tt(i18n::k::SET_ERR_PORT_NUM).to_string());
            return;
        };
        if port_n == 0 {
            page_status.set(i18n::tt(i18n::k::SET_ERR_PORT_RANGE).to_string());
            return;
        }
        if let Err(e) = hotkey::validate(hotkey_v.trim()) {
            page_status.set(i18n::fill(i18n::tt(i18n::k::SET_ERR_HOTKEY), e));
            return;
        }
        match settings::update(|st| {
            // O 波 #2：表单落位收拢到纯函数 apply_settings_form（可测）；
            // 返回值即保存后要 set 回信号的 (theme, lang)，这里以表单值为准
            let _ = apply_settings_form(
                st,
                &hotkey_v,
                port_n,
                &lang_v2,
                &theme_v2,
                ignore_rules.clone(),
                notify_sel(),
                sound_sel(),
                frameless_sel(),
                &picker_sel(),
            );
            Ok(())
        }) {
            Ok(()) => {
                // O 波 #2 主题热切换桥：theme 信号变化 → App 重渲染 → css() 重算
                theme.set(theme_v2.clone());
                hotkey_hint.set(hotkey_v.trim().to_string());
                // P2 #15：语言即时生效（切信号 → 订阅它的页面全部重渲染取新词）；
                // 全局原子同步写（spawn 闭包里的状态消息同刻生效）
                i18n::set_lang(&lang_v2);
                lang.set(lang_v2.clone());
                let base = i18n::tt(i18n::k::SET_SAVED);
                if bad.is_empty() {
                    page_status.set(base.to_string());
                } else {
                    // P2 #12：坏正则行内提示（保存继续，识别管线会跳过坏正则）
                    page_status.set(i18n::filln(
                        i18n::tt(i18n::k::SET_BAD_REGEX),
                        &[&bad.len(), &format!("{bad:?}")],
                    ));
                }
                if lang_v2 != old_lang {
                    // 语言变更：重 spawn 插件 worker 的 --lang（M3 §6）
                    let lang_for_worker = lang_v2.clone();
                    let rx =
                        ocr_flow::run_blocking(move || plugins_ui::respawn_for_lang(&lang_for_worker));
                    spawn(async move {
                        match rx.await {
                            Ok(Ok(())) => {
                                page_status.set(i18n::fill(i18n::tt(i18n::k::SET_WORKER_OK), lang_v2))
                            }
                            Ok(Err(e)) => {
                                page_status.set(i18n::fill(i18n::tt(i18n::k::SET_WORKER_FAIL), e))
                            }
                            Err(_) => page_status.set(i18n::tt(i18n::k::SET_THREAD_LOST).to_string()),
                        }
                    });
                }
            }
            Err(e) => page_status.set(format!("{}{e}", i18n::tt(i18n::k::ERR_MARK))),
        }
    };

    rsx! {
        div { class: "page",
            div { class: "page-head",
                span { class: "page-title", "{t(i18n::k::PAGE_SETTINGS)}" }
                span { class: "hint", "配置文件：{cfg_path}" }
            }

            // 当前引擎状态卡（P0 #3）：点"管理引擎"跳插件页
            div { class: "card engine-card",
                span { class: "dot {card.dot}" }
                div { class: "grow",
                    div { class: "engine-card-title", "{t(i18n::k::SET_ENGINE)} · {card.title}" }
                    div { class: "engine-card-sub", "{card.sub}" }
                }
                button { class: "btn-sm", onclick: move |_| tab.set(TAB_PLUGINS),
                    "{t(i18n::k::SET_MANAGE_ENGINE)}" }
            }

            // 状态行放在日志卡之前：复制/导出的反馈写 page_status，若在日志卡之后，
            // 展开 14 行日志时反馈会被顶到视口外看不见
            page_status_line { status: page_status }
            if let Some(w) = load_warn {
                div { class: "warnline", "{w}" }
            }

            // 运行日志（体验批次）：查看 / 复制 / 导出，方便用户发开发者排障
            div { class: "card log-card",
                div { class: "log-head",
                    div { class: "grow",
                        div { class: "engine-card-title", "{t(i18n::k::SET_LOG_TITLE)}" }
                        div { class: "engine-card-sub", "{t(i18n::k::SET_LOG_HINT)}" }
                    }
                    div { class: "row",
                        button { onclick: move |_| {
                                if log_show() {
                                    log_show.set(false);
                                } else {
                                    log_text.set(logger::tail(64 * 1024));
                                    log_show.set(true);
                                }
                            },
                            if log_show() { "{t(i18n::k::SET_LOG_HIDE)}" } else { "{t(i18n::k::SET_LOG_VIEW)}" }
                        }
                        button { onclick: move |_| {
                                match copy_text(&logger::read_all()) {
                                    Ok(()) => page_status.set(i18n::tt(i18n::k::SET_LOG_COPIED).to_string()),
                                    Err(e) => page_status.set(i18n::fill(i18n::tt(i18n::k::SET_LOG_EXPORT_FAIL), e)),
                                }
                            },
                            "{t(i18n::k::SET_LOG_COPY)}"
                        }
                        button { class: "primary", onclick: move |_| {
                                // rfd 同步对话框在 Linux GTK 后端会死锁主线程，
                                // 统一经 run_blocking 放 std 线程（见 batch.rs pick_files）
                                let rx = ocr_flow::run_blocking(|| {
                                    rfd::FileDialog::new()
                                        .set_file_name("rumi.log")
                                        .save_file()
                                });
                                spawn(async move {
                                    match rx.await {
                                        Ok(Some(path)) => match logger::export_to(&path) {
                                            Ok(()) => page_status.set(i18n::fill(
                                                i18n::tt(i18n::k::SET_LOG_EXPORTED),
                                                path.display().to_string(),
                                            )),
                                            Err(e) => page_status.set(i18n::fill(
                                                i18n::tt(i18n::k::SET_LOG_EXPORT_FAIL),
                                                e,
                                            )),
                                        },
                                        Ok(None) => {} // 用户取消
                                        Err(_) => page_status.set(i18n::tt(i18n::k::SET_THREAD_LOST).to_string()),
                                    }
                                });
                            },
                            "{t(i18n::k::SET_LOG_EXPORT)}"
                        }
                    }
                }
                if log_show() {
                    textarea {
                        class: "log-view",
                        readonly: true,
                        rows: "14",
                        spellcheck: false,
                        // 空态文案：刚装好/日志被清时给一句提示而不是空白框
                        value: if log_text().is_empty() {
                            t(i18n::k::SET_LOG_EMPTY).to_string()
                        } else {
                            log_text()
                        },
                    }
                    span { class: "hint", "日志文件：{log_path_str}" }
                }
            }

            div { class: "card",
                div { class: "formrow",
                    label { title: "global-hotkey 格式（示例 alt+q / ctrl+shift+a），重启生效。Wayland 会话经 XWayland 生效：焦点在原生 Wayland 窗口时可能不触发，可用托盘菜单兜底", "{t(i18n::k::SET_HOTKEY)}" }
                    div { class: "field",
                        input { class: "ctl", value: "{hotkey()}", oninput: move |e| hotkey.set(e.value()) }
                        span { class: "hint", "当前值 {hotkey_hint}；热键触发“区域截图”流程" }
                    }
                }
                div { class: "formrow",
                    label { title: "Umi 兼容 HTTP API 端口（M3a server 读取），重启生效", "{t(i18n::k::SET_PORT)}" }
                    div { class: "field",
                        input { class: "ctl", r#type: "number", min: "1", max: "65535",
                            value: "{port()}", oninput: move |e| port.set(e.value()) }
                        span { class: "hint", "当前值 {s.http_port}，范围 1-65535，重启生效" }
                    }
                }
                div { class: "formrow",
                    label { title: "插件 worker --lang（plugin_i18n setLangCode）；native 引擎忽略", "{t(i18n::k::SET_LANG)}" }
                    div { class: "field",
                        select { class: "ctl", value: "{lang_sel()}", onchange: move |e| lang_sel.set(e.value()),
                            option { value: "zh_CN", selected: lang_sel() == "zh_CN", "简体中文 (zh_CN)" }
                            option { value: "en_US", selected: lang_sel() == "en_US", "English (en_US)" }
                            option { value: "zh_TW", selected: lang_sel() == "zh_TW", "繁體中文 (zh_TW)" }
                            option { value: "ja_JP", selected: lang_sel() == "ja_JP", "日本語 (ja_JP)" }
                        }
                        span { class: "hint", "保存后界面文案即时切换；插件 worker 立即按新语言重启" }
                    }
                }
                div { class: "formrow",
                    label { title: "界面主题，实时生效（深浅共用一套 CSS 变量）", "{t(i18n::k::SET_THEME)}" }
                    div { class: "field",
                        // O 波 #2：主题热切换——切下拉即持久化并回写 theme 信号，
                        // css() 随信号重算、整页立即变色，无需再点"保存设置"。
                        // "保存设置"路径同样同步该信号（apply_settings_form 桥，见下）。
                        select { class: "ctl", value: "{theme_sel()}", onchange: move |e| {
                            let v = e.value();
                            theme_sel.set(v.clone());
                            let _ = settings::update(|s| { s.theme = v.clone(); Ok(()) });
                            theme.set(v);
                        },
                            option { value: "dark", selected: theme_sel() == "dark", "{t(i18n::k::SET_THEME_DARK)}" }
                            option { value: "light", selected: theme_sel() == "light", "{t(i18n::k::SET_THEME_LIGHT)}" }
                        }
                    }
                }
                // M 波：选区方式（跟随后端 / 原生 / 内置冻结帧）。每次捕获时读取，
                // 保存即生效；Wayland 用户也可选"内置"统一交互体验。
                div { class: "formrow",
                    label { title: "{t(i18n::k::SET_PICKER_HINT)}", "{t(i18n::k::SET_PICKER)}" }
                    div { class: "field",
                        select { class: "ctl", value: "{picker_sel()}", onchange: move |e| picker_sel.set(e.value()),
                            option { value: "auto", selected: picker_sel() == "auto",
                                "{t(i18n::k::SET_PICKER_AUTO)}" }
                            option { value: "native", selected: picker_sel() == "native",
                                "{t(i18n::k::SET_PICKER_NATIVE)}" }
                            option { value: "builtin", selected: picker_sel() == "builtin",
                                "{t(i18n::k::SET_PICKER_BUILTIN)}" }
                        }
                        span { class: "hint", "{t(i18n::k::SET_PICKER_HINT)}" }
                    }
                }
                // 关闭按钮行为（体验批次）：托盘常驻（默认）/ 直接退出；即时生效
                div { class: "formrow",
                    label { title: "{t(i18n::k::SET_CLOSE_HINT)}", "{t(i18n::k::SET_CLOSE_ACTION)}" }
                    div { class: "field",
                        select { class: "ctl", value: "{close_sel()}",
                            onchange: move |e| {
                                let v = e.value();
                                close_sel.set(v.clone());
                                let _ = settings::update(|s| {
                                    s.close_to_tray = v == "tray";
                                    Ok(())
                                });
                            },
                            option { value: "tray", selected: close_sel() == "tray",
                                "{t(i18n::k::SET_CLOSE_TRAY)}" }
                            option { value: "quit", selected: close_sel() == "quit",
                                "{t(i18n::k::SET_CLOSE_QUIT)}" }
                        }
                        span { class: "hint", "{t(i18n::k::SET_CLOSE_HINT)}" }
                    }
                }
                // Wave3：无边框窗口（自定义标题栏）。与热键/端口同口径——重启生效，
                // 设置页注明；关闭即回退系统装饰（TitleBar 不渲染）。
                div { class: "formrow",
                    label { title: "{t(i18n::k::SET_FRAMELESS_HINT)}", "{t(i18n::k::SET_FRAMELESS)}" }
                    div { class: "field",
                        label { class: "opt chk", style: "gap: 8px; padding: 4px 0; flex: none;",
                            input {
                                r#type: "checkbox",
                                checked: frameless_sel(),
                                onchange: move |e| frameless_sel.set(e.checked()),
                            }
                            span { class: "hint", "{t(i18n::k::SET_FRAMELESS_HINT)}" }
                        }
                    }
                }
                // P1 #10：识别完成桌面通知（默认关）
                div { class: "formrow",
                    label { title: "{t(i18n::k::SET_NOTIFY_HINT)}", "{t(i18n::k::SET_NOTIFY)}" }
                    div { class: "field",
                        label { class: "opt chk", style: "gap: 8px; padding: 4px 0; flex: none;",
                            input {
                                r#type: "checkbox",
                                checked: notify_v,
                                onchange: move |e| notify_sel.set(e.checked()),
                            }
                            span { class: "hint", "{t(i18n::k::SET_NOTIFY_HINT)}" }
                        }
                    }
                }
                // J2 #4：识别完成提示音（默认关；合成两短音，无资产文件）
                div { class: "formrow",
                    label { title: "{t(i18n::k::SET_SOUND_HINT)}", "{t(i18n::k::SET_SOUND)}" }
                    div { class: "field",
                        label { class: "opt chk", style: "gap: 8px; padding: 4px 0; flex: none;",
                            input {
                                r#type: "checkbox",
                                checked: sound_v,
                                onchange: move |e| sound_sel.set(e.checked()),
                            }
                            span { class: "hint", "{t(i18n::k::SET_SOUND_HINT)}" }
                        }
                    }
                }
                // P2 #12：忽略规则（每行一条正则）
                div { class: "formrow",
                    label { title: "{t(i18n::k::SET_IGNORE_HINT)}", "{t(i18n::k::SET_IGNORE)}" }
                    div { class: "field",
                        textarea {
                            rows: "4",
                            value: "{ignore_v}",
                            placeholder: "https?://\n^水印$",
                            oninput: move |e| ignore_text.set(e.value()),
                        }
                        span { class: "hint", "{t(i18n::k::SET_IGNORE_HINT)}" }
                    }
                }

                div { class: "row", style: "margin-top: var(--sp2);",
                    button { class: "primary", onclick: save, "{t(i18n::k::SET_SAVE)}" }
                    button {
                        onclick: move |_| page_status.set(i18n::tt(i18n::k::SET_EFFECT_TIP).to_string()),
                        "{t(i18n::k::SET_EFFECT)}" }
                }
            }
        }
    }
}

/// 页面状态行（设置页/插件页共用样式）
#[component]
fn page_status_line(status: String) -> Element {
    let cls = if status.starts_with('❌') {
        "page-status bad"
    } else if status.starts_with('✅') {
        "page-status ok"
    } else {
        "page-status"
    };
    rsx! {
        div { class: "{cls}", "{status}" }
    }
}

// ── Wave3 主题变量审计测试 ──────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    /// 解析 theme_vars 输出为 有序 (变量名, 值) 列表（分号切分，剔空项）。
    fn parse_vars(s: &str) -> Vec<(String, String)> {
        s.split(';')
            .map(str::trim)
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let (name, val) = kv
                    .split_once(':')
                    .unwrap_or_else(|| panic!("变量缺冒号：{kv:?}"));
                (name.trim().to_string(), val.trim().to_string())
            })
            .collect()
    }

    /// 深浅两套 :root 变量集合必须完全一致（成对补齐，不许一边多/少一个变量），
    /// 且每个值都非空、形如颜色（#hex / rgba()）——防"深色改了浅色漏改"回归。
    #[test]
    fn theme_vars_light_and_dark_have_identical_key_sets() {
        let light = parse_vars(theme_vars("light"));
        let dark = parse_vars(theme_vars("dark"));
        assert!(!light.is_empty() && !dark.is_empty());

        let light_keys: Vec<&str> = light.iter().map(|(k, _)| k.as_str()).collect();
        let dark_keys: Vec<&str> = dark.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(light_keys, dark_keys, "深浅两套变量名必须一致且同序");

        for (name, val) in light.iter().chain(dark.iter()) {
            assert!(!val.is_empty(), "{name} 值为空");
            let is_color = (val.starts_with('#') && val.len() >= 4)
                || val.starts_with("rgba(")
                || val.starts_with("rgb(");
            assert!(is_color, "{name} 的值 {val:?} 不是颜色字面量");
        }

        // 语义变量齐备性：Wave3 新增的覆盖层/衬底变量两套都在
        for name in ["--danger-overlay", "--danger-overlay-soft", "--img-bg"] {
            assert!(light_keys.contains(&name), "浅色缺 {name}");
            assert!(dark_keys.contains(&name), "深色缺 {name}");
        }
    }

    /// 主题可识别性：theme_vars 只认 "light"，其余（dark / 未知值）走深色表，
    /// 且两表内容确实不同（防复制粘贴成同一份）。
    #[test]
    fn theme_vars_light_differs_from_dark() {
        assert_ne!(theme_vars("light"), theme_vars("dark"));
        assert_eq!(theme_vars("anything-else"), theme_vars("dark"));
    }

    /// O 波 #2：保存 → 信号更新的纯函数层。apply_settings_form 返回的主题值
    /// 就是保存后要 set 进 theme 信号的值（css() 随信号重算 → 热切换），
    /// 且全部表单字段正确落进 Settings 快照（含热键 trim）。
    #[test]
    fn apply_settings_form_returns_theme_for_hot_switch_and_fills_snapshot() {
        let mut st = settings::Settings::default();
        assert_eq!(st.theme, settings::DEFAULT_THEME);
        let (theme, lang) = apply_settings_form(
            &mut st,
            "  alt+q  ",
            4396,
            "en_US",
            "dark",
            vec!["^水印$".into()],
            true,
            false,
            true,
            "builtin",
        );
        // 主题桥：返回值 → theme.set(...) → css() 重算（热切换链路的可测段）
        assert_eq!(theme, "dark");
        assert_eq!(st.theme, "dark");
        assert_eq!(lang, "en_US");
        assert_eq!(st.hotkey, "alt+q", "热键保存需 trim");
        assert_eq!(st.http_port, 4396);
        assert_eq!(st.lang, "en_US");
        assert_eq!(st.ignore_rules, vec!["^水印$".to_string()]);
        assert!(st.notify && !st.sound && st.frameless);
        assert_eq!(st.picker_mode, "builtin");
    }

    /// O 波 #3：checkbox 对齐修复的静态断言——css() 必须带 .chk 组合规则；
    /// 源码里每一处 checkbox 都必须挂在含 "chk" 的 label 组合下；并锚定当前
    /// checkbox 总数（识别页公式 + 设置页 3 开关 + 批量页二维码 = 5），
    /// 防止后续新增 checkbox 漏套样式再次错位。
    #[test]
    fn chk_css_rules_present_and_all_checkboxes_covered() {
        let css_text = css("dark");
        assert!(css_text.contains("label.chk"), "css 缺 .chk 组合规则");
        assert!(
            css_text.contains("label.chk input[type=\"checkbox\"]"),
            "css 缺 checkbox 固定尺寸规则"
        );

        let main_src = include_str!("main.rs");
        let batch_src = include_str!("batch.rs");
        for (file, src) in [("src/main.rs", main_src), ("src/batch.rs", batch_src)] {
            let mut scanned = 0;
            let mut from = 0;
            while let Some(rel) = src[from..].find("r#type: \"checkbox\"") {
                let at = from + rel;
                // checkbox 之前最近的 label 开标签：组合 class 应含 "chk"
                let label_at = src[..at].rfind("label {").expect("checkbox 必在 label 组合内");
                let window = &src[label_at..at];
                assert!(
                    window.contains("chk"),
                    "{file} 第 {} 处 checkbox 未套 .chk 组合（对齐回归）",
                    scanned + 1
                );
                scanned += 1;
                from = at + "r#type: \"checkbox\"".len();
            }
            if file == "src/main.rs" {
                assert_eq!(scanned, 4, "main.rs 应有 4 处 checkbox（公式 + 设置页 3 开关）");
            } else {
                assert_eq!(scanned, 1, "batch.rs 应有 1 处 checkbox（顺带扫二维码）");
            }
        }
        let total = main_src.matches("r#type: \"checkbox\"").count()
            + batch_src.matches("r#type: \"checkbox\"").count();
        assert_eq!(total, 5, "checkbox 总数锚定：新增时须同步套 .chk 并更新本测试");
    }
}
