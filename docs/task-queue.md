# Rumi 任务队列

> 排队中的波次；每波完成后按序派发（sonnet 代理 + 主管设计/巡场/验收，流程照旧）。

## M 波：xcap 跨平台截图后端 + 冻结帧选择器（排队中，L 波验收后立即派）

依据：2026-10-27 讨论（用户提议 + 主管评估采纳）。xcap 0.9.8（Apache-2.0，活跃）承担
X11/Windows/macOS 像素层与窗口枚举；我们保留 Wayland 三后端（portal/Spectacle/grim）。
xcap 输出 `image::RgbaImage` 与 K2 后全链路载体一致，零摩擦。

### 范围与要点（派发时展开为正式任务书）

1. **capture crate：`Capturer::Xcap` 变体**（cfg 门控依赖，非目标平台不拉）
   - probe 路由：`XDG_SESSION_TYPE=wayland` → 现有三后端；x11/win/macos → Xcap
   - 全屏 = `Monitor::all()` + `capture_image()`；窗口 = `Window::all()`（焦点判定查 API，
     无则提供 `list_windows` 供 UI 选择器，体验更优）
   - **Monitor 模型**：Caps 加能力位；`list_monitors()`（Wayland 下尽力/不可用如实上报）
   - xcap 类型不外泄（隔离 0.x API 变动风险）
2. **冻结帧选择器**（ui/，全平台通用组件）：
   非交互全屏帧 → 自家全屏窗口显示 → 拖选（虚线框/尺寸提示/ESC 取消/可选放大镜）
   → 裁剪为 Frame。`region_native=false` 的后端自动走它；Wayland 上作为设置项备选
3. **验证**：X11 路径用 Xephyr 嵌套 X 实测；win/mac 编译验证（真机验收延后，记录遗留）
4. **不碰**：L 波产物（TitleBar/主题）细节；服务与插件系统

### 预置风险记录

- Windows DPI 边缘坑（xcap 历史 issue）——真机阶段验收项
- macOS ScreenCaptureKit 权限引导 UX 归我们
- Wayland 无窗口枚举（安全模型），"选择窗口"选择器在 Wayland 缺失，Caps 如实上报
- 远期红利：xcap 三平台录屏 ✅ + KDE Spectacle Record* D-Bus——录屏功能低成本可期
- 可选社区动作：把我们的 Wayland 层 upstream PR 给 xcap

## 追加（2026-10-27，Capter 调研结论）

- 【L 波尾巴】ui/Cargo.toml：`arboard = { version = "3", features = ["wayland-data-control", "wl-clipboard-rs"] }`
  （Wayland 原生剪贴板协议，替代 XWayland 桥接）
- 【入 M 波任务书】M 波追加两点：
  1. 冻结帧选择器参考 Capter 的 src/capture/ 模块（canvas/crop/draw 逻辑同构，iced→Dioxus 移植思路）
  2. xcap 选型获生产验证（Capter 底层即 xcap 0.7）
- 【新条目 N 波（选择器落地后）】标注编辑器：矩形/圆/线/箭头/自由笔/荧光笔/文字，
  挂在冻结帧选择器之上（蓝本同 Capter draw.rs）；OCR 与标注共存（标注层不参与识别）
- 【小项】主题第三档"跟随系统"（dark-light crate 检测，Capter 同款用法）

## 状态更新（2026-10-27 M 波验收）

- M 波 ✅ 完成验收：xcap 后端（Xephyr 实测 X11 路由+抓屏与 import 交叉验证）、冻结帧选择器
  （picker.rs，主窗口全屏方案，spawn_forever 排掉自卸载取消坑）、窗口选择列表（caps 驱动）、
  select option 深色修复+原生控件审计（color-scheme/accent-color）。135 测试全绿。
- 用户可实测：设置页"选区方式"切"内置"→ 点"区域截图(原生)"体验冻结帧拖选；
  深色模式打开"排版解析"下拉验证修复。
- **下一条：N 波（标注编辑器）**，蓝本 Capter src/capture/draw.rs，挂选择器之上，标注层不参与 OCR。
- 真机验收债：Windows/macOS（DPI、SCK 权限、多屏 Monitor[0] 判定）。

## 追加（2026-10-27 深夜更新）：Wayland 最大化"上游取证"——已解决，无需跟进

根因是自家 CSS（`#main` 在 body flex row 下 shrink-to-fit，详见 `docs/开发记录.md`
"最大化调查"节），`#main { width: 100% }` 一行修复，Wayland 门控已撤销。
tao/wry/WebKitGTK 无需提 issue。经验提炼见 `docs/开发经验.md`。

## 追加（2026-10-27 深夜）：Linux 托盘左键/双击唤出主窗口（换 ksni 后端）

上游 tray-icon 0.21.3 GTK 后端无图标点击事件（源码确认），双击唤窗仅 Win/mac 可用。
若要 Linux 左键唤出主窗口：引入 ksni（纯 Rust SNI 实现，有 activate 回调），
Linux 下绕开 dioxus 内置托盘自建 SNI 服务与菜单（双击可用两次 activate 间隔
<400ms 判定）；Win/mac 保持现路径。注意 O 波教训：菜单对象不可整体重建
（dbusmenu 断链），ksni 菜单刷新需原地更新。工作量：tray.rs 双后端改造 + 回归。
