//! ocr_flow —— 捕获→识别流程（M1 迁出 + M3b 引擎分发 + Wave2-C 交互细化）。
//!
//! M1 行为保留：busy 互斥、Capturer::probe、捕获→PNG data-URL→结果面板、
//! OCR 重活在 std 线程（结果经 oneshot 回 dioxus spawn）。
//! M3b：识别一步按 settings.engine 分发——
//! - `native` → rumi_ocr::OcrEngine（模型常驻）
//! - `plugin:<名>` → plugins_ui::run_bytes_recognize（Frame 内存编码 PNG，
//!   走 worker 的 runBytes，免落盘；plugins-worker API：`runBytes(&[u8]) -> Result<Value>`）
//!
//! Wave2-C 新增：
//! - [`OcrOpts`]（P2 #11/#12）：排版解析 + 忽略规则 参数；识别分两层——
//!   [`recognize_raw`] 出原料（引擎分发 + 忽略区域），[`derive_display`] 应用
//!   排版/忽略规则（native / 插件两条路径统一走 rumi_ocr::postprocess，插件
//!   box 是占位值，NaturalPara 会退化成单段——见 [`apply_postprocess`] 注释）。
//!   分层的动机是**排版热重排**：识别完成后切排版下拉，对原料重派生即可；
//! - [`paste_and_ocr`]（P1 #8）：arboard 读剪贴板图像 → 与捕获同一识别管线；
//! - [`HistoryItem`]（P1 #7）：每次识别 append（回填用）；打磨阶段改为
//!   **磁盘缓存 + 内存缩略图**：完整 PNG 落 `~/.cache/rumi/history/<ts>_<seq>.png`，
//!   内存条目只留 缩略图 data-url（最长边 480px）+ 文本/分数/宽高，点击回填时
//!   懒加载完整图（文件被清则行内提示并禁用回填），启动时扫目录超 50 个删最旧；
//! - 识别完成桌面通知（P1 #10，settings.notify 开关，notify-rust 后台线程）。
//!
//! 忽略区域（P0 坐标版防水印）：[`apply_ignore_regions`] 在 [`recognize_raw`]
//! 入口（引擎分发前）按 settings.ignore_regions 把矩形内置白，native / 公式 /
//! 插件三条路径共用；预览图与历史 PNG 仍是原图（只在识别帧上置白）。忽略后
//! 全白帧识别自然为空行，无需特判。**batch 批量页未接线，待后续**（批量识别
//! 有独立路径，不经过本函数）。
//!
//! J2 新增：公式模式（settings.formula_mode，native 引擎分流到
//! recognize_structured，排版后处理不应用、忽略规则仍生效，见
//! [`apply_structured_postprocess`] 注释）+ 完成提示音（[`sound`]，合成
//! 两短音，settings.sound 开关）。
//!
//! 状态消息词条全部走 i18n：这些串在 spawn 闭包等非组件上下文产生，
//! 用 `i18n::tt()`（读全局原子语言，main/设置页维护）取词。

use base64::Engine;
use dioxus::prelude::*;
use futures::channel::oneshot;
use rumi_capture::{Capturer, Frame};
use rumi_ocr::postprocess::{Layout, PostRules};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::i18n;

/// 历史记录上限（P1 #7）
pub const HISTORY_CAP: usize = 50;

/// 识别结果面板的统一行（native 与插件输出都映射到这里）
#[derive(Clone, PartialEq)]
pub struct UiLine {
    pub text: String,
    /// 展示用置信度文本；插件未提供时为空串
    pub score: String,
    /// 定位框（**图像像素** [x0,y0,x1,y1] 轴对齐）：点结果行 → 预览图高亮对应区域。
    /// 按行 = 本行框；合并/分段 = 组内成员框并集；公式 = 逐行框。
    /// None = 无坐标可定位（插件占位框 / 历史回填行）。
    pub box_px: Option<[f32; 4]>,
}

/// 识别后处理选项（P2 #11/#12）：排版策略 + 忽略正则。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrOpts {
    pub layout: Layout,
    /// 忽略正则原文列表（编译失败项由 postprocess 跳过并记入 bad_regex）
    pub ignore: Vec<String>,
}

impl OcrOpts {
    /// 从 settings 组装（识别页/批量页共用一份配置）
    pub fn from_settings() -> Self {
        let st = crate::settings::get();
        Self {
            layout: parse_layout(&st.layout),
            ignore: st.ignore_rules.clone(),
        }
    }
}

/// 排版策略 ↔ 设置存储串映射（P2 #11，纯函数可测）。
/// 未知串回退 ByLine（默认按行）。
pub fn parse_layout(s: &str) -> Layout {
    match s {
        "merge_para" => Layout::MergePara,
        "natural_para" => Layout::NaturalPara,
        _ => Layout::ByLine,
    }
}

/// [`parse_layout`] 的逆映射（下拉框 option value 用）
pub fn layout_key(l: Layout) -> &'static str {
    match l {
        Layout::ByLine => "by_line",
        Layout::MergePara => "merge_para",
        Layout::NaturalPara => "natural_para",
    }
}

/// 应用后处理 → 面板行。分数取组内成员的均值（ByLine 时就是本行分数）。
///
/// 插件路径注意：worker 输出的 box 为占位值（全零 / None → 全零），
/// NaturalPara 的"行垂直间距聚类"在占位框上间距恒 0、永远不大于阈值，
/// 会退化成"全部合并成一段"——与 Umi 的插件后处理局限一致，接受并在此注明。
/// 行四点框 → 轴对齐 bbox（图像像素 [x0,y0,x1,y1]）。全零占位框（插件路径，
/// 见 recognize_raw）返回 None——无坐标可定位。
fn line_bbox(b: &[[f32; 2]; 4]) -> Option<[f32; 4]> {
    if b.iter().all(|p| p[0] == 0.0 && p[1] == 0.0) {
        return None;
    }
    let xs = [b[0][0], b[1][0], b[2][0], b[3][0]];
    let ys = [b[0][1], b[1][1], b[2][1], b[3][1]];
    Some([
        xs.into_iter().fold(f32::INFINITY, f32::min),
        ys.into_iter().fold(f32::INFINITY, f32::min),
        xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
        ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
    ])
}

/// 组（段）定位框：成员行 bbox 的并集；组内无任何有效框 → None。
/// 合并段落/自然分段下一段覆盖多行，定位高亮"这整段在图里的位置"。
fn group_bbox(lines: &[rumi_ocr::OcrLine], group: &[usize]) -> Option<[f32; 4]> {
    let mut acc: Option<[f32; 4]> = None;
    for &i in group {
        let Some(b) = lines.get(i).and_then(|l| line_bbox(&l.r#box)) else { continue };
        acc = Some(match acc {
            None => b,
            Some(o) => [o[0].min(b[0]), o[1].min(b[1]), o[2].max(b[2]), o[3].max(b[3])],
        });
    }
    acc
}

/// postprocess 结果 → 展示行（组内分数均值 + 定位框并集）。
/// 识别页与批量页共用——此前 batch.rs 手抄过同一映射，收敛到此处单一来源。
pub fn processed_to_ui(
    p: rumi_ocr::postprocess::Processed,
    lines: &[rumi_ocr::OcrLine],
) -> Vec<UiLine> {
    p.texts
        .into_iter()
        .zip(p.groups)
        .map(|(text, group)| {
            let scores: Vec<f32> =
                group.iter().filter_map(|&i| lines.get(i).map(|l| l.score)).collect();
            let avg = if scores.is_empty() {
                0.0
            } else {
                scores.iter().sum::<f32>() / scores.len() as f32
            };
            UiLine {
                text,
                score: format!("{avg:.2}"),
                box_px: group_bbox(lines, &group),
            }
        })
        .collect()
}

fn apply_postprocess(
    lines: Vec<rumi_ocr::OcrLine>,
    opts: &OcrOpts,
) -> Vec<UiLine> {
    let rules = PostRules { ignore_regex: opts.ignore.clone() };
    let p = rumi_ocr::postprocess::postprocess(&lines, opts.layout, &rules);
    processed_to_ui(p, &lines)
}

/// 识别引擎状态（结果面板上方状态行；描述 native 引擎生命周期，
/// 插件引擎时 App 会在旁边叠加当前引擎名）
#[derive(Debug, Clone, PartialEq)]
pub enum EngineState {
    /// 构建 ONNX session 中（首次含 ModelScope 模型下载）
    Init,
    Ready,
    /// recognize 进行中
    Busy,
    Error(String),
}

impl EngineState {
    pub fn label(&self) -> String {
        match self {
            EngineState::Init => i18n::tt(i18n::k::ENG_INIT).to_string(),
            EngineState::Ready => i18n::tt(i18n::k::ENG_READY).to_string(),
            EngineState::Busy => i18n::tt(i18n::k::ENG_BUSY).to_string(),
            EngineState::Error(e) => i18n::fill(i18n::tt(i18n::k::ENG_ERROR), e),
        }
    }
}

/// 捕获方式（与 M1 一致）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Full,
    Region,
    ActiveWindow,
}

impl Kind {
    /// 用户可见标签（i18n；状态消息在 spawn 闭包里用 tt() 取词）
    pub fn label(self) -> &'static str {
        let key = match self {
            Kind::Full => i18n::k::KIND_FULL,
            Kind::Region => i18n::k::KIND_REGION,
            Kind::ActiveWindow => i18n::k::KIND_ACTIVE,
        };
        i18n::tt(key)
    }
}

/// 历史记录条目（P1 #7）：识别完成即 append（时间倒序，上限 [`HISTORY_CAP`]）。
/// 打磨后内存只存轻量字段：完整 PNG 在磁盘（懒加载回填），内存仅留缩略图 data-url。
#[derive(Clone, PartialEq)]
pub struct HistoryItem {
    /// 识别完成时刻（unix 秒；显示层换算"刚刚/N 分钟前"相对时间，免时区处理）。
    /// 也是"缓存图缺失"标记的键（历史面板按它禁用回填，插入/删除不打乱对应关系）。
    pub at: u64,
    /// 摘要：首个非空行截断（列表展示）
    pub summary: String,
    /// 结果行数
    pub count: usize,
    /// 完整 PNG 所在磁盘路径（回填时懒加载；None = 写盘失败的历史条目）
    pub png_path: Option<PathBuf>,
    /// 缩略图 data-url（回填的即时反馈；最长边 [`THUMB_MAX_EDGE`]）
    pub thumb_url: String,
    /// 原图尺寸（回填后划选裁剪的坐标映射需要）
    pub w: u32,
    pub h: u32,
    /// 结果行文本（编辑前的识别原文本）
    pub texts: Vec<String>,
    /// 每行置信度文本
    pub scores: Vec<String>,
}

/// 历史缓存目录：`~/.cache/rumi/history`。`RUMI_HISTORY_DIR` 优先（测试/打包注入）。
pub fn history_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("RUMI_HISTORY_DIR") {
        return PathBuf::from(d);
    }
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("rumi")
        .join("history")
}

/// 历史文件名序号：同一秒内多次识别不重名
static HIST_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_hist_seq() -> u64 {
    HIST_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// 完整 PNG 落盘：`<目录>/<时间戳>_<序号>.png`（目录不存在则创建）。
pub fn save_history_png(dir: &Path, at: u64, seq: u64, png: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{at}_{seq}.png"));
    std::fs::write(&path, png)?;
    Ok(path)
}

/// 删除单个历史 PNG（清空/单条删除共用；文件已不在不算错误）
pub fn remove_history_png(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// 文件名 `<时间戳>_<序号>` → (ts, seq)（解析失败 → None）
fn parse_hist_name(path: &Path) -> Option<(u64, u64)> {
    let stem = path.file_stem()?.to_str()?;
    let (ts, seq) = stem.split_once('_')?;
    Some((ts.parse().ok()?, seq.parse().ok()?))
}

/// 扫描缓存目录，超过 `cap` 个 png 删最旧（LRU：文件名时间戳序；无法解析文件名的
/// 文件视为最旧优先删）。返回删除的文件数。启动时与识别落盘后调用。
pub fn prune_history_dir(dir: &Path, cap: usize) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut parsed: Vec<(PathBuf, (u64, u64))> = Vec::new();
    let mut unparsed: Vec<PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_file() || p.extension().map_or(true, |e| !e.eq_ignore_ascii_case("png")) {
            continue;
        }
        match parse_hist_name(&p) {
            Some(key) => parsed.push((p, key)),
            None => unparsed.push(p),
        }
    }
    parsed.sort_by_key(|(_, key)| *key); // 升序 = 最旧在前
    let total = parsed.len() + unparsed.len();
    let mut excess = total.saturating_sub(cap);
    let mut removed = 0usize;
    for p in unparsed.iter().chain(parsed.iter().map(|(p, _)| p)) {
        if excess == 0 {
            break;
        }
        remove_history_png(p);
        excess -= 1;
        removed += 1;
    }
    removed
}

/// 缩略图最长边（历史条目内存占用控制）
pub const THUMB_MAX_EDGE: u32 = 480;

/// PNG 字节 → 等比缩略图 PNG 字节（最长边钳到 [`THUMB_MAX_EDGE`]，小图不放大）。
/// 返回 (png, 宽, 高)；解码/编码失败返回 None。
pub fn thumb_png_of(png: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory(png).ok()?;
    let thumb = {
        let (w, h) = (img.width(), img.height());
        let long_edge = w.max(h);
        if long_edge <= THUMB_MAX_EDGE {
            img // 小图不放大
        } else {
            img.thumbnail(THUMB_MAX_EDGE, THUMB_MAX_EDGE) // 等比，最长边=480
        }
    };
    let (w, h) = (thumb.width(), thumb.height());
    let mut out = Vec::new();
    thumb
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some((out, w, h))
}

/// PNG 字节 → 缩略图 data-url（失败返回空串，调用方按"无图"降级）
pub fn make_thumb_url(png: &[u8]) -> String {
    thumb_png_of(png)
        .map(|(bytes, _, _)| png_data_url(&bytes))
        .unwrap_or_default()
}

/// PNG 字节 → 原尺寸 data-url（历史回填懒加载用；失败 None）
pub fn png_to_data_url(png: &[u8]) -> Option<String> {
    // 走一遍解码验证（防半截文件变成坏 <img>）
    image::load_from_memory(png).ok()?;
    Some(png_data_url(png))
}

fn png_data_url(png: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    format!("data:image/png;base64,{b64}")
}

/// 历史回填（P1 #7 打磨）：先用缩略图立即上屏（懒加载落地前的即时反馈），
/// 再从磁盘懒加载完整原图替换；文件被清（用户删缓存）→ 标记该条缺失，
/// 行内提示并禁用回填。在 UI 线程调用（onclick）。
pub fn restore_history(
    item: HistoryItem,
    mut hist_missing: Signal<BTreeSet<u64>>,
    mut shot: Signal<Option<String>>,
    mut shot_size: Signal<(u32, u32)>,
    mut lines: Signal<Vec<UiLine>>,
    mut texts: Signal<Vec<String>>,
    mut status: Signal<String>,
) {
    let idx_key = item.at;
    // 缩略图先上屏：行文本/尺寸同步回填，划选坐标映射不受影响（缩略图等比）
    if !item.thumb_url.is_empty() {
        let restored: Vec<UiLine> = item
            .texts
            .iter()
            .cloned()
            .zip(item.scores.iter().cloned())
            .map(|(text, score)| UiLine { text, score, box_px: None })
            .collect();
        shot.set(Some(item.thumb_url.clone()));
        shot_size.set((item.w, item.h));
        lines.set(restored);
        texts.set(item.texts.clone());
        status.set(i18n::tt(i18n::k::HIST_RESTORED).to_string());
    }
    let Some(path) = item.png_path.clone() else {
        // 写盘失败的旧条目：无完整图可回填
        hist_missing.with_mut(|s| {
            s.insert(idx_key);
        });
        status.set(i18n::tt(i18n::k::HIST_MISSING).to_string());
        return;
    };
    spawn(async move {
        let rx = run_blocking(move || std::fs::read(&path));
        match rx.await {
            Ok(Ok(bytes)) => match png_to_data_url(&bytes) {
                Some(url) => {
                    shot.set(Some(url));
                    status.set(i18n::tt(i18n::k::HIST_FULL_RESTORED).to_string());
                }
                None => {
                    hist_missing.with_mut(|s| {
                        s.insert(idx_key);
                    });
                    status.set(i18n::tt(i18n::k::HIST_MISSING).to_string());
                }
            },
            Ok(Err(_)) => {
                // 文件被清：行内提示 + 禁用回填（缩略图仍可见，文本不受影响）
                hist_missing.with_mut(|s| {
                    s.insert(idx_key);
                });
                status.set(i18n::tt(i18n::k::HIST_MISSING).to_string());
            }
            Err(_) => status.set(i18n::tt(i18n::k::STA_OCR_THREAD_LOST).to_string()),
        }
    });
}

/// 捕获/识别流程需要的信号束（全部 Copy，便于热键/托盘触发时捕获进闭包）。
/// Wave2-C 在 M1 基础上扩了 shot_size / texts / history 三个信号；
/// M 波追加 picker（内置冻结帧选择器的激活态：Some(data-url) = 全屏选择中）。
#[derive(Clone, Copy, PartialEq)]
pub struct CaptureCtx {
    pub shot: Signal<Option<String>>,
    /// 预览图原始像素尺寸（划选裁剪的坐标映射用）
    pub shot_size: Signal<(u32, u32)>,
    pub status: Signal<String>,
    pub engine_st: Signal<EngineState>,
    pub lines: Signal<Vec<UiLine>>,
    /// 结果文本（可编辑版，P1 #6；识别完成时重置为识别原文，复制/导出取编辑后内容）
    pub texts: Signal<Vec<String>>,
    /// 最近一次识别的原始行 + 管线类别（排版热重排的原料；None = 尚无结果）。
    /// 识别完成时与 lines/texts 同步写入；切换排版下拉对它重 derive_display 即换排版。
    pub raw: Signal<Option<(Vec<rumi_ocr::OcrLine>, ProcKind)>>,
    /// 文字定位（点结果行 → 预览图高亮）：Some((行号, 图像像素 bbox))；
    /// 再点同号取消，点新行切换。新识别 / 切排版时清空（行↔框对应关系已变）。
    pub locate: Signal<Option<(usize, [f32; 4])>>,
    /// 识别历史（P1 #7，最新在前）
    pub history: Signal<Vec<HistoryItem>>,
    /// 缓存图已缺失（磁盘文件被清）的历史条目键集合（item.at）；回填禁用+行内提示
    pub hist_missing: Signal<BTreeSet<u64>>,
    pub busy: Signal<bool>,
    /// M 波冻结帧选择器：Some(全屏帧 data-url) = 主窗口进入全屏拖选态；
    /// App 据此整页切换为选择器组件（遮住侧栏/标题栏以外的一切）。
    pub picker: Signal<Option<String>>,
}

// ── M 波：内置冻结帧选择器（主窗口临时全屏方案）──────────────────────────────
//
// 形态（主管裁决，规避 dioxus 多窗口 API 风险）：抓全屏帧 → 主窗口临时全屏显示
// 该帧 → 拖选矩形 → 松手裁剪子图 → 退出全屏 → 进现有识别管线；ESC 取消。
// 像素帧不进 Signal（大缓冲 clone 昂贵）：放在下面的进程级互斥槽里，
// picker 信号只携带显示用的 data-url。
//
// 帧里包含我们自己的窗口？不——抓帧前先隐藏主窗（抓帧是阻塞调用，放 std 线程；
// 隐藏/恢复窗口的 tao 调用留在 dioxus 主线程上下文，见 dioxus-desktop 的
// poll_vdom 全在 tao 事件循环主线程）。任何失败路径都必须恢复窗口可见性。

/// 冻结帧槽（选择期间的像素帧；`std::sync::Mutex::new` 是 const，可直接 static）
static PICKER_FRAME: Mutex<Option<Frame>> = Mutex::new(None);

/// 存入冻结帧（进入选择态前调用；重复进入自动覆盖旧帧）
pub fn picker_frame_store(frame: Frame) {
    *PICKER_FRAME.lock().unwrap_or_else(|p| p.into_inner()) = Some(frame);
}

/// 取走冻结帧（裁剪提交时；take 保证选择态结束即释放大缓冲）
pub fn picker_frame_take() -> Option<Frame> {
    PICKER_FRAME.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// 清空冻结帧（取消路径用，避免大缓冲滞留）
pub fn picker_frame_clear() {
    *PICKER_FRAME.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// 冻结帧原始尺寸（选择器顶栏的实时"W × H"读数用；不取走帧）
pub fn picker_frame_dims() -> Option<(u32, u32)> {
    PICKER_FRAME
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|f| (f.width, f.height))
}

/// 抓帧前隐藏主窗后留给合成器卸窗的时间（portal 往返天然有余量，
/// 这里主要覆盖 xcap/X11 的即时抓取，两帧足够）
const PICKER_HIDE_SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

/// 恢复主窗为常规态：退出全屏 + 确保可见（取消 / 失败 / 完成三条路径共用）。
/// 全部在 dioxus 主线程上下文调用（tao GTK 后端约束）。
pub fn window_restore_normal() {
    let dc = dioxus::desktop::window();
    dc.window
        .set_fullscreen(None::<dioxus::desktop::tao::window::Fullscreen>);
    dc.window.set_visible(true);
}

/// 冻结帧选区裁剪（纯函数可测）：显示坐标矩形 → 原图像素坐标 → 子帧。
///
/// - `disp` = 冻结帧在窗口内的显示尺寸（CSS px；stage 铺满全屏窗口）
/// - `rect` = (x0, y0, x1, y1) 任意方向的显示坐标拖选
/// - 比例映射 + 钳制口径与识别页 finish_area_copy 一致（floor/ceil + 帧界钳制）
/// - 选区在原图上不足 4px（视为普通点击）或显示尺寸未知 → None
pub fn crop_frame_region(
    frame: &Frame,
    disp: (f64, f64),
    rect: (f64, f64, f64, f64),
) -> Option<Frame> {
    let (x0, y0, x1, y1) = rect;
    let (disp_w, disp_h) = disp;
    let (nat_w, nat_h) = (frame.width, frame.height);
    if disp_w < 1.0 || disp_h < 1.0 || nat_w == 0 || nat_h == 0 {
        return None;
    }
    let sx = nat_w as f64 / disp_w;
    let sy = nat_h as f64 / disp_h;
    let ix0 = ((x0.min(x1) * sx).floor() as i64).clamp(0, nat_w as i64) as u32;
    let iy0 = ((y0.min(y1) * sy).floor() as i64).clamp(0, nat_h as i64) as u32;
    let ix1 = ((x1.max(x0) * sx).ceil() as i64).clamp(0, nat_w as i64) as u32;
    let iy1 = ((y1.max(y0) * sy).ceil() as i64).clamp(0, nat_h as i64) as u32;
    if ix1.saturating_sub(ix0) < 4 || iy1.saturating_sub(iy0) < 4 {
        return None; // 视为普通点击
    }
    let img = image::RgbaImage::from_raw(
        nat_w,
        nat_h,
        frame.rgba.clone(),
    )
    .and_then(|full| {
        Some(image::imageops::crop_imm(&full, ix0, iy0, ix1 - ix0, iy1 - iy0).to_image())
    })?;
    Some(Frame::from_rgba_image(img))
}

/// 选区方式决策（M 波）：Kind::Region 是否应改走内置冻结帧选择器。
/// builtin=强制走；native=永不；auto=看 caps（region_native=false 的后端接管）。
/// 需要 caps → 在拿到 Capturer 的 async 上下文里调用。
fn region_wants_builtin(cap: &Capturer) -> bool {
    match crate::settings::parse_picker_mode(&crate::settings::get().picker_mode) {
        crate::settings::PickerMode::Builtin => true,
        crate::settings::PickerMode::Native => false,
        crate::settings::PickerMode::Auto => !cap.caps().region_native,
    }
}

/// 发起一次捕获 → 按当前引擎识别 → 更新结果面板。
/// Capturer 的 async 调用放进 dioxus spawn，不阻塞 UI；识别在 std 线程跑。
/// M 波：Kind::Region 按选区方式决策可改走内置冻结帧选择器（捕获阶段先跑完，
/// 拖选完成后再回到本函数的识别尾段）。
pub fn capture_and_ocr(ctx: CaptureCtx, kind: Kind) {
    let CaptureCtx { mut status, mut busy, .. } = ctx;
    if busy() {
        return; // 上一轮还没完，避免并发捕获/识别互相踩
    }
    busy.set(true);
    status.set(i18n::fill(i18n::tt(i18n::k::STA_CAPTURING), kind.label()));
    // 体验批次：运行日志（设置页可查）——每次捕获真正开始记一条（Kind 已派生 Debug）
    crate::logger::info("ocr", format!("[ocr] 开始 kind={kind:?}"));
    spawn(async move {
        let cap = match Capturer::shared().await {
            Ok(c) => c,
            Err(e) => {
                let msg = i18n::fill(i18n::tt(i18n::k::STA_PROBE_FAIL), e);
                status.set(msg.clone());
                crate::logger::err("ocr", msg);
                busy.set(false);
                return;
            }
        };
        // M 波：区域捕获按选区方式分流到内置冻结帧选择器（busy 交接给它）
        if let Kind::Region = kind {
            if region_wants_builtin(cap) {
                busy.set(false);
                capture_region_builtin(ctx);
                return;
            }
        }
        let result = match kind {
            Kind::Full => cap.fullscreen().await,
            Kind::Region => cap.region_native().await,
            Kind::ActiveWindow => cap.active_window().await,
        };
        match result {
            Ok(frame) => match frame_to_data_url(&frame) {
                Ok(url) => {
                    status.set(i18n::filln(
                        i18n::tt(i18n::k::STA_CAP_OK),
                        &[&kind.label(), &frame.width, &frame.height],
                    ));
                    recognize_captured(ctx, frame, url, kind.label().to_string()).await
                }
                Err(e) => {
                    let msg = format!("{}{e}", i18n::tt(i18n::k::ERR_MARK));
                    status.set(msg.clone());
                    crate::logger::err("ocr", msg);
                    busy.set(false);
                }
            },
            Err(e) => {
                let msg = i18n::filln(
                    i18n::tt(i18n::k::STA_CAP_FAIL),
                    &[&kind.label(), &e],
                );
                status.set(msg.clone());
                crate::logger::err("ocr", msg);
                busy.set(false);
            }
        }
    });
}

/// M 波：内置冻结帧选择器入口（"区域截图"在 builtin/auto+region_native=false 时走）。
/// 流程：隐藏主窗 → 抓全屏帧（Wayland=portal 非交互全屏；xcap 平台直抓）→
/// 主窗转全屏并显示冻结帧（picker 信号驱动 App 渲染选择器组件）→ 拖选 →
/// [`finish_builtin_region`] / ESC → [`cancel_builtin_picker`]。
/// 调用方（capture_and_ocr 分流点 / 设置触发点）保证 busy 已复位，本函数自管 busy。
pub fn capture_region_builtin(ctx: CaptureCtx) {
    let CaptureCtx { mut status, mut busy, mut picker, .. } = ctx;
    if busy() || picker().is_some() {
        return;
    }
    busy.set(true);
    status.set(i18n::tt(i18n::k::PICKER_GRABBING).to_string());
    // 隐藏主窗：冻结帧不该包含自己的窗口（tao 调用在 dioxus 主线程上下文）
    dioxus::desktop::window().window.set_visible(false);
    // 抓帧是阻塞调用（portal 往返 / X11 GetImage），放 std 线程
    let rx = run_blocking(|| {
        std::thread::sleep(PICKER_HIDE_SETTLE);
        futures::executor::block_on(async {
            let cap = Capturer::shared().await?;
            cap.fullscreen().await
        })
    });
    // spawn_forever（根作用域）：本任务的最后一步 `picker.set(Some)` 会把触发它的
    // 组件（MainTab/热键宿主）随整页切换卸载掉——普通 spawn 会被连带取消。
    dioxus::core::spawn_forever(async move {
        let frame = match rx.await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                window_restore_normal();
                status.set(i18n::fill(i18n::tt(i18n::k::STA_CAP_FAIL), e));
                busy.set(false);
                return;
            }
            Err(_) => {
                window_restore_normal();
                status.set(i18n::tt(i18n::k::PICKER_THREAD_LOST).to_string());
                busy.set(false);
                return;
            }
        };
        let Ok(url) = frame_to_data_url(&frame) else {
            window_restore_normal();
            let msg = format!("{}{}", i18n::tt(i18n::k::ERR_MARK), i18n::tt(i18n::k::PICKER_ENCODE_FAIL));
            status.set(msg.clone());
            crate::logger::err("ocr", msg);
            busy.set(false);
            return;
        };
        picker_frame_store(frame);
        // 主窗转全屏并显示冻结帧（进入选择态；窗口在隐藏态，先全屏再可见）。
        // tao 的无边框全屏（Borderless(None) = 当前显示器），比 Exclusive 少一档
        // 模式切换延迟，冻结帧拖选场景足够。
        let dc = dioxus::desktop::window();
        dc.window
            .set_fullscreen(Some(dioxus::desktop::tao::window::Fullscreen::Borderless(
                None,
            )));
        dc.window.set_visible(true);
        dc.window.set_focus();
        status.set(i18n::tt(i18n::k::PICKER_HINT).to_string());
        picker.set(Some(url));
    });
}

/// 内置选择器取消（ESC / 选区过小视为点击）：回到常规窗口，busy 复位。
pub fn cancel_builtin_picker(ctx: CaptureCtx) {
    let CaptureCtx { mut status, mut busy, mut picker, .. } = ctx;
    if picker().is_none() {
        return;
    }
    picker_frame_clear();
    picker.set(None);
    window_restore_normal();
    status.set(i18n::tt(i18n::k::PICKER_CANCEL).to_string());
    busy.set(false);
}

/// 内置选择器拖选完成：裁剪子图 → 退出全屏 → 进现有识别尾段。
/// `rect`/`disp` 由选择器组件按显示坐标传入（见 [`crop_frame_region`]）。
/// 选区过小按取消处理（不识别、不报错）。
pub async fn finish_builtin_region(ctx: CaptureCtx, rect: (f64, f64, f64, f64), disp: (f64, f64)) {
    let CaptureCtx { mut status, mut busy, mut picker, .. } = ctx;
    let cropped = picker_frame_take().and_then(|f| crop_frame_region(&f, disp, rect));
    picker_frame_clear();
    picker.set(None);
    window_restore_normal();
    match cropped {
        Some(frame) => {
            let label = i18n::tt(i18n::k::KIND_REGION_BUILTIN).to_string();
            let Ok(url) = frame_to_data_url(&frame) else {
                let msg = format!(
                    "{}{}",
                    i18n::tt(i18n::k::ERR_MARK),
                    i18n::tt(i18n::k::PICKER_ENCODE_FAIL)
                );
                status.set(msg.clone());
                crate::logger::err("ocr", msg);
                busy.set(false);
                return;
            };
            status.set(i18n::filln(
                i18n::tt(i18n::k::STA_CAP_OK),
                &[&label, &frame.width, &frame.height],
            ));
            recognize_captured(ctx, frame, url, label).await;
        }
        None => {
            // 选区过小（普通点击）或帧已失效：按取消处理
            status.set(i18n::tt(i18n::k::PICKER_CANCEL).to_string());
            busy.set(false);
        }
    }
}

/// 把阻塞任务扔进 std 线程，返回可在 dioxus spawn 里 await 的 oneshot（M1 原样）。
pub fn run_blocking<T, F>(f: F) -> oneshot::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

/// 应用忽略区域（P0 坐标版防水印）：归一化坐标 × 帧宽高取整，矩形内像素置白。
///
/// - `regions` 元素为 `[x0, y0, x1, y1]`（图像归一化 ∈ 0..1，相对整图左上原点，
///   语义见 settings.ignore_regions 注释）
/// - 越界坐标先钳到 0..1、换算后钳到帧界；x0>x1 / y0>y1 自动交换
/// - 半开矩形 `[x0,x1) × [y0,y1)`：与 UI 拖选 min/max 换算一致，相邻框不重复刷边
/// - 空列表不动帧；缓冲长度与宽高不符（坏帧）时整帧不动，宁可漏白不可越界
pub fn apply_ignore_regions(frame: &mut Frame, regions: &[[f32; 4]]) {
    if regions.is_empty() {
        return;
    }
    let stride = frame.width as usize * 4;
    if frame.rgba.len() < frame.height as usize * stride {
        return;
    }
    let to_px = |v: f32, span: u32| -> i64 {
        ((v.clamp(0.0, 1.0) * span as f32).round() as i64).clamp(0, span as i64)
    };
    for r in regions {
        let (mut x0, mut x1) = (to_px(r[0], frame.width), to_px(r[2], frame.width));
        let (mut y0, mut y1) = (to_px(r[1], frame.height), to_px(r[3], frame.height));
        if x0 > x1 {
            std::mem::swap(&mut x0, &mut x1);
        }
        if y0 > y1 {
            std::mem::swap(&mut y0, &mut y1);
        }
        for y in y0..y1 {
            let row = y as usize * stride;
            for x in x0..x1 {
                let i = row + x as usize * 4;
                frame.rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
}

/// 公式（结构化）引擎是否已预热（J2）：recognize_structured 首次调用在引擎锁内
/// 自动构建（PP-DocLayout-S + PP-FormulaNet-S，约 230MB 模型下载）。该标志只用于
/// 状态行提示——冷启动首跑前提示"公式引擎初始化…"，成功识别后不再每帧刷提示。
static STRUCTURED_WARM: AtomicBool = AtomicBool::new(false);

/// 结构化引擎是否已成功识别过一次（见 [`STRUCTURED_WARM`]）。
pub fn structured_warm() -> bool {
    STRUCTURED_WARM.load(Ordering::Relaxed)
}

/// 标记结构化引擎已预热（O 波 #6）：App 启动时后台 `warmup_structured` 成功后
/// 调用，使 [`hint_formula_init`] 的"首次下载"提示不再出现。
pub fn mark_structured_warm() {
    STRUCTURED_WARM.store(true, Ordering::Relaxed);
}

/// 公式模式首跑提示（capture_and_ocr / paste_and_ocr 在派发识别线程前调用）：
/// 开关开着且结构化引擎未预热 → 状态行提示首次下载（构建本身由
/// recognize_raw 内的 recognize_structured 幂等完成）。
pub fn hint_formula_init(status: &mut Signal<String>) {
    if crate::settings::get().formula_mode && !structured_warm() {
        status.set(i18n::tt(i18n::k::STA_FORMULA_INIT).to_string());
    }
}

/// 识别分发（std 线程调用）：按 settings.engine 走 native 或插件 runBytes，
/// 两条路径统一过 [`apply_postprocess`]（P2 #11/#12 接入点）。
/// 入口先应用忽略区域（P0 防水印，引擎分发前置白）：对帧克隆置白，
/// 调用方持有的原图不受影响（预览/历史仍存原图）。
///
/// J2 公式模式：settings.formula_mode 开且引擎为 native 时，分流到
/// `OcrEngine::global().recognize_structured`（首次自动 warmup_structured，
/// 约 230MB 模型下载，见 ocr crate 注释）；结构化输出走
/// [`apply_structured_postprocess`]——**排版后处理不应用**。插件引擎无此开关
/// （结构化管线是 native 引擎的一部分，插件 worker 不感知）。
/// 识别结果的后处理类别（热重排时决定走哪条管线）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProcKind {
    /// 普通文字管线：ByLine / MergePara / NaturalPara 全部参与
    Normal,
    /// 结构化（公式）管线：排版不参与（版面语义行，仅忽略规则）
    Structured,
}

/// 引擎分发 + 忽略区域，返回**原始行 + 管线类别**（不做排版后处理）。
/// 排版/忽略规则留给 [`derive_display`]——识别完成后切换排版下拉，
/// 可对同一份原料即时重解析（热重排），无需重新截图。
pub fn recognize_raw(frame: &Frame) -> Result<(Vec<rumi_ocr::OcrLine>, ProcKind), String> {
    let st = crate::settings::get();
    let mut masked = frame.clone();
    // 忽略区域先于任何分流统一生效（P0 防水印：native / 公式 / 插件三条路径共用）
    apply_ignore_regions(&mut masked, &st.ignore_regions);
    // Frame → ocr 统一图像载体一次到位（K2：引擎只认 image::RgbaImage；
    // masked 归本函数所有，move 零拷贝），三条路径共用同一份
    let img = masked.into_rgba_image();
    match st.engine_plugin() {
        None if st.formula_mode => {
            // 结构化识别：版面检测 + 文字 + 公式行→LaTeX（首次含模型下载）
            let raw = rumi_ocr::OcrEngine::global()
                .recognize_structured(&img)
                .map_err(|e| e.to_string())?;
            STRUCTURED_WARM.store(true, Ordering::Relaxed);
            Ok((raw, ProcKind::Structured))
        }
        None => {
            let raw = rumi_ocr::OcrEngine::global()
                .recognize(&img)
                .map_err(|e| e.to_string())?;
            Ok((raw, ProcKind::Normal))
        }
        Some(name) => {
            let png = frame_to_png(&img)?;
            let raw = crate::plugins_ui::run_bytes_recognize(name, &png)?;
            // 插件 box 是占位值：全零框使 NaturalPara 退化为单段（见 apply_postprocess）
            let raw = raw
                .into_iter()
                .map(|l| rumi_ocr::OcrLine {
                    text: l.text,
                    score: l.score.unwrap_or(0.0),
                    r#box: l.box_.unwrap_or([[0.0; 2]; 4]),
                })
                .collect();
            Ok((raw, ProcKind::Normal))
        }
    }
}

/// 原始行 → 展示行（排版/忽略规则在此应用；可对同一原料重复调用＝热重排）。
pub fn derive_display(
    raw: &[rumi_ocr::OcrLine],
    kind: ProcKind,
    opts: &OcrOpts,
) -> Vec<UiLine> {
    match kind {
        ProcKind::Normal => apply_postprocess(raw.to_vec(), opts),
        ProcKind::Structured => apply_structured_postprocess(raw.to_vec(), opts),
    }
}

/// 结构化（公式）输出的后处理（J2）：**只做忽略规则逐行剔除，不合并行**。
///
/// 为什么不用 apply_postprocess：结构化输出的行已是版面语义行——公式行的
/// text 是 LaTeX、box 是版面输出框，再走 ByLine/MergePara/NaturalPara 的
/// 行合并/段落聚类毫无意义（会把相邻 LaTeX 行并成一段、丢掉版面归属）。
/// 因此固定按 ByLine（1 行 = 1 组）过 PostRules：忽略规则照常生效、
/// 坏正则跳过，排版选项（settings.layout）在公式模式下不参与。
fn apply_structured_postprocess(
    lines: Vec<rumi_ocr::OcrLine>,
    opts: &OcrOpts,
) -> Vec<UiLine> {
    let rules = PostRules { ignore_regex: opts.ignore.clone() };
    // 公式管线豁免排版（ByLine 组内恒为单行，postprocess 1:1 保证），
    // 逐行框即定位框
    let p = rumi_ocr::postprocess::postprocess(&lines, Layout::ByLine, &rules);
    processed_to_ui(p, &lines)
}

/// 识别完成后的面板更新 + 历史落盘 + 桌面通知（P1 #7/#10；dioxus spawn 内 await 调用）。
/// 历史：完整 PNG 写 `~/.cache/rumi/history/`，内存只留缩略图（P1 #7 打磨）。
/// O 波 #7：`elapsed` = 本次识别耗时（纯识别段，不含捕获/编码），进状态行展示。
async fn finish_recognize(
    ctx: &CaptureCtx,
    frame: &Frame,
    label: &str,
    ls: Vec<UiLine>,
    elapsed: std::time::Duration,
) {
    let CaptureCtx {
        mut engine_st,
        mut status,
        mut lines,
        mut texts,
        mut history,
        mut locate,
        ..
    } = *ctx;
    engine_st.set(EngineState::Ready);
    // 新结果上线：清掉上一张图的定位高亮（行号↔框对应已失效）
    locate.set(None);
    let count = ls.len();
    let scores: Vec<String> = ls.iter().map(|l| l.score.clone()).collect();
    let summary = ls
        .iter()
        .map(|l| l.text.trim())
        .find(|t| !t.is_empty())
        .map(|t| {
            let mut s: String = t.chars().take(24).collect();
            if t.chars().count() > 24 {
                s.push('…');
            }
            s
        })
        .unwrap_or_else(|| i18n::tt(i18n::k::STA_NO_TEXT).to_string());
    lines.set(ls.clone());
    texts.set(ls.iter().map(|l| l.text.clone()).collect());
    // 体验批次：运行日志——识别成功落地记行数（设置页日志卡可回查）
    crate::logger::info("ocr", format!("[ocr] 完成 lines={count}"));
    // 历史落盘：完整 PNG → 缓存目录；内存条目只留缩略图（磁盘 IO 放 std 线程）
    let png = frame_to_png(&frame.to_rgba_image()).unwrap_or_default();
    let at = now_secs();
    let rx = run_blocking(move || {
        // PNG 编码失败（空字节）不落盘：省一个 0 字节垃圾文件，回填侧按缺失降级
        let path = if png.is_empty() {
            None
        } else {
            save_history_png(&history_dir(), at, next_hist_seq(), &png).ok()
        };
        let thumb = make_thumb_url(&png);
        (path, thumb)
    });
    let (png_path, thumb_url) = rx.await.unwrap_or((None, String::new()));
    history.with_mut(|v| {
        // 最新在前，超上限丢最旧（内存条目 + 对应磁盘文件一起清）
        let mut dropped: Vec<HistoryItem> = Vec::new();
        v.insert(
            0,
            HistoryItem {
                at,
                summary,
                count,
                png_path,
                thumb_url,
                w: frame.width,
                h: frame.height,
                texts: ls.iter().map(|l| l.text.clone()).collect(),
                scores,
            },
        );
        while v.len() > HISTORY_CAP {
            match v.pop() {
                Some(old) => dropped.push(old),
                None => break,
            }
        }
        for old in dropped {
            if let Some(p) = &old.png_path {
                remove_history_png(p);
            }
        }
    });
    // 启动后目录兜底再钳一次（含用户手动放进来的多余文件）
    spawn(async move {
        let dir = history_dir();
        let _ = run_blocking(move || prune_history_dir(&dir, HISTORY_CAP)).await;
    });
    // O 波 #7：状态行带行数 + 耗时（"✅ …识别完成: 6 行 · 1.2s"）
    let secs = format!("{:.1}", elapsed.as_secs_f64());
    status.set(i18n::filln(
        i18n::tt(i18n::k::STA_RECOGNIZE_DONE),
        &[&label, &count, &secs],
    ));
    // 提示音（J2 #4，默认关）：settings.sound 开时播放合成两短音；
    // fire-and-forget std 线程，失败静默（无音频设备不报错，见 sound 注释）
    if crate::settings::get().sound {
        sound::play_done_chime();
    }
    // 桌面通知（P1 #10，默认关）：后台线程，失败静默（通知是尽力而为的副产物）。
    // notify-rust 仅 Linux（zbus，见 ui/Cargo.toml 平台门控）；非 Linux 记日志留 TODO
    if crate::settings::get().notify {
        #[cfg(target_os = "linux")]
        {
            let body = i18n::fill(i18n::tt(i18n::k::NTF_DONE), count);
            std::thread::spawn(move || {
                let _ = notify_rust::Notification::new()
                    .summary("Rumi OCR")
                    .body(&body)
                    .show();
            });
        }
        #[cfg(not(target_os = "linux"))]
        {
            crate::logger::err("notify", "桌面通知暂仅支持 Linux".into());
        }
    }
}

/// 提示音（J2 #4）：无资产文件，运行时合成 880→1320Hz 两短音
/// （各 0.08s、48kHz f32，合计 0.16s），rodio 播放。开关在 settings.sound。
///
/// 播放放 std 线程 fire-and-forget：OutputStream / Sink 构建失败（无音频设备、
/// PulseAudio/PipeWire 不可达、无权限）一律静默返回——提示音是尽力而为的
/// 副产物，绝不影响识别主流程。测试只覆盖纯合成函数（不动音频设备）。
pub mod sound {
    /// 每段短音时长（秒）；两段合计 0.16s
    pub const TONE_SECS: f32 = 0.08;
    /// 播放采样率（48kHz f32 mono，rodio SamplesBuffer 直喂数据）
    pub const SAMPLE_RATE: u32 = 48_000;

    /// 合成提示音样本（纯函数可测）：880Hz 一短音接 1320Hz 一短音，
    /// 每段 [`TONE_SECS`]；段首尾 5ms 线性淡入淡出（正弦突然截断会有"啪"声）。
    pub fn chime_samples() -> Vec<f32> {
        const FADE: usize = (0.005 * SAMPLE_RATE as f32) as usize; // 240 样本
        let n = (SAMPLE_RATE as f32 * TONE_SECS) as usize; // 3840 样本/段
        let mut out = Vec::with_capacity(n * 2);
        for freq in [880.0f32, 1320.0] {
            for i in 0..n {
                let t = i as f32 / SAMPLE_RATE as f32;
                // 包络：段内前 FADE 个样本 0→1，末 FADE 个样本 1→0
                let env = ((i as f32 / FADE as f32).min(1.0))
                    * (((n - 1 - i) as f32 / FADE as f32).min(1.0));
                out.push(0.35 * env * (2.0 * std::f32::consts::PI * freq * t).sin());
            }
        }
        out
    }

    /// 播放完成提示音（settings.sound 为真时由 [`finish_recognize`] 调用）。
    /// std 线程 fire-and-forget：任何失败静默。48kHz f32 单声道直进 rodio。
    pub fn play_done_chime() {
        std::thread::spawn(|| {
            // rodio 0.20 与 0.19 同形：try_default 返回 (stream, handle) 元组
            let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
                return; // 无音频设备等 → 静默
            };
            let Ok(sink) = rodio::Sink::try_new(&handle) else {
                return;
            };
            let src = rodio::buffer::SamplesBuffer::new(1, SAMPLE_RATE, chime_samples());
            sink.append(src);
            sink.sleep_until_end(); // 线程驻留到播放完（fire-and-forget 的一次性生命周期）
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 合成提示音：非空 f32 缓冲，时长 ≈0.16s（两段 0.08s）；首样本淡入为 0；
        /// 前段主频 880Hz、后段 1320Hz（按段内过零数区分，880Hz 过零 ≈ 段长×2×880/48000）。
        #[test]
        fn chime_samples_nonempty_and_about_160ms() {
            let samples = chime_samples();
            assert!(!samples.is_empty());
            let dur = samples.len() as f32 / SAMPLE_RATE as f32;
            assert!((dur - 2.0 * TONE_SECS).abs() < 1e-3, "时长 {dur}s 应 ≈0.16s");
            // 首样本淡入为 0；全部样本幅度有界（包络 × 音量）
            assert!(samples[0].abs() < 1e-6);
            assert!(samples.iter().all(|s| s.abs() <= 0.36));
            // 两段各 n 个样本：880Hz 段过零 ≈ 2×0.08×880 ≈ 141 次，1320Hz ≈ 211 次
            let n = samples.len() / 2;
            let zeros = |seg: &[f32]| {
                seg.windows(2).filter(|w| w[0].signum() * w[1].signum() < 0.0).count()
            };
            let (z1, z2) = (zeros(&samples[..n]), zeros(&samples[n..]));
            assert!((100..=180).contains(&z1), "前段应主频 880Hz（过零 {z1}）");
            assert!(z2 > z1 + 30, "后段应主频更高（1320Hz，过零 {z2} > {z1}）");
        }
    }
}

/// 当前 unix 秒（历史时间戳用；显示层只做相对时间，免时区处理）
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 历史条目的相对时间文案（纯函数可测）：免时区处理，用"多久以前"表达。
pub fn history_age(lang: &str, at: u64, now: u64) -> String {
    let key = |k: &'static str| crate::i18n::ts(lang, k);
    let d = now.saturating_sub(at);
    if d < 60 {
        key(crate::i18n::k::AGE_JUST_NOW).to_string()
    } else if d < 3600 {
        crate::i18n::fill(key(crate::i18n::k::AGE_MIN), d / 60)
    } else if d < 86400 {
        crate::i18n::fill(key(crate::i18n::k::AGE_HOUR), d / 3600)
    } else {
        crate::i18n::fill(key(crate::i18n::k::AGE_DAY), d / 86400)
    }
}

/// RGBA 图像 → PNG 内存编码（插件 runBytes 与 data-URL 共用）。
///
/// 参数是 `image::RgbaImage`（K2：ocr/引擎侧的统一图像载体）；调用方手里
/// 还是 capture `Frame` 时先 `to_rgba_image()` / `into_rgba_image()` 转一次。
pub fn frame_to_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut png: Vec<u8> = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("PNG 编码: {e}"))?;
    Ok(png)
}

/// RGBA Frame → PNG 内存编码 → base64 data-URL（M1 原样）
pub fn frame_to_data_url(frame: &Frame) -> Result<String, String> {
    let png = frame_to_png(&frame.to_rgba_image())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok(format!("data:image/png;base64,{b64}"))
}

/// 捕获尾段共用（按钮/热键/托盘、内置冻结帧选择器提交、窗口选择器提交都走这里）：
/// 面板回填 + 后台识别 + 历史落盘 + 通知/提示音 + busy 复位。
/// 状态行的"✅ {0}成功: …"由调用方先行设置（各自捕获方式标签不同）。
/// dioxus spawn 内 await 调用；结束后 busy 一律复位，调用方无需再管。
pub async fn recognize_captured(ctx: CaptureCtx, frame: Frame, url: String, label: String) {
    let CaptureCtx {
        mut shot,
        mut shot_size,
        mut status,
        mut engine_st,
        mut busy,
        mut raw,
        ..
    } = ctx;
    shot.set(Some(url));
    shot_size.set((frame.width, frame.height));
    engine_st.set(EngineState::Busy);
    // 选项在捕获时取一次快照（识别线程读不到 Dioxus 信号）
    let opts = OcrOpts::from_settings();
    // J2：公式模式冷启动首跑提示（首次结构化识别含 ~230MB 模型下载）
    hint_formula_init(&mut status);
    let frame_for_ocr = frame.clone();
    // O 波 #7：计时只覆盖纯识别段（含识别线程排队），捕获/编码不计入
    let rx = run_blocking(move || {
        let t0 = std::time::Instant::now();
        let res = recognize_raw(&frame_for_ocr);
        (res, t0.elapsed())
    });
    match rx.await {
        Ok((Ok((raw_lines, kind)), elapsed)) => {
            // 存原料供排版热重排（main.rs 下拉切换时对同一原料重 derive_display）
            raw.set(Some((raw_lines.clone(), kind)));
            let ls = derive_display(&raw_lines, kind, &opts);
            finish_recognize(&ctx, &frame, &label, ls, elapsed).await
        }
        Ok((Err(e), _)) => {
            engine_st.set(EngineState::Error(e.clone()));
            let msg = i18n::fill(i18n::tt(i18n::k::STA_RECOGNIZE_FAIL), e);
            status.set(msg.clone());
            crate::logger::err("ocr", msg);
        }
        Err(_) => {
            let lost = i18n::tt(i18n::k::STA_OCR_THREAD_LOST).to_string();
            engine_st.set(EngineState::Error(lost.clone()));
            status.set(lost.clone());
            crate::logger::err("ocr", lost);
        }
    }
    busy.set(false);
}

/// 粘贴识别（P1 #8）：arboard 读剪贴板图像 → 与捕获同一识别管线。
/// 按钮点击 / 识别页窗口内 Ctrl+V 都走这里。
pub fn paste_and_ocr(ctx: CaptureCtx) {
    let CaptureCtx {
        mut shot,
        mut shot_size,
        mut status,
        mut engine_st,
        mut busy,
        ..
    } = ctx;
    if busy() {
        return;
    }
    busy.set(true);
    status.set(i18n::tt(i18n::k::STA_CLIP_READING).to_string());
    // arboard 读图是阻塞调用（Wayland 上尤其），放 std 线程
    let rx = run_blocking(|| {
        let img = crate::clipboard()?.get_image().map_err(|e| format!("读取剪贴板图像: {e}"))?;
        let frame = Frame {
            width: img.width as u32,
            height: img.height as u32,
            rgba: img.bytes.into_owned(),
        };
        let url = frame_to_data_url(&frame)?;
        Ok::<_, String>((frame, url))
    });
    spawn(async move {
        match rx.await {
            Ok(Ok((frame, url))) => {
                shot.set(Some(url));
                shot_size.set((frame.width, frame.height));
                status.set(i18n::filln(
                    i18n::tt(i18n::k::STA_CLIP_OK),
                    &[&frame.width, &frame.height],
                ));
                engine_st.set(EngineState::Busy);
                let opts = OcrOpts::from_settings();
                // J2：公式模式冷启动首跑提示（同捕获路径）
                hint_formula_init(&mut status);
                let frame_for_ocr = frame.clone();
                // O 波 #7：计时只覆盖纯识别段（同 capture 路径）
                let rrx = run_blocking(move || {
                    let t0 = std::time::Instant::now();
                    let res = recognize_raw(&frame_for_ocr);
                    (res, t0.elapsed())
                });
                let label = i18n::tt(i18n::k::STA_CLIP_LABEL);
                let CaptureCtx { mut raw, .. } = ctx;
                match rrx.await {
                    Ok((Ok((raw_lines, kind)), elapsed)) => {
                        // 存原料供排版热重排（同 capture 路径）
                        raw.set(Some((raw_lines.clone(), kind)));
                        let ls = derive_display(&raw_lines, kind, &opts);
                        finish_recognize(&ctx, &frame, label, ls, elapsed).await
                    }
                    Ok((Err(e), _)) => {
                        engine_st.set(EngineState::Error(e.clone()));
                        let msg = i18n::fill(i18n::tt(i18n::k::STA_RECOGNIZE_FAIL), e);
                        status.set(msg.clone());
                        crate::logger::err("ocr", msg);
                    }
                    Err(_) => {
                        let lost = i18n::tt(i18n::k::STA_OCR_THREAD_LOST).to_string();
                        engine_st.set(EngineState::Error(lost.clone()));
                        status.set(lost.clone());
                        crate::logger::err("ocr", lost);
                    }
                }
            }
            Ok(Err(e)) => status.set(i18n::fill(i18n::tt(i18n::k::STA_CLIP_FAIL), e)),
            Err(_) => status.set(i18n::tt(i18n::k::STA_CLIP_THREAD_LOST).to_string()),
        }
        busy.set(false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 排版选项映射（P2 #11）：下拉 value ↔ Layout 双向，未知串回退 ByLine
    #[test]
    fn layout_mapping_roundtrip_and_fallback() {
        for (key, layout) in [
            ("by_line", Layout::ByLine),
            ("merge_para", Layout::MergePara),
            ("natural_para", Layout::NaturalPara),
        ] {
            assert_eq!(parse_layout(key), layout);
            assert_eq!(layout_key(layout), key);
        }
        assert_eq!(parse_layout(""), Layout::ByLine);
        assert_eq!(parse_layout("garbage"), Layout::ByLine);
        // 默认设置即按行
        assert_eq!(parse_layout(crate::settings::DEFAULT_LAYOUT), Layout::ByLine);
    }

    /// 后处理接入（native 路径同一入口）：合并 + 忽略 + 分数取均值
    #[test]
    fn apply_postprocess_merges_filters_and_averages_scores() {
        let line = |t: &str, s: f32, y: f32| rumi_ocr::OcrLine {
            text: t.into(),
            score: s,
            r#box: [[0.0, y], [100.0, y], [100.0, y + 30.0], [0.0, y + 30.0]],
        };
        let lines = vec![line("你好", 0.9, 0.0), line("世界", 0.7, 20.0), line("SECRET", 1.0, 40.0)];
        let opts = OcrOpts {
            layout: Layout::MergePara,
            ignore: vec!["SECRET".into()],
        };
        let out = apply_postprocess(lines, &opts);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "你好世界"); // CJK 相连不加空格
        assert_eq!(out[0].score, "0.80"); // (0.9 + 0.7) / 2
    }

    /// 排版热重排：同一份原料按不同排版重复 derive_display，结果随排版变化——
    /// 识别完成后切排版下拉对原料重派生即可，无需重新截图识别（回归保护）。
    #[test]
    fn derive_display_regroups_same_raw_per_layout() {
        let line = |t: &str, y: f32| rumi_ocr::OcrLine {
            text: t.into(),
            score: 0.9,
            r#box: [[0.0, y], [100.0, y], [100.0, y + 30.0], [0.0, y + 30.0]],
        };
        // 两行同段（行高 30、行距 40 → 间隙 10 < 阈值 24）+ 一行远距断段。
        // 行距 40 > 行高 30：真实几何，防"中心距"判定回归（中心距 40 > 24 会误断段）
        let raw = vec![line("第一段甲", 0.0), line("第一段乙", 40.0), line("第二段", 220.0)];

        let by_line =
            derive_display(&raw, ProcKind::Normal, &OcrOpts { layout: Layout::ByLine, ignore: vec![] });
        assert_eq!(by_line.len(), 3, "按行不合并");

        let merged = derive_display(
            &raw,
            ProcKind::Normal,
            &OcrOpts { layout: Layout::MergePara, ignore: vec![] },
        );
        assert_eq!(merged.len(), 1, "MergePara 全并一段");
        assert_eq!(merged[0].text, "第一段甲第一段乙第二段");

        let natural = derive_display(
            &raw,
            ProcKind::Normal,
            &OcrOpts { layout: Layout::NaturalPara, ignore: vec![] },
        );
        assert_eq!(natural.len(), 2, "自然分段：远距行断段");
        assert_eq!(natural[0].text, "第一段甲第一段乙");
        assert_eq!(natural[1].text, "第二段");

        // 公式（结构化）管线豁免排版：同一原料换排版不改变分组
        let st = derive_display(
            &raw,
            ProcKind::Structured,
            &OcrOpts { layout: Layout::MergePara, ignore: vec![] },
        );
        assert_eq!(st.len(), 3, "公式管线不合并行");

        // 定位框恢复：按行 = 本行 bbox；分段 = 段内并集（y 0..30 ∪ 40..70）
        assert_eq!(by_line[0].box_px, Some([0.0, 0.0, 100.0, 30.0]));
        assert_eq!(by_line[2].box_px, Some([0.0, 220.0, 100.0, 250.0]));
        assert_eq!(natural[0].box_px, Some([0.0, 0.0, 100.0, 70.0]));
        assert_eq!(natural[1].box_px, Some([0.0, 220.0, 100.0, 250.0]));
        // MergePara 全并一段 = 三行总并集
        assert_eq!(merged[0].box_px, Some([0.0, 0.0, 100.0, 250.0]));

        // 全零占位框（插件路径）→ None：无坐标可定位
        let zero = rumi_ocr::OcrLine {
            text: "插件行".into(),
            score: 1.0,
            r#box: [[0.0; 2]; 4],
        };
        let out = derive_display(&[zero], ProcKind::Normal, &OcrOpts { layout: Layout::ByLine, ignore: vec![] });
        assert_eq!(out[0].box_px, None);
    }

    /// 忽略规则命中全部行 → 空结果不 panic
    #[test]
    fn apply_postprocess_all_ignored_yields_empty() {
        let line = |t: &str| rumi_ocr::OcrLine {
            text: t.into(),
            score: 1.0,
            r#box: [[0.0; 2]; 4],
        };
        let opts = OcrOpts { layout: Layout::ByLine, ignore: vec![".+".into()] };
        assert!(apply_postprocess(vec![line("a"), line("b")], &opts).is_empty());
    }

    /// J2：结构化（公式）输出后处理——**不合并行**（LaTeX 行合并无意义），
    /// 忽略规则仍逐行生效，分数取本行原值。
    #[test]
    fn structured_postprocess_keeps_lines_and_applies_ignore() {
        // 两个"相邻"行：走 MergePara 会被并成一段，这里必须保持两行
        let line = |t: &str, s: f32, y: f32| rumi_ocr::OcrLine {
            text: t.into(),
            score: s,
            r#box: [[0.0, y], [100.0, y], [100.0, y + 30.0], [0.0, y + 30.0]],
        };
        let lines = vec![
            line("x + y = 1", 0.88, 0.0),
            line("E = m c^2", 0.92, 20.0),
            line("内部资料水印", 0.99, 60.0),
        ];
        // 排版选项即使是 MergePara 也不参与（公式模式豁免）；
        // "内部资料水印"命中忽略规则被剔除
        let opts = OcrOpts { layout: Layout::MergePara, ignore: vec!["内部资料".into()] };
        let out = apply_structured_postprocess(lines, &opts);
        assert_eq!(out.len(), 2, "公式行不合并（对照：同输入走 MergePara 会并成一段）");
        assert_eq!(out[0].text, "x + y = 1");
        assert_eq!(out[1].text, "E = m c^2");
        assert_eq!(out[0].score, "0.88", "分数为本行原值");
    }

    /// 历史相对时间（P1 #7 列表文案）
    #[test]
    fn history_age_buckets() {
        let now = 10_000_000u64;
        assert_eq!(history_age("zh_CN", now - 5, now), "刚刚");
        assert_eq!(history_age("zh_CN", now - 120, now), "2 分钟前");
        assert_eq!(history_age("en_US", now - 7200, now), "2 h ago");
        assert_eq!(history_age("zh_CN", now - 3 * 86400, now), "3 天前");
    }

    /// 独立临时目录（进程 id + 原子计数，并行测试安全）
    fn test_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{tag}-{}-{n}", std::process::id()))
    }

    /// 缩略图尺寸钳制：大图最长边钳到 THUMB_MAX_EDGE、小图不放大
    #[test]
    fn thumbnail_clamps_long_edge_without_upscale() {
        let make_png = |w: u32, h: u32| {
            let img = image::RgbaImage::from_fn(w, h, |x, y| {
                image::Rgba([((x % 251) as u8), ((y % 241) as u8), 7, 255])
            });
            let mut png = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            png
        };
        // 横向大图 1000x500 → 最长边 480，等比 480x240
        let (bytes, w, h) = thumb_png_of(&make_png(1000, 500)).expect("大图缩略图");
        assert_eq!((w, h), (THUMB_MAX_EDGE, 240));
        use image::GenericImageView;
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.dimensions(), (THUMB_MAX_EDGE, 240));
        // 竖向大图 400x1200 → 160x480
        let (_, w2, h2) = thumb_png_of(&make_png(400, 1200)).expect("竖图缩略图");
        assert_eq!((w2, h2), (160, THUMB_MAX_EDGE));
        // 小图 300x200 不放大
        let (_, w3, h3) = thumb_png_of(&make_png(300, 200)).expect("小图缩略图");
        assert_eq!((w3, h3), (300, 200));
        // 坏字节 → None（调用方按"无图"降级）
        assert!(thumb_png_of(b"not a png").is_none());
    }

    /// 历史缓存目录清理：超上限删最旧（按文件名时间戳序），无法解析文件名的优先删
    #[test]
    fn prune_history_dir_keeps_newest_within_cap() {
        let dir = test_dir("rumi-hist-prune");
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str| std::fs::write(dir.join(name), b"png").unwrap();
        // 乱序写入：文件名时间戳决定新旧，而非创建顺序
        write("2000000100_0.png");
        write("2000000101_0.png");
        write("2000000102_0.png");
        write("2000000103_0.png");
        write("garbage.png"); // 无法解析 → 最旧优先删
        // 5 个文件，cap=3 → 删 2 个：garbage + 最旧时间戳
        let removed = prune_history_dir(&dir, 3);
        assert_eq!(removed, 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "2000000101_0.png".to_string(),
                "2000000102_0.png".to_string(),
                "2000000103_0.png".to_string(),
            ]
        );
        // 未超上限：零删除
        assert_eq!(prune_history_dir(&dir, 10), 0);
        // 目录不存在：零删除不报错
        assert_eq!(prune_history_dir(&test_dir("rumi-hist-absent"), 5), 0);
        // save→parse 往返：保存的路径可被 parse_hist_name 解析
        let saved = save_history_png(&dir, 2000000104, 7, b"png").unwrap();
        assert_eq!(saved, dir.join("2000000104_7.png"));
        assert_eq!(parse_hist_name(&saved), Some((2000000104, 7)));
        // 删除（幂等）
        remove_history_png(&saved);
        remove_history_png(&saved); // 文件已不在不算错误
        assert!(!saved.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 忽略区域测试辅助：整帧同色帧 + 读单像素
    fn solid_frame(w: u32, h: u32, c: [u8; 4]) -> Frame {
        let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
        for _ in 0..(w as usize) * (h as usize) {
            rgba.extend_from_slice(&c);
        }
        Frame { rgba, width: w, height: h }
    }

    fn px_at(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * frame.width as usize + x as usize) * 4;
        [
            frame.rgba[i],
            frame.rgba[i + 1],
            frame.rgba[i + 2],
            frame.rgba[i + 3],
        ]
    }

    /// 归一化 → 像素换算：100x50 图、[0.1,0.2,0.3,0.4] → 半开矩形
    /// [10,30)×[10,20) 置白，边界外像素原样
    #[test]
    fn apply_ignore_regions_maps_normalized_rect_to_pixels() {
        let mut f = solid_frame(100, 50, [7, 8, 9, 255]);
        apply_ignore_regions(&mut f, &[[0.1, 0.2, 0.3, 0.4]]);
        assert_eq!(px_at(&f, 10, 10), [255, 255, 255, 255]);
        assert_eq!(px_at(&f, 29, 19), [255, 255, 255, 255]);
        assert_eq!(px_at(&f, 9, 10), [7, 8, 9, 255]);
        assert_eq!(px_at(&f, 30, 10), [7, 8, 9, 255]);
        assert_eq!(px_at(&f, 10, 9), [7, 8, 9, 255]);
        assert_eq!(px_at(&f, 10, 20), [7, 8, 9, 255]);
    }

    /// 越界钳制 + 倒序坐标交换：出帧坐标钳到帧界；完全帧外为空矩形不改动不 panic
    #[test]
    fn apply_ignore_regions_clamps_out_of_bounds() {
        let base = [3, 4, 5, 255];
        let mut f = solid_frame(100, 50, base);
        // x0=-5 → 0，x1=2.0 → 整宽；倒序由交换兜底
        apply_ignore_regions(&mut f, &[[-5.0, 0.5, 2.0, 0.6]]);
        assert_eq!(px_at(&f, 0, 25), [255, 255, 255, 255]);
        assert_eq!(px_at(&f, 99, 29), [255, 255, 255, 255]);
        assert_eq!(px_at(&f, 0, 24), base);
        assert_eq!(px_at(&f, 0, 30), base);
        // 全部 >1 → 钳到同一点，空矩形
        apply_ignore_regions(&mut f, &[[1.1, 1.1, 1.2, 1.2]]);
        assert_eq!(px_at(&f, 99, 49), base);
        // 全图框叠加：其余像素也变白
        apply_ignore_regions(&mut f, &[[0.0, 0.0, 1.0, 1.0]]);
        assert_eq!(px_at(&f, 0, 0), [255, 255, 255, 255]);
        assert_eq!(px_at(&f, 99, 49), [255, 255, 255, 255]);
    }

    /// 空列表不动帧
    #[test]
    fn apply_ignore_regions_empty_list_leaves_frame_untouched() {
        let mut f = solid_frame(10, 10, [1, 2, 3, 4]);
        apply_ignore_regions(&mut f, &[]);
        assert_eq!(px_at(&f, 0, 0), [1, 2, 3, 4]);
        assert_eq!(px_at(&f, 9, 9), [1, 2, 3, 4]);
    }

    // ── M 波：内置冻结帧选择器 ─────────────────────────────────────────────

    /// 冻结帧选区裁剪（纯函数）：显示坐标 → 原图像素，比例映射、边界钳制、
    /// 倒序拖选归一、过小选区拒绝四条契约。
    #[test]
    fn crop_frame_region_maps_display_rect_to_pixels() {
        // 100x50 帧以 200x100 显示（放大 2x）：显示矩形 (20,10)-(60,50)
        // → 原图 (10,5)-(30,25)，尺寸 20x20
        let f = solid_frame(100, 50, [9, 9, 9, 255]);
        let out = crop_frame_region(&f, (200.0, 100.0), (20.0, 10.0, 60.0, 50.0))
            .expect("正常选区应裁出子帧");
        assert_eq!((out.width, out.height), (20, 20));
        // 子帧左上像素保留原图 (10,5) 的颜色
        assert_eq!(px_at(&out, 0, 0), px_at(&f, 10, 5));
        assert_eq!(px_at(&out, 19, 19), px_at(&f, 29, 24));
    }

    /// 倒序拖选（右下→左上）与倒序拖选归一 + 帧界钳制。
    #[test]
    fn crop_frame_region_normalizes_reversed_drag_and_clamps() {
        let f = solid_frame(100, 50, [5, 6, 7, 255]);
        // 倒序等价正序
        let a = crop_frame_region(&f, (200.0, 100.0), (20.0, 10.0, 60.0, 50.0)).unwrap();
        let b = crop_frame_region(&f, (200.0, 100.0), (60.0, 50.0, 20.0, 10.0)).unwrap();
        assert_eq!((a.width, a.height), (b.width, b.height));
        assert_eq!(a.rgba, b.rgba);
        // 超出显示区的拖选钳到帧界（不 panic、不越界）
        let c = crop_frame_region(&f, (200.0, 100.0), (-30.0, -30.0, 400.0, 400.0))
            .expect("越界拖选钳到整帧");
        assert_eq!((c.width, c.height), (100, 50));
    }

    /// 过小选区（原图 <4px）与显示尺寸未知 → None（调用方按取消处理）。
    #[test]
    fn crop_frame_region_rejects_tiny_or_unknown_selection() {
        let f = solid_frame(100, 50, [1, 1, 1, 255]);
        // 2px 显示选区 ×2 比例 = 4px 恰好不拒；1px 显示 = 2px 原图 → 拒
        assert!(crop_frame_region(&f, (200.0, 100.0), (0.0, 0.0, 1.0, 1.0)).is_none());
        // 显示尺寸未知（0）
        assert!(crop_frame_region(&f, (0.0, 0.0), (0.0, 0.0, 50.0, 50.0)).is_none());
    }

    /// 冻结帧槽：store → take（取走即空）→ clear 幂等。
    #[test]
    fn picker_frame_slot_stores_takes_and_clears() {
        picker_frame_clear();
        assert!(picker_frame_take().is_none(), "空槽取出应 None");
        picker_frame_store(solid_frame(4, 4, [1, 2, 3, 255]));
        let got = picker_frame_take().expect("存入后应可取走");
        assert_eq!((got.width, got.height), (4, 4));
        assert!(picker_frame_take().is_none(), "take 语义：取走即空");
        picker_frame_store(solid_frame(2, 2, [0, 0, 0, 255]));
        picker_frame_clear();
        assert!(picker_frame_take().is_none(), "clear 后应为空");
    }
}
