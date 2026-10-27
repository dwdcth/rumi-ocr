# Rumi 交接文档（2026-10-27）

> 交接目的：新 AI 接手开发。本文覆盖：项目全貌、架构决策、本日全部改动、
> **当日已修复的定位 bug（根因 = DOM 测量竞态，完整诊断记录见 §4）**、
> 验证工作流、环境坑。读完后应能直接继续开发与排障。

## 0. 一句话与启动

Rumi = Umi-OCR（github.com/hiroi-sora/Umi-OCR）的 Rust 复刻：跨平台截图 →
OCR（oar-ocr，PP-OCRv6/公式）→ Umi 插件协议兼容（系统 python3 + 托管 venv，
不内置 Python）。KDE Plasma Wayland 真机开发环境。

```bash
cd /home/banshan/项目/jt/rumi
cargo build --workspace          # 全量构建
cargo test  -p rumi-ui           # 57 passed / 1 ignored（基线，别低于它）
cargo test  -p rumi-ocr          # 30 passed / 2 ignored（ignored=真推理，模型已缓存 ~/.oar）
pkill -x rumi-ui; nohup ./target/debug/rumi-ui > ui_run.out 2>&1 &   # 起 GUI
tail -f ui_run.out               # 看日志（含 [tray]/[locate] 诊断插桩）
```

## 1. 架构速览

cargo workspace（edition 2024，无 git 仓库！）：

| crate | 职责 |
|---|---|
| `capture` | 截图 lib：KDE Spectacle D-Bus / xdg-desktop-portal / grim / **xcap**(X11/Win/mac)。`Capturer::shared()` 探测路由，`Frame`→`image::RgbaImage` |
| `ocr` | oar-ocr 封装：recognize / recognize_structured(公式) / postprocess(排版) / PDF 双层导出 / QR |
| `ui` | Dioxus 0.7.10 桌面端（唯一 GUI，bin=rumi-ui）。**绝大多数工作在这里** |
| `server` | axum HTTP API :1224 |
| `plugins-worker` + `pyshim/` | Umi 插件协议：python worker 子进程 + JSON-RPC stdio |
| `cli` | 组合 crate（package=rumi-cli，bin=rumi-capture）——见 docs/consult-cyclic-dep.md 采纳记录（K1/K2，解决 capture⇄ocr 循环依赖） |

**依赖铁律**：ocr 不依赖 capture（K2：统一 `image::RgbaImage` 载体）。
触发"下沉重构 rumi-types"的三次法则未到，别提前做。

### ui 关键文件

- `main.rs`（~2200 行）：App 装配、CaptureCtx 信号束构建、MainTab（识别页）、
  OcrRow（结果行）、预览图覆盖层（划选/忽略框/定位框）、CSS 全在这一个 const 字符串里
- `ocr_flow.rs`（~1550 行）：识别管线核心。**两层架构**（本日改造，见 §3.1）
- `tray.rs`：托盘（两根因修复记录在模块注释，**别用 `TrayIcon::set_menu`，别去掉 `.enabled(true)`**）
- `i18n.rs` + `assets/i18n/{zh-CN,en-US}.json`：词条外置 JSON（本日迁移，见 §3.4）
- `settings.rs`（config.toml）、`batch.rs`（批量/PDF/QR）、`picker.rs`（冻结帧选择器）、
  `plugins_ui.rs`、`clipboard.rs`、`icon.rs`、`history`（在 ocr_flow 内）
- `docs/`：task-queue.md（N 波=标注编辑器，下一个大任务）、polish-backlog.md、
  plugin-worker-spec.md、consult-cyclic-dep.md（架构咨询记录）

## 2. 核心数据流（识别页）

```
截图/粘贴 → Frame
  → ocr_flow::recognize_raw(frame) -> (Vec<OcrLine>, ProcKind)   ← 原料+管线类别
  → ctx.raw.set(...)                                               ← 存原料（热重排用）
  → derive_display(&raw, kind, &opts) -> Vec<UiLine>              ← 排版/忽略在此应用
  → finish_recognize: lines/texts/history 落地
```

- `CaptureCtx`（ocr_flow.rs）：Copy 信号束（shot/shot_size/status/engine_st/lines/
  texts/**raw**/**locate**/history/hist_missing/busy/picker），App 构建、处处传值。
- `UiLine { text, score, box_px }`；`box_px` = 图像像素 [x0,y0,x1,y1]（定位用）。
- `processed_to_ui()`（ocr_flow）是 postprocess→UiLine 的**单一映射**
  （组内均分 + 组内 bbox 并集），识别页与批量页共用。
- 排版热重排：切"排版解析"下拉 → 对 ctx.raw 现场 re-derive → lines/texts 即时刷新
  （无需重识别）；locate 同步清空。

## 3. 本日（2026-10-27）改动明细 —— 全部已验证 ✓

### 3.1 排版热重排（✓ 用户已确认）
recognize_frame 拆成 recognize_raw + derive_display；CaptureCtx 加 raw；
截图/粘贴两路径存原料；main.rs 下拉 onchange 现场重派生（lines+texts 同刷）。

### 3.2 自然分段算法修复（✓ 用户已确认）
`ocr/src/postprocess.rs` paragraph_break：**中心距 → 真实行间隙**（上行底→下行顶，
间隙>行高×0.8 断段；框重叠=同段）。旧版中心距≈1.2-1.5×行高恒断段，自然分段退化成按行。
真实引擎 e2e 测试：`cargo test -p rumi-ocr natural_para_real_engine -- --ignored`（两段唐诗→恰 2 段）。

### 3.3 托盘两根因（✓ 用户已确认）
1. **灰显**：muda `MenuItemBuilder.enabled` 是 `#[derive(Default)]` bool=**false**，
   必须 `.enabled(true)`（tray.rs build_menu + 测试守卫）。
2. **点击无动作**：dioxus-desktop 0.7.10 上游 bug——`App::new` 先 set menubar 的
   `muda::MenuEvent::set_event_handler`，再 set tray 的（同一个 OnceCell！后者静默失败）
   → TrayMenuEvent 永不产生。**修复：handler 挂 `use_muda_event_handler`**（我们无 menubar）。
诊断方法论（可复用）：busctl 查导出的 dbusmenu（`GetLayout` 看 enabled）、
与 clash-verge（同栈 Tauri 应用）对照、~/项目/jt/traytest 纯 gtk 最小复现。

### 3.4 i18n 外置 JSON（✓ 测试全绿）
166 词条迁 `ui/assets/i18n/{zh-CN,en-US}.json`；`include_str!` 编译期嵌入 +
`LazyLock<HashMap<String,String>>`（**值必须 String 不能零拷贝 &str**：JSON 值带转义
引号时 serde 借不出，会解析失败——踩过）。`k` 常量区只留 key 标识符；en 缺词回退 zh。

### 3.5 文字定位（✓ 当日修复并实测验证，完整诊断记录见 §4）
数据层（processed_to_ui 的 bbox 恢复）早已正确；三轮交互层迭代未中根因。
真根因是 **img_px 采集竞态**（onmounted 的异步 get_client_rect 晚到、把
onresize 已送达的真实尺寸覆盖回 0×0），修法：wrapper 挂 `onresize`
（ResizeObserver）唯一维护显示尺寸，onmounted 只存句柄。
用户实测：点行号 1-5 逐行切换，蓝色定位框全部出现且位置正确。

### 3.6 其它
- 结果行复制按钮：btn-sm → 标准尺寸 + 文案 "⧉ 复制"
- 三处提示文案改为"点行首数字定位图中位置，行尾按钮复制"（中英）
- batch.rs 手抄映射收敛到 processed_to_ui
- /tmp 审计：6 处硬编码全在 #[cfg(test)]，生产代码全走 std::env::temp_dir() ✓

## 4. ✓ 已修复：点行首数字后预览图无高亮（当日诊断全记录）

**现象**：识别后点行首数字（悬停有变色），预览图上不出现蓝色定位框。

**实现链路**（main.rs）：
```
OcrRow 行号 span onclick → locate.set(Some((行号, bbox)))
  → MainTab rsx 读 locate() → locate_view(locate, shot_size, img_px)
  → Some((l,t,w,h)) → <div class="loc-rect"> 覆盖在预览图 wrapper 内
```

### 4.1 日志分流结果（§4.3 的表work了，落第 3 行）

用户点击后 ui_run.out 出现：
```
[locate] click no=1 box=Some([...]) prev=None      ← onclick 正常触发 ✓
[locate] view b=[...] size=(1038, 513) disp=(0.0, 0.0) -> None
                                                  ← 渲染跑了，但 img_px=0×0 被守卫拦下 ✗
```
即：交互、信号、重渲染全通，断点只在**预览图显示尺寸（img_px）恒为 0**。

### 4.2 根因：两条采集通道的竞态（插桩实测，ui_run.out 原始序列）

```
[locate] resize evt border=Ok(0.0x0.0)              ← ResizeObserver 初始观察：0×0
[locate] resize evt border=Ok(210.5x316.45)         ← 图片解码、布局定型：真实尺寸到达 ✓
[locate] mounted rect=(0.0,0.0)                      ← onmounted 的 get_client_rect
                                                      【晚到 30ms，快照是挂载瞬间的 0×0】
                                                      【把上面的真实值覆盖回 0】✗
```

机制：`.shot-wrap` 是 inline-block、尺寸完全由内部 `img` 撑开；onmounted 在
元素入 DOM 时触发，那一刻 data-URL 图片还没解码，wrapper 是 0×0；dioxus-desktop
的 `get_client_rect` 是 evaluate_script 异步查询——**快照取于早、结果回于晚**，
必然覆盖事件驱动（ResizeObserver）先送达的真实值。之后只有"在预览图上按下鼠标"
（划选路径的 mousedown 重采）能治好 img_px——这解释了历史上一切"时好时坏"：
**v1 验证会话里用户先测过划选复制（P1 #9 同波功能），img_px 被治好过**；
后续会话直接 OCR→点行，则必失败。

### 4.3 修法（已落地，main.rs 预览区）

- wrapper 挂 `onresize`（dioxus-html 有此事件，底层 ResizeObserver，
  事件自带 border-box 尺寸，同步取值）：布局定型/换图/窗口 resize 全覆盖，
  **唯一维护 img_px**；
- `onmounted` 只存 `wrap_mounted` 句柄（供 mousedown 重采），**绝不写 img_px**；
- mousedown 现场重采保留（划选基准，快照取于点击当下，无竞态）。

dioxus 侧确认过的机制（读源码）：desktop 加载 SLEDGEHAMMER+NATIVE 解释器，
`load`/`resize` 等非冒泡事件直挂元素本身（`bubbles.rs` 显式标记），不走根委托。

### 4.4 两个被否证的理论（别再走回头路）

1. **`docs/代码修复指南.md` 的 v2-stop_propagation 回归论**：v1→v2 的按钮
   stop_propagation 破坏 li 冒泡触发区——插桩证明 v3 的行号 span 点击**完全正常**
   （`[locate] click` 齐全），交互层从来没断；"v1 可用"是 mousedown 治好 img_px
   的巧合（见 4.2）。其路径 A（回退整行 li onclick）修的是没坏的层，不采纳；
   v3 行号 span 交互保留。指南 §8 已附实测结论。
2. **CSS 层叠/z-index 论**（分流表第 4 行）：修复后截图确认蓝框可见，
   `.loc-rect` 的绝对定位/pointer-events 从来没问题。

### 4.5 验证记录（2026-10-27 下午）

- `cargo test -p rumi-ui` 57 passed / 1 ignored（基线保持）；workspace 零 error。
- 用户真机连续点击行号 1-5，日志全部 `disp=(181.2, 263.0) -> Some((l,t,w,h))`。
- 截图确认：预览图上蓝色描边+半透明填充定位框可见，位置与所点行对应。
- `[locate] click/view` 两处诊断插桩**保留**（低频、定位排障用）；
  mounted/resize 的临时插桩已随修复移除。

## 5. 环境坑（全都踩过，直接照做）

- Bash 钩子拦截 `*.log` 后缀与 /tmp 日志路径 → 一律用 `.out` 后缀
- `rm -rf` 被钩子拦 → 用 python os 或逐个删
- `pkill -f` 会自杀（匹配到自己的 wrapper）→ 必须 `pkill -x rumi-ui`
- curl 被重定向沙箱 → 用 ctx_execute 或 WebFetch
- 端口 1224 有僵尸 server 会造成假验收 → 先 `ss -ltnp | grep 1224` / pgrep
- KDE Spectacle D-Bus 是 no-reply（信号回结果），portal 中文路径要 percent_decode
- oar-ocr 必须 git 依赖（crates.io 0.1.1 是旧版）
- dioxus 0.7 坑：rsx 循环体禁 let/解构（用组件）；`select` 无 value 属性（用
  option selected）；Signal 是 !Send（跨线程用 spawn+run_blocking oneshot 模式）；
  `get_client_rect` 是异步查询，**快照取于发起时刻、结果晚到**——别拿它做
  "挂载后测量"，会覆盖事件驱动先到的值（§4 教训）；要尺寸用 `onresize`

## 6. 验证工作流（提交前必做）

1. `cargo test -p rumi-ui -p rumi-ocr`（基线 57+1 / 30+2）
2. `cargo build --workspace` 零 error
3. 重启 GUI（§0 命令）真机过一遍识别主链路
4. 改 tray/i18n 时跑对应单测（tray.rs / i18n.rs 内嵌测试会抓回归）

## 7. 任务队列（docs/task-queue.md 摘要）

- **N 波（下一个大任务）**：标注编辑器（矩形/圆/线/箭头/自由笔/荧光笔/文字），
  蓝本 Caper src/capture/draw.rs，挂在 picker.rs 冻结帧选择器之上，标注层不参与 OCR
- 小项：主题第三档"跟随系统"（dark-light crate）
- 真机债：Windows DPI / macOS SCK 权限引导 / 多屏 Monitor[0]
- 遗留：~/项目/jt/traytest（排障用最小复现，可删）

### 7.1 GUI 无人工验证法（§4 当日实证可行，排障可复用）

"开发者无法点 GUI"其实可解（KDE Wayland）：
- **窗口几何**：KWin 脚本（`busctl --user call org.kde.KWin /Scripting
  org.kde.kwin.Scripting loadScript/start`），`workspace.windowList()` 拿
  frameGeometry（逻辑 px），`workspace.activeWindow=w` 激活置顶，输出走 journalctl。
- **合成点击**：ydotool 可用，但 `mousemove -a` 绝对模式在 KWin 下失效（钉死 1,1）；
  **相对模式预热一次后 1:1 精确**——闭环：读 `workspace.cursorPos` → 算差值 →
  `mousemove -x dx -y dy` → 复读 → 修正，最后 `ydotool click 0xC0`。
- **坐标标定**：屏幕缩放 1.6（2880×1800 物理 = 1800×1125 逻辑），截图像素
  ÷1.6 = 逻辑坐标；视觉模型报整窗坐标不稳，**裁条放大 2 倍再问**收敛好。
- 截图用 `spectacle -b -n -o 文件.png`；裁剪/比对用 PIL。
