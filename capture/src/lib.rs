//! rumi-capture：跨平台截图核心。
//!
//! 设计（见 ../PROPOSAL-umi-clone.md §1）：
//! - 像素获取与选区交互分离；选区优先用平台原生（KDE→Spectacle 选区 UI，
//!   GNOME→portal 交互 UI，wlroots→slurp），冻结帧仅兜底。
//! - Wayland 上无法绘制透视覆盖层，这是安全模型决定的，不是实现缺陷。

mod decode;

use std::sync::Mutex;

/// 进程级共享实例缓存（`Capturer::shared` 用）。
/// 锁内只有指针读写、绝无 await——std 锁不能跨 `.await` 持有；
/// D-Bus 建连/捕获等慢操作全部在锁外进行。
static SHARED: Mutex<Option<&'static Capturer>> = Mutex::new(None);

/// 一帧 RGBA 图像
#[derive(Debug, Clone)]
pub struct Frame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Frame {
    /// Frame → `image::RgbaImage`（克隆版）：调用方之后还要继续用 Frame 时用这个。
    ///
    /// K2 解耦点：ocr crate 统一以 `image::RgbaImage` 为图像载体，本转换让
    /// capture→ocr 的交接不引入 ocr 对 capture 的依赖（image 本就是本 crate 依赖）。
    /// 缓冲长度与宽高不符（不应发生，Frame 恒来自 PNG 解码）时回退为
    /// 同尺寸全透明黑图，绝不 panic。
    pub fn to_rgba_image(&self) -> image::RgbaImage {
        image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .unwrap_or_else(|| fallback_rgba_image(self.width, self.height))
    }

    /// Frame → `image::RgbaImage`（move 版，零拷贝）：调用方不再使用 Frame 时用这个，
    /// 像素缓冲直接移交，无复制。缓冲不符的兜底同 [`Frame::to_rgba_image`]。
    pub fn into_rgba_image(self) -> image::RgbaImage {
        image::RgbaImage::from_raw(self.width, self.height, self.rgba)
            .unwrap_or_else(|| fallback_rgba_image(self.width, self.height))
    }

    /// `image::RgbaImage` → Frame（move 版，零拷贝）：M 波 Xcap 后端的入站转换——
    /// xcap 0.9 的 `capture_image()` 返回的就是 `image::RgbaImage`，与 K2 后全链路
    /// 载体一致，`into_raw` 直接移交缓冲，无复制。
    pub fn from_rgba_image(img: image::RgbaImage) -> Frame {
        let (width, height) = img.dimensions();
        Frame { rgba: img.into_raw(), width, height }
    }
}

/// 同尺寸全透明黑图（Frame 缓冲长度异常时的防御性回退，见 [`Frame::to_rgba_image`]）。
fn fallback_rgba_image(width: u32, height: u32) -> image::RgbaImage {
    image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 0]))
}

/// 后端能力描述
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// 无交互全屏帧
    pub fullscreen: bool,
    /// 用户原生选区（compositor/辅助程序提供的选区 UI）
    pub region_native: bool,
    /// 活动窗口捕获
    pub active_window: bool,
    /// 内置冻结帧选择器接管条件（M 波）：region_native=false 时为 true，
    /// UI 据此在"区域截图"上自动走自家全屏冻结帧拖选，而不是原生选区。
    pub builtin_picker_needs: bool,
    /// 窗口枚举可用（M 波 Xcap 平台）：UI"活动窗口"按钮据此弹"选择窗口"
    /// 列表面（list_windows → window_by_id）；false 时按钮隐藏。
    /// Wayland 安全模型不提供枚举，三个 Wayland 后端恒为 false。
    pub window_list: bool,
}

/// 显示器信息（M 波 `list_monitors` 出参；xcap 类型不外泄，映射成本结构）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub w: u32,
    pub h: u32,
}

/// 可捕获窗口信息（M 波 `list_windows` 出参；只保留选择列表需要的字段）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// 当前环境没有可用后端
    NoBackend(String),
    /// 后端拒绝了请求（portal 授权被拒、用户取消选区等）
    Denied(String),
    /// 后端调用失败
    Backend(String),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoBackend(m) => write!(f, "无可用截图后端: {m}"),
            Error::Denied(m) => write!(f, "请求被拒绝或取消: {m}"),
            Error::Backend(m) => write!(f, "后端错误: {m}"),
            Error::Io(e) => write!(f, "IO 错误: {e}"),
        }
    }
}
impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Linux 截图后端。Windows/macOS 后端在 M4 加入后改为 trait 泛化。
pub enum Capturer {
    /// KDE：org.kde.Spectacle 专属 D-Bus（区域/窗口/全屏，2026-10 于 Plasma 6.8 实测）
    KdeSpectacle { conn: zbus::Connection },
    /// 通用 Wayland：xdg-desktop-portal Screenshot（全屏；GNOME 亦可交互选区）
    Portal { conn: zbus::Connection },
    /// wlroots 系（Sway/Hyprland/Niri）：grim + slurp（需 feature "grim"）
    GrimSlurp,
    /// M 波：xcap 像素层——Linux X11（xcb）与 Windows/macOS 的唯一选择。
    /// 无原生选区交互（`region_native=false`，冻结帧选择器接手）；窗口枚举
    /// （list_windows/window_by_id）是 Win/mac/X11 的红利。xcap 类型不外泄。
    Xcap,
}

impl std::fmt::Debug for Capturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

impl Capturer {
    /// 探测当前会话最优后端。
    ///
    /// 优先级：Windows/macOS → Xcap 唯一选择；Linux 上 KDE 会话（`KDE_FULL_SESSION`
    /// 或 Spectacle D-Bus 可达）> Wayland portal > 其余（X11 / 未知会话 /
    /// Wayland 探测失败的兜底）→ Xcap。xcap 在 Linux 内部自行区分 X11（xcb）与
    /// Wayland（libwayshot，仅 wlroots 系）——后者由 Wayland 三后端优先覆盖，
    /// 这里只作最后兜底。
    pub async fn probe() -> Result<Capturer> {
        // Windows/macOS：xcap 是唯一像素层（真机验收延后，编译验证靠 cfg）
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        return Ok(Capturer::Xcap);

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let session_type =
                std::env::var("XDG_SESSION_TYPE").unwrap_or_default().to_lowercase();
            let kde =
                std::env::var("KDE_FULL_SESSION").map(|v| v == "true").unwrap_or(false);

            if session_type == "wayland" || kde {
                if let Ok(conn) = session_conn().await {
                    if kde {
                        // Spectacle 服务可 D-Bus 激活（org.kde.Spectacle.service）
                        return Ok(Capturer::KdeSpectacle { conn });
                    }
                    return Ok(Capturer::Portal { conn });
                }
                // 会话总线不可达：落到 Xcap 兜底（捕获时如实报错）
            }
            // x11 / 未知会话 / Wayland 兜底 → Xcap（xcap 内部再辨 X11/Wayland）
            Ok(Capturer::Xcap)
        }
    }

    /// 进程级共享实例：首次调用探测并缓存（Box::leak 或等价 &'static 方案）；
    /// 后续调用直接复用，不再新建 D-Bus 连接。
    /// KDE 后端调用失败（Backend/Denied 且非用户取消语义）时：自动切换 Portal 重建一次并重试当前操作；
    /// Portal 也失败才向上报错。
    ///
    /// 实现说明：
    /// - 缓存于 [`SHARED`]（`std::sync::Mutex`，capture 无 tokio 依赖）；锁内只有
    ///   指针读写，探测（D-Bus 建连，await 点）在锁外进行，绝不跨 `.await` 持锁。
    /// - 降级只发生在**共享实例**上（`portal_fallback_eligible` 校验指针同一）：
    ///   直接 `probe()` 得到的实例保持原语义，不自动降级。降级判定见
    ///   [`should_fallback_to_portal`]（保守：超时/用户取消类不降级）。
    pub async fn shared() -> Result<&'static Capturer> {
        // 快路径：短临界区读缓存（无 await，阻塞上限为他人临界区的微秒级耗时）
        if let Ok(g) = SHARED.lock() {
            if let Some(c) = *g {
                return Ok(c);
            }
        }
        // 探测在锁外：D-Bus 建连是 await 点
        let fresh = Capturer::probe().await?;
        let mut g = SHARED.lock().map_err(poisoned)?;
        Ok(match *g {
            // 并发下另一任务已先初始化：采纳先到者，自己这份多余连接正常析构
            Some(c) => c,
            None => {
                // Box::leak：进程级单例，连接量级 1 个，随进程存活可接受
                let leaked: &'static Capturer = Box::leak(Box::new(fresh));
                *g = Some(leaked);
                leaked
            }
        })
    }

    /// 降级重建：把共享实例替换为 Portal 后端（锁外新建 session 连接）并返回。
    /// 并发下若槽内已非 KDE（别的任务抢先降级，或原本就不是 KDE），直接采纳现值。
    async fn shared_fallback_portal() -> Result<&'static Capturer> {
        {
            let g = SHARED.lock().map_err(poisoned)?;
            if let Some(c) = *g {
                if !matches!(c, Capturer::KdeSpectacle { .. }) {
                    return Ok(c);
                }
            }
        }
        let conn = session_conn().await?;
        let mut g = SHARED.lock().map_err(poisoned)?;
        Ok(match *g {
            // 并发抢先：降级已完成
            Some(c) if !matches!(c, Capturer::KdeSpectacle { .. }) => c,
            _ => {
                let portal: &'static Capturer = Box::leak(Box::new(Capturer::Portal { conn }));
                *g = Some(portal);
                portal
            }
        })
    }

    /// self 是否就是当前共享实例（指针同一性；短临界区读，无 await）。
    fn is_shared_instance(&self) -> bool {
        match SHARED.lock() {
            Ok(g) => matches!(*g, Some(c) if std::ptr::eq(c, self)),
            Err(_) => false,
        }
    }

    /// 允许走 KDE→Portal 降级重试：仅当 self 是共享 KDE 实例，且错误判定可降级
    /// （[`should_fallback_to_portal`]）。
    fn portal_fallback_eligible(&self, e: &Error) -> bool {
        matches!(self, Capturer::KdeSpectacle { .. })
            && self.is_shared_instance()
            && should_fallback_to_portal(e)
    }

    pub fn id(&self) -> &'static str {
        match self {
            Capturer::KdeSpectacle { .. } => "kde-spectacle",
            Capturer::Portal { .. } => "wayland-portal",
            Capturer::GrimSlurp => "grim-slurp",
            Capturer::Xcap => "xcap",
        }
    }

    pub fn caps(&self) -> Caps {
        match self {
            // Spectacle D-Bus: FullScreen / ActiveWindow / RectangularRegion 均可用
            Capturer::KdeSpectacle { .. } => Caps {
                fullscreen: true,
                region_native: true,
                active_window: true,
                builtin_picker_needs: false,
                window_list: false,
            },
            // portal v2（KDE 6.8 实测）：全屏 OK；区域/窗口依赖各发行版后端实现
            Capturer::Portal { .. } => Caps {
                fullscreen: true,
                region_native: true, // 交互 UI；GNOME 上可选区域
                active_window: false,
                builtin_picker_needs: false,
                window_list: false,
            },
            Capturer::GrimSlurp => Caps {
                fullscreen: true,
                region_native: true,
                active_window: false,
                builtin_picker_needs: false,
                window_list: false,
            },
            // xcap：像素层只管"抓"，无原生选区 UI → 冻结帧选择器接手（M 波）。
            // 窗口枚举是 X11/Win/mac 红利；活动窗口的"焦点判定"（is_focused）在
            // X11 上依赖 EWMH、易碎，统一走 UI 的"选择窗口"列表面，故
            // active_window 如实报 false（active_window() 实现保留，见下）。
            Capturer::Xcap => Caps {
                fullscreen: true,
                region_native: false,
                active_window: false,
                builtin_picker_needs: true,
                window_list: true,
            },
        }
    }

    /// 无交互全屏帧。共享 KDE 实例失败（非取消/超时）时自动降级 Portal 重试一次。
    pub async fn fullscreen(&self) -> Result<Frame> {
        match self.fullscreen_once().await {
            Ok(f) => Ok(f),
            Err(e) if self.portal_fallback_eligible(&e) => {
                Self::shared_fallback_portal().await?.fullscreen_once().await
            }
            Err(e) => Err(e),
        }
    }

    async fn fullscreen_once(&self) -> Result<Frame> {
        match self {
            Capturer::KdeSpectacle { .. } => self.spectacle_call("FullScreen", &(0i32,)).await,
            Capturer::Portal { .. } => self.portal_shot(false).await,
            Capturer::GrimSlurp => Err(Error::Backend("grim 全屏待实现（本机无 grim）".into())),
            // xcap 无光标位置 API（0.9 未提供），按任务书取第一块显示器；
            // 主显示器判定（is_primary）留作真机多屏阶段的打磨项。
            Capturer::Xcap => {
                let monitor = xcap::Monitor::all()
                    .map_err(xcap_err)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| Error::Backend("xcap 未枚举到任何显示器".into()))?;
                Ok(Frame::from_rgba_image(
                    monitor.capture_image().map_err(xcap_err)?,
                ))
            }
        }
    }

    /// 用户原生选区。选区 UI 由平台提供（Spectacle 覆盖层 / portal 交互窗 / slurp）。
    /// 共享 KDE 实例失败（非取消/超时）时自动降级 Portal 重试一次。
    pub async fn region_native(&self) -> Result<Frame> {
        match self.region_once().await {
            Ok(f) => Ok(f),
            Err(e) if self.portal_fallback_eligible(&e) => {
                Self::shared_fallback_portal().await?.region_once().await
            }
            Err(e) => Err(e),
        }
    }

    async fn region_once(&self) -> Result<Frame> {
        match self {
            Capturer::KdeSpectacle { .. } => {
                self.spectacle_call("RectangularRegion", &(0i32,)).await
            }
            Capturer::Portal { .. } => self.portal_shot(true).await,
            Capturer::GrimSlurp => Err(Error::Backend("slurp 待实现（本机无 grim）".into())),
            // xcap 无选区 UI：调用方应按 caps.region_native=false 走内置冻结帧
            // 选择器；误调用时给出明确指向而不是静默错路。
            Capturer::Xcap => Err(Error::Backend(
                "xcap 后端不提供原生选区（caps.region_native=false），应由内置冻结帧选择器接手".into(),
            )),
        }
    }

    /// 活动窗口。共享 KDE 实例失败（非取消/超时）时自动降级——portal 不支持
    /// 窗口捕获，降级重试也会失败，故此处降级仅重建为 Portal 实例试探一次
    /// （与全屏/选区一致走统一管线，Portal 端报"不支持"即向上报错）。
    pub async fn active_window(&self) -> Result<Frame> {
        match self.active_once().await {
            Ok(f) => Ok(f),
            Err(e) if self.portal_fallback_eligible(&e) => {
                Self::shared_fallback_portal().await?.active_once().await
            }
            Err(e) => Err(e),
        }
    }

    async fn active_once(&self) -> Result<Frame> {
        match self {
            // 参数: includeWindowDecorations, includeMousePointer, includeWindowShadow
            Capturer::KdeSpectacle { .. } => {
                self.spectacle_call("ActiveWindow", &(0i32, 0, 0)).await
            }
            // xcap 焦点窗口：is_focused 优先（EWMH 缺失时退化为首个窗口）。
            // caps.active_window=false：常规 UI 走"选择窗口"列表面，此实现供
            // CLI `window` 子命令与后续真机打磨直接复用。
            Capturer::Xcap => {
                let win = xcap::Window::all()
                    .map_err(xcap_err)?
                    .into_iter()
                    .find(|w| w.is_focused().unwrap_or(false))
                    .or_else(|| {
                        xcap::Window::all()
                            .ok()
                            .and_then(|ws| ws.into_iter().next())
                    })
                    .ok_or_else(|| Error::Backend("xcap 未枚举到任何窗口".into()))?;
                Ok(Frame::from_rgba_image(win.capture_image().map_err(xcap_err)?))
            }
            _ => Err(Error::Backend("该后端不支持窗口捕获".into())),
        }
    }

    // ---- M 波：窗口/显示器枚举（Xcap 平台红利；Wayland 安全模型如实拒绝）----

    /// 列出显示器。Wayland 后端：安全模型不提供枚举 API，如实报 NoBackend。
    pub async fn list_monitors(&self) -> Result<Vec<MonitorInfo>> {
        match self {
            Capturer::Xcap => {
                let monitors = xcap::Monitor::all().map_err(xcap_err)?;
                monitors.iter().map(xcap_monitor_info).collect()
            }
            _ => Err(Error::NoBackend(
                "Wayland 安全模型不提供显示器枚举，该能力仅 X11/Windows/macOS（xcap）可用".into(),
            )),
        }
    }

    /// 列出可捕获窗口（已过滤最小化 / 无标题 / 零尺寸，见 [`should_list_window`]）。
    /// Wayland 后端：安全模型不提供窗口枚举，如实报 NoBackend。
    pub async fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        match self {
            Capturer::Xcap => {
                let windows = xcap::Window::all().map_err(xcap_err)?;
                let mut out = Vec::new();
                for w in &windows {
                    let title = w.title().unwrap_or_default();
                    let minimized = w.is_minimized().unwrap_or(false);
                    let (width, height) = (w.width().unwrap_or(0), w.height().unwrap_or(0));
                    if !should_list_window(&title, minimized, width, height) {
                        continue;
                    }
                    let Ok(id) = w.id() else { continue };
                    out.push(WindowInfo { id, title });
                }
                Ok(out)
            }
            _ => Err(Error::NoBackend(
                "Wayland 安全模型不提供窗口枚举，该能力仅 X11/Windows/macOS（xcap）可用".into(),
            )),
        }
    }

    /// 按 id 捕获窗口（`list_windows` 给出的 id）→ Frame。
    /// 窗口在枚举后关闭/最小化时报 Backend 错误，由调用方提示重选。
    pub async fn window_by_id(&self, id: u32) -> Result<Frame> {
        match self {
            Capturer::Xcap => {
                let win = xcap::Window::all()
                    .map_err(xcap_err)?
                    .into_iter()
                    .find(|w| w.id().map(|wid| wid == id).unwrap_or(false))
                    .ok_or_else(|| Error::Backend(format!("窗口 {id} 不存在或已关闭")))?;
                Ok(Frame::from_rgba_image(win.capture_image().map_err(xcap_err)?))
            }
            _ => Err(Error::NoBackend(
                "Wayland 安全模型不提供窗口枚举，无法按 id 捕获".into(),
            )),
        }
    }

    // ---- KDE Spectacle ----

    async fn spectacle_call<B>(&self, method: &str, body: &B) -> Result<Frame>
    where
        // zbus 5.19 call_method 签名要求 body: Serialize + DynamicType 且 Sized（元组）
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        use futures::{FutureExt, StreamExt};
        use zbus::Proxy;
        let Capturer::KdeSpectacle { conn } = self else {
            return Err(Error::Backend("非 KDE 后端".into()));
        };
        let proxy = Proxy::new(conn, "org.kde.Spectacle", "/", "org.kde.Spectacle")
            .await
            .map_err(|e| Error::Backend(format!("proxy: {e}")))?;
        // Plasma 6.8 实测（busctl introspect org.kde.Spectacle /）：全部方法带
        // no-reply 标志——入参 i/iii，返回体为空，结果经信号异步送达：
        //   ScreenshotTaken(s)  截图文件路径
        //   ScreenshotFailed(s) 失败原因
        // 必须先挂信号流再发起调用，否则会错过（调用立即返回）。
        // 信号等待不设超时：区域选区由用户拖多久都合法，等用户操作完。
        // SignalStream 非 Unpin：先 pin 再取 next（pin_mut! 会用 Pin<&mut> 重绑定）
        let taken = proxy
            .receive_signal("ScreenshotTaken")
            .await
            .map_err(|e| Error::Backend(format!("信号订阅: {e}")))?;
        let failed = proxy
            .receive_signal("ScreenshotFailed")
            .await
            .map_err(|e| Error::Backend(format!("信号订阅: {e}")))?;
        futures::pin_mut!(taken, failed);
        proxy
            .call_method(method, body)
            .await
            .map_err(|e| Error::Backend(format!("D-Bus {method}: {e}")))?;
        let file: String = match futures::future::select(
            taken.next().boxed_local(),
            failed.next().boxed_local(),
        )
        .await
        {
            futures::future::Either::Left((Some(msg), _)) => msg
                .body()
                .deserialize()
                .map_err(|e| Error::Backend(format!("ScreenshotTaken 解析: {e}")))?,
            futures::future::Either::Right((Some(msg), _)) => {
                let why: String = msg.body().deserialize().unwrap_or_default();
                return Err(Error::Denied(format!("Spectacle {method}: {why}")));
            }
            _ => return Err(Error::Backend("Spectacle 信号流中断".into())),
        };
        // 信号可能带 file:// URL（中文路径被百分号编码）
        let path = decode::percent_decode(file.strip_prefix("file://").unwrap_or(&file));
        decode::png_file_to_frame(&path)
    }

    // ---- xdg-desktop-portal ----

    async fn portal_shot(&self, interactive: bool) -> Result<Frame> {
        use ashpd::desktop::screenshot::Screenshot;
        let req = Screenshot::request()
            .interactive(interactive)
            .send()
            .await
            .map_err(|e| Error::Backend(format!("portal: {e}")))?;
        let resp = req
            .response()
            .map_err(|e| Error::Denied(with_portal_hint(format!("portal response: {e}"))))?;
        let uri = resp.uri().to_string();
        let path = decode::percent_decode(uri.strip_prefix("file://").unwrap_or(&uri));
        decode::png_file_to_frame(&path)
    }
}

// ---------------------------------------------------------------------------
// 共享实例与降级的辅助函数（纯逻辑，单元化测试降级判定）
// ---------------------------------------------------------------------------

/// 建立会话总线连接。
///
/// zbus 5.19 没有 Proxy::set_method_timeout；方法超时唯一的公开入口是
/// Connection 级 zbus::connection::Builder::method_timeout（builder.rs）。
/// Spectacle 的选区/授权对话框可能等用户很久，放宽到 300s。
/// （portal 的 ashpd 请求走自建连接，不受此超时影响。）
async fn session_conn() -> Result<zbus::Connection> {
    zbus::connection::Builder::session()
        .map_err(|e| Error::NoBackend(format!("session bus: {e}")))?
        .method_timeout(std::time::Duration::from_secs(300))
        .build()
        .await
        .map_err(|e| Error::NoBackend(format!("session bus: {e}")))
}

/// std 锁被毒化（持锁线程 panic；本 crate 临界区内不可能 panic，纯防御）。
fn poisoned<T>(_: std::sync::PoisonError<T>) -> Error {
    Error::Backend("共享实例锁被毒化".into())
}

/// 用户取消 / 超时语义的关键词探测。Spectacle 的 ScreenshotFailed 原因串是
/// 本地化文本，这里覆盖中英文常见表述；判定只服务于"要不要降级重试"，
/// 宁可漏判（多报一次错）也不误降（用户取消后弹出 portal 授权窗）。
fn is_cancel_or_timeout(msg: &str) -> bool {
    const MARKS: [&str; 8] = [
        "cancel", "abort", "dismiss", "取消", "中止", "放弃", "timed out", "timeout",
    ];
    let low = msg.to_lowercase();
    MARKS.iter().any(|m| low.contains(m))
}

/// 降级判定（保守）：KDE Spectacle 路径失败后是否值得切 Portal 重试。
/// - `Backend`（D-Bus 错误 / 服务不存在 / 信号中断 / 崩溃）：降级——超时样消息除外
/// - `Denied`（Spectacle 报失败）：非用户取消语义才降级
/// - `NoBackend` / `Io`：不降级（Io 说明截图已产出、只是落盘解码失败，Portal 重试无意义）
fn should_fallback_to_portal(e: &Error) -> bool {
    match e {
        Error::Backend(m) | Error::Denied(m) => !is_cancel_or_timeout(m),
        _ => false,
    }
}

/// portal 权限被拒时的用户引导：仅当错误文本含 portal 痕迹时附加一句
/// 处置建议（非交互全屏在 GNOME 等会话上最常见的失败形态就是授权被拒）。
fn with_portal_hint(msg: String) -> String {
    if msg.contains("portal") {
        format!("{msg}。可在系统设置→应用权限中允许截图，或改用 KDE Spectacle 后端")
    } else {
        msg
    }
}

// ---------------------------------------------------------------------------
// Xcap 后端辅助（xcap 类型不外泄：只在这里摸 xcap:: 类型）
// ---------------------------------------------------------------------------

/// xcap::XCapError → 我们统一的 Backend 错误
fn xcap_err(e: xcap::XCapError) -> Error {
    Error::Backend(format!("xcap: {e}"))
}

/// xcap Monitor → 对外 MonitorInfo（名称优先 friendly_name，退回 name）
fn xcap_monitor_info(m: &xcap::Monitor) -> Result<MonitorInfo> {
    Ok(MonitorInfo {
        id: m.id().map_err(xcap_err)?,
        name: m.friendly_name().or_else(|_| m.name()).unwrap_or_default(),
        w: m.width().map_err(xcap_err)?,
        h: m.height().map_err(xcap_err)?,
    })
}

/// 窗口列表过滤决策（纯函数可测）：丢弃最小化 / 无标题 / 零尺寸窗口——
/// 三者都无法作为有意义的捕获目标出现在选择列表里。
fn should_list_window(title: &str, minimized: bool, width: u32, height: u32) -> bool {
    !minimized && !title.is_empty() && width > 0 && height > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K2：Frame → image::RgbaImage 两条转换路径的像素/尺寸契约。
    #[test]
    fn rgba_image_conversions_preserve_pixels_and_dimensions() {
        let frame = Frame { rgba: vec![1, 2, 3, 4, 5, 6, 7, 8], width: 2, height: 1 };
        // 克隆版：图像内容一致，原 Frame 仍可用
        let img = frame.to_rgba_image();
        assert_eq!(img.dimensions(), (2, 1));
        assert_eq!(img.get_pixel(0, 0).0, [1, 2, 3, 4]);
        assert_eq!(img.get_pixel(1, 0).0, [5, 6, 7, 8]);
        assert_eq!(frame.rgba.len(), 8, "to_rgba_image 不得消耗 Frame");
        // move 版：内容一致（零拷贝语义，行为上只验像素）
        let img2 = frame.into_rgba_image();
        assert_eq!(img2.dimensions(), (2, 1));
        assert_eq!(img2.get_pixel(1, 0).0, [5, 6, 7, 8]);
    }

    /// 缓冲长度与宽高不符（防御路径）：回退为同尺寸透明黑图，不 panic。
    #[test]
    fn rgba_image_conversions_fall_back_on_bad_buffer() {
        let bad = Frame { rgba: vec![0; 3], width: 4, height: 4 };
        let img = bad.into_rgba_image();
        assert_eq!(img.dimensions(), (4, 4));
        assert!(img.pixels().all(|p| p.0 == [0, 0, 0, 0]));
    }

    #[test]
    fn shared_returns_same_pointer_across_calls() {
        // 共享实例的核心契约：两次调用返回同一 &'static 指针（不重复建连）。
        // 无 Wayland/X11 会话（headless 环境）探测会失败：跳过而非误报。
        match futures::executor::block_on(Capturer::shared()) {
            Ok(a) => {
                let b = futures::executor::block_on(Capturer::shared()).expect("第二次 shared()");
                assert!(std::ptr::eq(a, b), "shared() 两次调用必须返回同一实例");
            }
            Err(e) => eprintln!("SKIP：无可用截图会话（{e}），跳过共享实例指针测试"),
        }
    }

    #[test]
    fn cancel_and_timeout_markers_are_recognized() {
        // 用户取消（中英文）与超时样消息
        for m in [
            "User cancelled",
            "user cancel",
            "Aborted",
            "已取消",
            "操作被中止",
            "D-Bus FullScreen: Timed out",
            "connection timeout",
        ] {
            assert!(is_cancel_or_timeout(m), "{m} 应识别为取消/超时");
        }
        // 服务类失败文案不应误判
        for m in [
            "ServiceUnknown",
            "Spectacle FullScreen: 崩溃",
            "信号订阅: 服务不存在",
        ] {
            assert!(!is_cancel_or_timeout(m), "{m} 不是取消/超时");
        }
    }

    #[test]
    fn fallback_decision_is_conservative() {
        // Backend/Denied（D-Bus/服务类失败）→ 降级
        assert!(should_fallback_to_portal(&Error::Backend(
            "D-Bus FullScreen: ServiceUnknown".into()
        )));
        assert!(should_fallback_to_portal(&Error::Denied(
            "Spectacle FullScreen: 崩溃".into()
        )));
        // 用户取消 / 超时类：不降级，直接上报
        assert!(!should_fallback_to_portal(&Error::Denied(
            "Spectacle RectangularRegion: 用户取消".into()
        )));
        assert!(!should_fallback_to_portal(&Error::Backend(
            "D-Bus FullScreen: Timed out".into()
        )));
        // 其余错误类型一律不降级
        assert!(!should_fallback_to_portal(&Error::NoBackend("x".into())));
        assert!(!should_fallback_to_portal(&Error::Io(std::io::Error::other("png"))));
    }

    #[test]
    fn fallback_only_applies_to_shared_kde_instance() {
        // 非 KDE 后端永不降级（GrimSlurp 无需 D-Bus 连接，任意环境可构造）
        let g = Capturer::GrimSlurp;
        assert!(!g.portal_fallback_eligible(&Error::Backend("待实现".into())));
        // 非共享实例不自动降级：probe() 原语义由调用方自处（GrimSlurp 已覆盖该分支）
    }

    #[test]
    fn portal_hint_appended_only_with_portal_trace() {
        let hinted = with_portal_hint("portal response: access denied".into());
        assert!(hinted.contains("系统设置"), "应附加处置引导：{hinted}");
        assert!(hinted.contains("Spectacle"), "应指明 KDE Spectacle 后端：{hinted}");
        // 无 portal 痕迹的错误保持原样
        assert_eq!(with_portal_hint("别的错误".into()), "别的错误");
    }

    /// M 波：`image::RgbaImage` → Frame 零拷贝入站转换（xcap capture_image 的
    /// 返回类型即此载体），像素与尺寸逐项保持。
    #[test]
    fn frame_from_rgba_image_preserves_pixels_and_dimensions() {
        let img = image::RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([x as u8, (y * 10) as u8, 7, 255])
        });
        let frame = Frame::from_rgba_image(img);
        assert_eq!((frame.width, frame.height), (3, 2));
        assert_eq!(frame.rgba.len(), 3 * 2 * 4);
        // 像素抽查：from_fn 的坐标规则 (x, y*10, 7, 255)
        let at = |x: u32, y: u32| {
            let i = (y as usize * 3 + x as usize) * 4;
            [frame.rgba[i], frame.rgba[i + 1], frame.rgba[i + 2], frame.rgba[i + 3]]
        };
        assert_eq!(at(0, 0), [0, 0, 7, 255]);
        assert_eq!(at(2, 0), [2, 0, 7, 255]);
        assert_eq!(at(1, 1), [1, 10, 7, 255]);
    }

    /// M 波能力位：xcap 声明"无原生选区 + 内置选择器接管 + 窗口枚举可用"；
    /// 构造无需显示（GrimSlurp 同理），可离线断言。
    #[test]
    fn xcap_caps_need_builtin_picker_and_expose_window_list() {
        let c = Capturer::Xcap;
        assert_eq!(c.id(), "xcap");
        let caps = c.caps();
        assert!(caps.fullscreen);
        assert!(!caps.region_native, "xcap 无选区 UI，冻结帧选择器接手");
        assert!(!caps.active_window, "窗口捕获统一走 list_windows 选择器");
        assert!(caps.builtin_picker_needs, "region_native=false 时 UI 自动走内置选择器");
        assert!(caps.window_list, "X11/Win/mac 的窗口枚举红利");

        // 对照组：原生选区存在的后端不接管（UI 不会被误导）
        assert!(!Capturer::GrimSlurp.caps().builtin_picker_needs);
        assert!(!Capturer::GrimSlurp.caps().window_list);
    }

    /// M 波：窗口列表过滤——最小化 / 无标题 / 零尺寸都不能进选择列表。
    #[test]
    fn window_list_filters_minimized_untitled_and_zero_sized() {
        assert!(should_list_window("终端", false, 800, 600));
        assert!(should_list_window("Firefox - 100 tab", false, 1, 1));
        assert!(!should_list_window("后台窗口", true, 800, 600), "最小化不过滤会抓到黑帧");
        assert!(!should_list_window("", false, 800, 600), "无标题窗口不可选");
        assert!(!should_list_window("占位", false, 0, 600), "零宽不可捕获");
        assert!(!should_list_window("占位", false, 800, 0), "零高不可捕获");
    }

    /// M 波：Wayland 后端的枚举 API 如实拒绝（安全模型，不是未实现）。
    /// GrimSlurp 无需 D-Bus 连接，任意环境可构造。
    #[test]
    fn wayland_enum_apis_refuse_with_no_backend() {
        let g = Capturer::GrimSlurp;
        for r in [
            futures::executor::block_on(g.list_monitors()).err(),
            futures::executor::block_on(g.list_windows()).err(),
            futures::executor::block_on(g.window_by_id(1)).err(),
        ] {
            let e = r.expect("Wayland 枚举 API 应返回错误");
            let msg = e.to_string();
            assert!(matches!(e, Error::NoBackend(_)), "{msg}");
            assert!(msg.contains("安全模型"), "应说明是安全模型限制：{msg}");
        }
    }
}
