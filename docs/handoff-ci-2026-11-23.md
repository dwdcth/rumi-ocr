# rumi CI 交接文档（2026-11-23）

> 接手前必读：本文件描述 rumi-ocr 打包发布体系的现状、刚发生的事、**未完成的改动**和下一步。仓库 `dwdcth/rumi-ocr`（public），本地 `/home/banshan/项目/rumiocr/rumi`，分支 main。

## 1. 体系总览（已完成并验证的部分）

### 三个 workflow（全部手动触发，无自动消耗；均带 concurrency 自动取消旧轮）

| workflow | 内容 | 状态 |
|---|---|---|
| `ci.yml` | Linux 门禁：build rumi-ui/server + 全量测试 + protobuf link_guard | ✅ 稳定 4-5min（ccache 命中态） |
| `prebuilt-deps.yml` | **一次性 bootstrap**：5 主流平台经 cargo 完整 CMake 编 C++，捞 `ctranslate2/dnnl/cpu_features` 三静态库；`prebuilt-loong64` job 用 QEMU 容器纯 cmake 编龙芯 CT2。产物上传统一 `deps` release。**仅依赖版本变化时跑** | ✅ 6/6 平台归档已入库 |
| `release.yml` | 正式打包：6 平台。5 主流走动态 matrix（**预编译消费模式**：下载 deps 归档 → `CT2RS_PREBUILT_DIR` 直链，C++ 零编译）；`build-loong64` 独立 job（QEMU 容器 + 预装镜像 + 预编译 CT2）。tag `v*` 触发时 softprops 把全部资产挂进同一 release | 🔄 收官验证中（见 §3） |

### 手动打包用法

```bash
# 全 6 平台
gh workflow run release.yml -R dwdcth/rumi-ocr
# 只跑指定平台（loongarch64 是 build-loong64 的开关词；5 主流用全名）
gh workflow run release.yml -R dwdcth/rumi-ocr \
  -f targets='["x86_64-pc-windows-msvc","loongarch64"]'
# 龙芯本地出包（见 §5）
./scripts/loong-release.sh <release-tag> [label]
```

### 发布资产分布

| Release | 内容 | 状态 |
|---|---|---|
| `deps` | C++ 静态库归档 ×6（CT2+oneDNN+cpu_features；loong 仅 CT2）。win 归档已 llvm-strip（119.7→113.3MB，MSVC 修饰名占大头，压不动更多） | ✅ 齐 |
| `loong64-deps` | 龙芯 CT2 旧归档（已被 deps 取代，**退役待删**） | 历史遗留 |
| `ci-966db1f` | 成品包 ×5（**缺龙芯**，用户反馈的就是这批里的 linux 包有 bug，见 §4） | 待重出 |
| `v*`（未来） | tag 触发，6 平台资产汇入同一 release | 未打过 tag |

### 本地资产

- 镜像 `ghcr.io/dwdcth/rumi-loong-build:v1`（public，4.1GB）：trixie 新世界 + apt 全套（含 gbm/drm/x11/xcb/wayland dev）+ rustup + ORT 1.32 + **中科大 crates 镜像配置**（CI 上会被删，见 §6-2）
- 本地容器 `rumi-loong`（同源，带全套环境 + 龙芯 CT2 手动 cmake 产物 /tmp/ct2build）；binfmt `qemu-loongarch64` 重启后会丢，用 `docker run --privileged --rm tonistiigi/binfmt --install loong64` 重装
- 本地增量缓存：`target/loong-target|loong-cargo-registry|loong-cargo-git`、`target/loong-ct2-prebuilt/`（CT2 .a 本地副本）

## 2. 两个辅助脚本

- `scripts/loong-release.sh`：龙芯本地出包直传（容器构建 → 打包 → `gh release upload`，上传需 `LOONG_RELEASE_UPLOAD=1` 显式开启）。**已跑通**（产物 63.8MB，结构自检过）。**但注意：它还没同步 §4 的 libxdo/RPATH 修复**（待办 §5-2）。
- `scripts/check-linux-package.sh`：Linux 分发包自检（**新增，已三向测试**：正向 exit 0 / 删随包库 exit 1 点名 / 用户失败包 exit 1 复现其报错）。规则：每个 bin 的 NEEDED 要么随包 `lib/`、要么在宿主允许清单（清单来自 x64/arm64/loong 三平台产物 NEEDED 并集）；bin 必须带 `DT_RPATH=$ORIGIN/lib`；加载器实际解析一遍且包内库必须被命中。**尚未接入任何 workflow**（待办 §5-2）。

## 3. 进行中的事（截稿时）

- **run 45118056164**（release，targets=win+loong）跑到 ~1h20m：win **✅ 成功**（19min，预编译消费模式首绿，strip 后 .lib 链接正常）；loong 🔄 进行中——**预判会失败**，原因见 §4-3（那份 workflow 里 loong job 有两处未修的不一致）。它跑完即被下一轮取代，不用救。
- 收官验证轮待 dispatch（见 §5-1）。

## 4. 用户报的 bug 与根因（本节是本次交接的核心）

**现象**：`ci-966db1f` 的 Linux x64 包，Arch 上直接运行 `rumi-ui` 报 `libxdo.so.3: cannot open shared object file`。

**根因**（本地用该包实测定案）：
1. `libxdo.so.3` 由 dioxus-desktop 的 muda/tray-icon 链入（仅 rumi-ui）；CI 构建机 Ubuntu 有 `.so.3`，Arch 的 xdotool 新版只有 `.so.4` → **未随包**。
2. 四个 bin **都没有 `DT_RPATH`**：直接运行时 `rumi-server`/`rumi-capture` 用到系统的 `libonnxruntime.so.1`（Arch 是 1.29.0），符号版本要求 `VERS_1.22.1` 不满足直接拒绝；经 `run-*.sh`（LD_LIBRARY_PATH）才能用包内 1.22.1。**修法 = 链接时加 `--disable-new-dtags -rpath $ORIGIN/lib`**（DT_RPATH 先于 ld.so.cache；DT_RUNPATH 不够）。
3. 附带发现：检查"加载器缺库"时不能只看退出码（符号版本不匹配时 `--list` 退出码为 0 但打印 `version ... not found`），要看非正常行；`objdump -p` 在 binutils 2.47 不再打印 interpreter 行（用 `readelf -l`），且中文 locale 会本地化标签（脚本内 `export LC_ALL=C`）。

**修复已写到一半（工作区未提交！）**：
- `scripts/check-linux-package.sh`：完整可用（untracked）。
- `.github/workflows/release.yml`（modified）：已改 5 主流平台的三处——Linux RUSTFLAGS 加 rpath、Package(unix) 随带 libxdo（`ldconfig -p` 找 `.so.N` 取实体）、打包前跑自检。**未改**：loong job 的三处 + 本地脚本 + README（待办 §5-2，改动内容见该文件 diff 与下述）。

## 5. 下一步（按序，具体操作）

1. **dispatch 收官验证**：`gh workflow run release.yml -R dwdcth/rumi-ocr`（默认 all）。concurrency 会自动取消在跑的旧轮。
2. **完成剩余补丁并提交**（一次 commit 即可）：
   - `release.yml` loong job（`Build in QEMU loong64 container` 步骤的 heredoc 内）：
     a. `export CT2RS_PREBUILT_DIR=$PWD/ct2-prebuilt` → **`/opt/ct2-prebuilt`**（第 ~492 行已解压到 /opt，第 506 行却指向 $PWD/ct2-prebuilt——不一致，ort 倒无关但 ct2rs 会 assert 失败）；
     b. `cp -P ... libonnxruntime.so.1 "/out/${stage}/lib/"` → **`cp -L ...libonnxruntime.so` + `ln -sf libonnxruntime.so .so.1`**（镜像里没有 .so.1 链接，本地脚本踩过同一坑，修法照抄 `scripts/loong-release.sh`）；
     c. cargo build 前加 `export RUSTFLAGS='-Clink-arg=-Wl,--exclude-libs,ALL -Clink-arg=-Wl,--disable-new-dtags -Clink-arg=-Wl,-rpath,$ORIGIN/lib'`（RUSTFLAGS env 覆盖 config.toml，exclude-libs 必须一并带）；
     d. 打包段加 libxdo 随包（镜像里有 `/usr/lib/loongarch64-linux-gnu/libxdo.so.3`）+ `bash /build/scripts/check-linux-package.sh "/out/${stage}"`（/src 挂载了仓库）。
   - `scripts/loong-release.sh`：同步 c/d 两处（RUSTFLAGS + libxdo + 自检；它已带 ORT env 修复）。
   - `README.md`：更新预编译包说明（lib/ 自带 ORT+libxdo，RPATH 使直接运行可用；平台清单 6 个）。
   - 建议顺手：删 `loong64-deps` release；头注释第 19 行有一处历史合并造成的粘连文本（"…fork 支持）#  lipo 合并…"）可整理。
   - 全部 `bash -n` + `yaml.safe_load` 校验后 commit + push。
3. **验证收官轮**（6 job 全绿）→ 下载 loong 包在本地 QEMU/容器里跑 `scripts/check-linux-package.sh` 复核 → 让用户在 Arch 实测 `rumi-ui` 直接运行。
4. **重出用户可用的包**：删除 `ci-966db1f` 旧 Linux 资产或直接打 `v0.1.0` tag 出正式版（tag 会触发 release.yml 全平台 + 汇入同一 release）。
5. **重启巡检**（如用户需要）：20 分钟 cron 巡检自动化还在（`CronList` 可查），策略照旧"报错就修、只重跑报错平台、新轮自动取消旧轮"。

## 6. 踩过的坑（避免重蹈）

1. **Windows 的 build script 目录是 `build\ct2rs\<hash>`**（子目录形式），不是 Unix 的 `ct2rs-<hash>`；Collect 归档要全树 `Get-ChildItem -Recurse -Filter`。且 x86_64 的 CTranslate2 硬依赖 `cpu_features.lib`（缺它下游 LNK2019 GetX86Info）——Collect 必须断言存在。
2. **Windows 上 CMAKE_GENERATOR 条件表达式求值为空串仍会 export** → cmake 收到 `-G ""` 挂；要用 bash 内 `if` 条件 export。Ninja+msvc-dev-cmd 方案试过，msvc-dev-cmd 的 env 会踩坏 rustup 工具链的 link.exe——Windows 保持 VS 生成器。
3. **workflow 顶层 `env:` 不能引用 `runner`/`inputs` 上下文**（0-job 幽灵失败 run）；job 级 `if:` 不能引用 `matrix`。步骤里的 `${{ }}` 嵌 bash 双引号会被输入值里的引号截断——经 `env:` 传。
4. **`gh run rerun --failed` 用旧 commit 的 workflow 定义**——修复必须新 dispatch 验证，rerun 不算数。
5. **中科大镜像配置**（`/root/.cargo/config.toml`）在国内本地快，但 GitHub 境外 runner 返回 403——CI 龙芯构建脚本里 `rm -f /root/.cargo/config.toml`（已在文件里，勿删）。
6. **动态 matrix 表达式**：`(A && s) || (B && s2) || default` 这类布尔混 `fromJSON` 的写法会静默炸掉整个 matrix job；现在是 setup job 预过滤成纯 JSON 数组（`needs.setup.outputs.matrix`），别改回表达式版。
7. **curl 下载 release 资产必须 `-f`**（无 -f 时 404/403 的 HTML 页被存成文件，tar 解压才炸）；配 `--retry 3`。
8. **本地调试工具链**：`agy --add-dir <dir> --print '<prompt>'`（高级 AI 咨询，定位过 ct2s 拼写、MSBuild 之谜、CT2 体积构成）；镜像层列表用 `docker create` + `docker export | tar -tv`（不执行容器）。
9. **Windows 预编译 113MB 不是调试信息**：MSVC 把修饰名嵌进逐函数节名（`$unwind/$pdata/$ip2state...`），oneDNN 模板实例爆炸导致名字表巨大，剥不掉；它只是 CI 中间件不影响用户包。
10. **concurrency 已加到全部 workflow**（group=<name>-<ref>, cancel-in-progress）——同 CI 新轮自动杀旧轮，这是用户拍板的规矩，不要手动 gh run cancel 旧轮了。

## 7. 未验证 / 遗留

- 龙芯 CI 出包从未成功过（历轮均败于 §6-5/路径不一致），本轮预判失败后由 §5-1/5-2 的修复轮首次验证。本地等价产物已验证可用。
- 用户在 Arch 的实测（rumi-ui 直接运行）是最终验收标准，CI 绿不等于用户绿。
- loong 的 CT2 归档只有 3.7MB 是**设计使然**（无 oneDNN、单 ISA），不是缺文件——链接成功过（45104199268）。
- 巡检 cron（每 20 分钟）仍在运行，策略由用户指令驱动；全绿后可 `CronPauseSelf` 或删除。
- 性能优化遗留（用户未催）：agy P0 关 LTO、mac x64 ARM 交叉编译（需修 ct2rs build.rs 的 `cfg!(target_arch)` 宿主宏 bug）、win 预编译 strip 已做但收益仅 5%。
