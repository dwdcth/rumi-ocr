# Rumi OCR 多平台 CI 与打包发布工程总结

> 本文档沉淀 `dwdcth/rumi-ocr` 6 平台自动化构建、预编译依赖管理、分发包自检与发布体系的完整架构、平台专项踩坑与运维手册（2026-11-23 `v0.1.0` 全平台全绿验证定案）。

---

## 1. 体系总览

### 1.1 三个 Workflow 职责与触发策略

所有 workflow 均带 `concurrency`（`group: <name>-${{ github.ref }}`, `cancel-in-progress: true`），同分支新轮自动取消旧轮，避免 runner 浪费。

| Workflow | 文件 | 触发方式 | 职责 | 实测耗时 |
|---|---|---|---|---|
| **CI 门禁** | `.github/workflows/ci.yml` | 手动 `workflow_dispatch` | Linux x64 全量构建 + `cargo test --workspace` + protobuf `link_guard`（源码编 CT2 + ccache） | 4–5 min（ccache 命中态） |
| **预编译引导** | `.github/workflows/prebuilt-deps.yml` | 手动 `workflow_dispatch`（仅 C++/CT2/oneDNN 升级时跑） | 5 主流平台经 cargo CMake 编出 `ctranslate2` + `dnnl`（+ x64 `cpu_features`）静态库；龙芯 QEMU 纯 cmake 编 `libctranslate2.a`。全部上传至 `deps` release | 13–29 min / 平台 |
| **多平台发布** | `.github/workflows/release.yml` | `push` tag `v*` / 手动 `workflow_dispatch`（支持 `targets` 挑平台） | 6 平台**预编译消费模式**（C++ 零编译）+ 平台分发包自检；tag 触发时汇总 6 包 + `SHA256SUMS.txt` 自动挂入同一 GitHub Release | 冷编 10–28 min；缓存命中 3–6 min |

### 1.2 六大目标平台与发布实测基线（`v0.1.0`）

| 平台 Target | CI Runner | 构建模式 | 冷编耗时 | 成品包大小 | 随包动态库 |
|---|---|---|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | 原生 + `lld` + `ct2-prebuilt` | ~15 min | 59.2 MiB | `libonnxruntime.so(.1)`、`libxdo.so.3` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | 原生 + `lld` + `ct2-prebuilt` | ~12 min | 50.1 MiB | `libonnxruntime.so(.1)`、`libxdo.so.3` |
| `loongarch64-unknown-linux-gnu` | `ubuntu-latest` | **x86_64 宿主原生交叉编译** + `clang/lld` + sysroot + QEMU 验包 | **~15 min**（本机 4m22s） | 54.0 MiB | `libonnxruntime.so(.1)`、`libxdo.so.3` |
| `x86_64-pc-windows-msvc` | `windows-latest` | 原生 MSVC `/MT` + `ct2-prebuilt` | ~14 min | 66.0 MiB | `onnxruntime.dll` |
| `aarch64-apple-darwin` | `macos-latest` | 原生 AppleClang + `ct2-prebuilt` | ~10 min | 45.1 MiB | `libonnxruntime.1.22.1.dylib`、`libomp.dylib` |
| `x86_64-apple-darwin` | `macos-15-intel` | 原生 AppleClang + `ct2-prebuilt` | ~21–28 min | 54.7 MiB | `libonnxruntime.1.22.1.dylib`、`libomp.dylib` |

### 1.3 静态/动态链接核心架构

- **离线翻译引擎（全平台静态链接）**：
  - `CTranslate2`（`libctranslate2.a` / `ctranslate2.lib`）+ `oneDNN`（`libdnnl.a` / `dnnl.lib`，龙芯除外）+ x86_64 `cpu_features` + 纯 Rust 分词器 `sentencepiece-rs` 全部静态编进可执行文件。
  - 正式打包走 `CT2RS_PREBUILT_DIR` 消费 `deps` release 里的预编译归档，实现 **C++ 零编译**。
- **ONNX Runtime（全平台动态链接）**：
  - `ort-sys` 走 `pkg-config` 动态链，彻底隔离 protobuf：避免静态 ORT 内嵌的 protobuf 与其他静态 C++ 依赖发生 ELF 全局符号劫持（interposition）导致模型加载期 SIGSEGV。
  - 源统一改用 **NuGet 官方包** `Microsoft.ML.OnnxRuntime`（本质是 zip，内嵌全部 RID 的 `runtimes/<rid>/native/` 与 `build/native/include/`），规避微软官方 GitHub release 404 问题。
  - 构建前强制 `pkg-config --modversion libonnxruntime` 断言，绝不给 `ort-sys` 静默回退静态下载的机会。

### 1.4 Cargo Workspace Feature 穿透坑与正确设计

#### 踩过的坑
`translate/Cargo.toml` 最初定义了：
```toml
[features]
default = ["ct2-dnnl"]
ct2-dnnl = ["ct2rs/dnnl"]
ct2-prebuilt = ["ct2rs/dnnl-prebuilt"]
```
但 `ui/Cargo.toml` 和 `server/Cargo.toml` 依赖它时写了 `rumi-translate = { path = "../translate" }`（**未设 `default-features = false`**）。

结果导致即使在 workspace 根目录执行：
```bash
cargo build --release --workspace --no-default-features --features rumi-translate/ct2-prebuilt
```
`--no-default-features` 只关掉了选中包自身的 `default`，`rumi-ui` 和 `rumi-server` 依然通过依赖边隐式拉回了 `rumi-translate/default` → `ct2rs/dnnl` → `onednn-src`！
- 在 5 主流平台上：白白重复编译了一整遍 `onednn-src`（预编译形同虚设）；
- 在龙芯平台上：直接拉起 `onednn-src` 编 x64 汇编（`-msse4.1`），导致 `loongarch64` 必挂。

#### 根治方案
在 `ui/Cargo.toml` 与 `server/Cargo.toml` 中均改为 `default-features = false` 并透传 feature：
```toml
[features]
default = ["ct2-dnnl"]
ct2-dnnl = ["rumi-translate/ct2-dnnl"]
ct2-prebuilt = ["rumi-translate/ct2-prebuilt"]

[dependencies]
rumi-translate = { path = "../translate", default-features = false }
```
**三态验证命令**（修改依赖后务必用 `cargo tree` 复核）：
```bash
# 1) 本地开发 / ci.yml / prebuilt-deps.yml → ct2rs 含 dnnl
cargo tree --workspace -e features -i ct2rs
# 2) 5 主流平台 release.yml → ct2rs 仅含 dnnl-prebuilt（无 onednn-src）
cargo tree --workspace -e features -i ct2rs --no-default-features --features rumi-translate/ct2-prebuilt
# 3) 龙芯 release.yml → ct2rs 既无 dnnl 也无 dnnl-prebuilt
cargo tree --workspace -e features -i ct2rs --no-default-features
```

---

## 2. Linux（x86_64 & aarch64）打包经验与踩坑

### 2.1 `DT_RPATH=$ORIGIN/lib` vs `DT_RUNPATH`
- **现象**：Ubuntu CI 打出的 Linux 包放到 Arch Linux 上直接运行 `./rumi-ui` 或 `./rumi-server` 失败：
  1. `rumi-ui` 报 `libxdo.so.3: cannot open shared object file`（Ubuntu 24.04 是 `.so.3`，Arch 的 xdotool 新版只有 `.so.4`）；
  2. 直接运行（不经 `run-*.sh`）时，加载器搜到系统里的 `/usr/lib/libonnxruntime.so.1`（Arch 为 1.29.0），与二进制要求的符号版本 `VERS_1.22.1` 冲突直接拒绝。
- **根因**：现代链接器默认写入 `DT_RUNPATH`，而 glibc `ld.so` 查找顺序为 `DT_RPATH` > `LD_LIBRARY_PATH` > `DT_RUNPATH` > `ld.so.cache`——虽 `DT_RUNPATH` 也在 `ld.so.cache` 之前，但 `DT_RUNPATH` **不传递**给间接依赖的动态库；而 `--disable-new-dtags` 写入的 `DT_RPATH` 具有传递性且稳定优先命中包内 `lib/`。
- **修法**：
  - 链接期加 `-Clink-arg=-Wl,--disable-new-dtags -Clink-arg=-Wl,-rpath,$ORIGIN/lib`；
  - `lib/` 随包携带 `libonnxruntime.so` + 符号链接 `libonnxruntime.so.1` + 宿主的 `libxdo.so.<N>`（解引用 `cp -L`）。
  - 注意：`RUSTFLAGS` 环境变量会**覆盖** `.cargo/config.toml` 的 `rustflags`，因此 `--exclude-libs,ALL` 必须一并显式携带。

### 2.2 GHA `env:` 中 `$ORIGIN` 双美元符号误转义坑
- **坑**：在 GitHub Actions `env:` 表达式里误写了 `-rpath,$$ORIGIN/lib`（把 Makefile 规则带进了 GHA）。
- **真相**：GHA `${{ '...' }}` 单引号字符串、Cargo `RUSTFLAGS` 拆词、`rustc` `execve` 传参**全都不经过 shell 也不反转义 `$$`**，结果二进制被写进了字面量 `RPATH $$ORIGIN/lib`，导致 `ld.so` 完全找不到包内库！
- **修法**：GHA `env:` 表达式里写**单 `$`**（`-rpath,$ORIGIN/lib`），并在 `scripts/check-linux-package.sh` 中用严格正则 `(\[|[[:space:]])[$]ORIGIN/lib(\]|$)` 拦截 `$$ORIGIN`。

### 2.3 `set -o pipefail` 下 `ldconfig -p | awk '...; exit'` 的 SIGPIPE（exit 141）
- **坑**：`xdo=$(ldconfig -p | awk '/libxdo\.so\.[0-9]+/ {print $NF; exit}')` 在库数量较多的系统（如 Arch）上，`awk` 命中后立刻 `exit` 关闭管道，导致上游 `ldconfig -p` 收到 `SIGPIPE` 返回 141，在 `set -o pipefail` 下脚本直接中止。
- **修法**：不提前 `exit`，让 `awk` 读完管道：
  ```bash
  xdo=$(ldconfig -p | awk '/libxdo\.so\.[0-9]+/ && !f {print $NF; f=1}')
  ```
  同理，所有 `set -o pipefail` 脚本里的 `cmd | head -n N` 建议用 `cmd | awk 'NR<=N'` 替代。

### 2.4 分发包自动化自检门禁（`scripts/check-linux-package.sh`）
打包前强制执行四项硬检查，缺一不可：
1. **直接依赖白名单**：每个 bin 的 `DT_NEEDED` 要么存在于包内 `lib/`，要么在明确登记的宿主发行版允许清单内；新增第三方动态库依赖会直接红灯。
2. **`DT_RPATH=$ORIGIN/lib` 校验**：必须为 `RPATH`（非 `RUNPATH`）且精确匹配单 `$ORIGIN/lib`。
3. **动态加载器真实解析**：
   - 固定 `export LC_ALL=C`（中文 locale 会翻译 `readelf`/`objdump` 标签导致正则失效）；
   - 用 `readelf -l` 提取 `PT_INTERP`（binutils 2.47 起 `objdump -p` 不再输出 interpreter 行）；
   - 运行 `"$loader" --list "$bin"` **不能只看退出码**（实测符号版本不匹配时 `--list` 仍返回 0，但会打印 `version ... not found`），必须逐行白名单校验；
   - **Ubuntu glibc 2.39 兼容**：对链了 `libgbm`/`EGL` 的二进制（`rumi-ui`/`rumi-capture`），`ld.so --list` 会额外打印一行匿名映射 `\t (0x0000...)`，正则需允许 `^[[:space:]]*(linux-vdso\.so\.1|/[^ ]+ld-[^ ]*)? \(0x[0-9a-f]+\)$`。
4. **随包库命中断言**：`--list` 输出中，本 bin 依赖且存在于 `$stage/lib/` 的库，必须精确解析到 `$stage/lib/<lib>`，证明即使构建机系统里存在同名库也会被包内副本抢先。

### 2.5 其他 Linux 细节
- **`CXXFLAGS=-march=x86-64`**：防 CTranslate2 基础 `kernels.cc` 在 CI runner 上发生 AVX2 ISA 泄漏（否则缺少 GENERIC/AVX 模板实例导致链接报 `did you mean (CpuIsa)2`）。
- **链接器提速**：安装 `lld` 并加 `-Clink-arg=-fuse-ld=lld`，ThinLTO + 大 C++ 静态库链接时间从数十秒降到数秒。
- **最终验收铁律**：CI（Ubuntu）绿 ≠ 跨发行版绿，必须在非 Ubuntu 发行版（如 Arch）上 `unset LD_LIBRARY_PATH` 直接运行 `./rumi-ui` 完成完整 GUI + OCR + 翻译闭环验收。

---

## 3. macOS（aarch64 & x86_64）打包经验与踩坑

### 3.1 架构决策：原生双 Runner，不做交叉编译 `lipo`
- `ct2rs` 的 `build.rs` 使用宿主编译期宏 `cfg!(target_arch)` 决定 `CMAKE_OSX_ARCHITECTURES`，在 ARM runner 上交叉编译 x86_64 必错架构；因此 `aarch64-apple-darwin`（`macos-latest`）与 `x86_64-apple-darwin`（`macos-15-intel`）各自原生构建。

### 3.2 Homebrew `libomp` 的三连坑（重点！）
AppleClang 不内置 OpenMP，而 CTranslate2（`OPENMP_RUNTIME=COMP`）与 oneDNN 依赖 OpenMP，因此 CI 安装 `brew install libomp`：

1. **编译探测坑**：`libomp` 是 keg-only，且 CTranslate2 的 `cmake_minimum_required(3.5)` 未启用 `CMP0074`，CMake 会忽略 `OpenMP_ROOT`。必须执行 `brew link --force libomp`，同时设置 `CPATH`（给 oneDNN 找 `omp.h`）与 `LIBRARY_PATH`（给 rustc 链 `-lomp`）。
2. **分发运行坑（差点漏掉的隐蔽 bug）**：构建出的 `rumi-ui`、`rumi-server`、`rumi-translate-cli` 的 Mach-O `LC_LOAD_DYLIB` 会硬编码构建机的 Homebrew 绝对路径：
   - arm64：`/opt/homebrew/opt/libomp/lib/libomp.dylib`
   - x64：`/usr/local/opt/libomp/lib/libomp.dylib`
   未装过 `brew install libomp` 的 Mac 用户直接启动必挂。
3. **根治方案（已入 `release.yml` 打包段 + 自检门禁）**：
   ```bash
   omp_prefix="$(brew --prefix libomp)"
   cp -L "${omp_prefix}/lib/libomp.dylib" "$stage/lib/libomp.dylib"
   chmod 755 "$stage/lib/libomp.dylib"          # brew 原文件常为 0444，不 chmod 会导致 install_name_tool 拒写
   install_name_tool -id @rpath/libomp.dylib "$stage/lib/libomp.dylib"

   for b in rumi-ui rumi-server rumi-capture rumi-translate-cli; do
     install_name_tool -add_rpath @executable_path/lib "$stage/$b"
     while IFS= read -r omp_ref; do
       [ -n "$omp_ref" ] && install_name_tool -change "$omp_ref" @rpath/libomp.dylib "$stage/$b"
     done < <(otool -L "$stage/$b" | awk '/libomp\.dylib/ {print $1}')
     codesign --force --sign - "$stage/$b"
   done
   codesign --force --sign - "$stage/lib/libonnxruntime.${ORT_VERSION}.dylib"
   codesign --force --sign - "$stage/lib/libomp.dylib"
   ```
   并在打包前用 `otool -L` 扫全部二进制与 `.dylib`，**断言输出中不得出现 `/opt/homebrew` 或 `/usr/local`**。

### 3.3 `x86_64-apple-darwin` 预编译缺 `_GetX86Info`
- **现象**：`release.yml` 消费 `ct2-prebuilt-x86_64-apple-darwin.tar.gz` 链接 `rumi-server` 时报 `Undefined symbols for architecture x86_64: "_GetX86Info"`。
- **根因**：`prebuilt-deps.yml` 的 `Collect archives (unix)` 原写法只在 `DIR=$(dirname "$CT2")`（即 `out/lib/`）里找 `libcpu_features.a`，而 macOS 上该文件留在 `out/build/third_party/cpu_features/libcpu_features.a`（不在 `out/lib/` 下），被静默漏打。
- **修法**：改为全 `target` 树 `find target -name 'libcpu_features.a'`，且对所有 `x86_64-*` 目标强制 `test -n "$CF"` 断言。

### 3.4 `@rpath` 与 Ad-hoc 签名
- macOS 上 `DYLD_LIBRARY_PATH` 对 `@rpath` 依赖无效；`libonnxruntime.${ORT_VERSION}.dylib` 需按 `install_name` 放入 `lib/` 并给每个 bin 加 `LC_RPATH=@executable_path/lib`。
- 任何经 `install_name_tool` 改动的二进制及 NuGet 解出的原始 `.dylib`，都必须执行 `codesign --force --sign -` 做 ad-hoc 签名（Apple Silicon 上无有效签名会被内核直接 `SIGKILL`）。
- ad-hoc 签名存储在 Mach-O 的 `LC_CODE_SIGNATURE` load command 中（不在 xattr 里），因此在 Linux 上解压重命名顶层目录再重新打包不会破坏签名。
- 用户从浏览器下载的包首次运行若被 Gatekeeper 拦截，提示执行 `xattr -d com.apple.quarantine rumi-ui`。

---

## 4. Windows（x86_64-pc-windows-msvc）打包经验与踩坑

### 4.1 静态 CRT（`/MT`）与免 VC++ 运行库
- `ct2rs` 传给 CTranslate2 的是 `static_crt(true)`（`/MT`），Rust 侧必须统一加 `RUSTFLAGS=-Ctarget-feature=+crt-static`，否则链接报 `LNK2038`（MT/MD 不匹配）。
- 顺带收益：用户无需额外安装 VC++ Redistributable。
- `onnxruntime.dll` 自身是 `/MD` 自带 CRT，但只通过纯 C API 跨边界交互不传 CRT 对象，混合链接完全合法，且 `onnxruntime.dll` 与 `.exe` 同目录即可被 Windows 加载器自动找到。

### 4.2 `prebuilt-deps` Windows 收集归档的目录布局坑
- **坑**：Windows 下 Cargo 的 build script 输出目录是 `target/<t>/release/build/ct2rs/<hash>/`（嵌套子目录），不同于 Unix 的 `ct2rs-<hash>`；最初按 `ct2rs-*` glob 搜导致漏掉 `cpu_features.lib`，下游链接报 `LNK2019: unresolved external symbol GetX86Info`。
- **修法**：PowerShell 一律 `Get-ChildItem "target/$t/release/build" -Recurse -Filter <name>` 全树查找，且对 `ctranslate2.lib`、`dnnl.lib`、`cpu_features.lib` 三者全部强制非空断言。

### 4.3 `CMAKE_GENERATOR` 与为何保持 VS 生成器
- **坑 1**：在 workflow `env:` 中写条件表达式 `CMAKE_GENERATOR: ${{ runner.os != 'Windows' && 'Ninja' || '' }}` 时，Windows 仍会导出一个**空字符串环境变量**，导致 `cmake-rs` 传 `-G ""` 直接报 `Could not create named generator`。修法：在 bash step 内用 `if [ "${{ runner.os }}" != "Windows" ]; then export CMAKE_GENERATOR=Ninja; fi` 条件导出。
- **坑 2**：尝试过 Ninja + `msvc-dev-cmd`，但 `msvc-dev-cmd` 注入的环境变量会踩坏 rustup 对 MSVC `link.exe` 的探测。Windows 保持默认 VS 生成器最稳，预编译消费模式下全量仅 ~14 min。

### 4.4 Windows 预编译归档 113MB 的真相
- `ct2-prebuilt-x86_64-pc-windows-msvc.tar.gz` 体积约 113MB（`llvm-strip --strip-debug` 仅从 119.7MB 降到 113.3MB）。
- 原因不是调试信息，而是 MSVC 将 C++ 修饰名嵌进逐函数节名（`$unwind/$pdata/$ip2state`），oneDNN 模板实例化后名字表巨大，无法安全剥离。它仅作 CI 构建期中间件，最终用户 `.zip` 成品包约 66MB，不受影响。

### 4.5 NuGet 解压与 pkg-config
- Windows 上用 `tar -xf`（系统自带 bsdtar）按 RID 选择性解压 NuGet 包，比 PowerShell `Expand-Archive` 全量解 500MB 快得多；通过 `choco install pkgconfiglite` 提供 `pkg-config` 供 `ort-sys` 探测。

---

## 5. 龙芯（loongarch64 新世界 ABI 2.0）工程化与 10 倍提速实战

### 5.1 龙芯生态约束与基础资产
- **无 oneDNN 后端**：上游 oneDNN 无 LoongArch CPU 后端，CTranslate2 的多 ISA dispatch 也不覆盖龙芯。使用 `dwdcth/ctranslate2-rs@loongarch64` fork 关 `ENABLE_CPU_DISPATCH`，龙芯构建传 `--no-default-features` 去掉 `ct2-dnnl`，只链接预编译的 `libctranslate2.a`（3.7MB，设计使然）。
- **基础镜像**：`ghcr.io/dwdcth/rumi-loong-build:v1`（4.1GB，Debian trixie 新世界 + WebKitGTK/GTK3/ALSA/PipeWire/libxdo/GBM/DRM/X11/Wayland dev 包 + 社区版 ORT 1.32 + rustup）。

### 5.2 全容器 QEMU 方案的教训（为何淘汰）
早期把 `cargo build` 整个放进 `--platform linux/loong64` QEMU 容器里跑，踩了五连坑：
1. 镜像内带本地中科大 crates 镜像 `/root/.cargo/config.toml`，境外 GitHub runner 访问返 403；
2. 镜像内 `/opt/onnxruntime/lib/` 只有 `libonnxruntime.so -> libonnxruntime.so.1.32.0`、没有 `.so.1` 链接，直接 `cp -P ... .so.1` 会挂（需 `cp -L ... .so` + `ln -sf libonnxruntime.so .so.1`）；
3. `CT2RS_PREBUILT_DIR` 解压路径与 export 路径不一致；
4. `onednn-src` feature 穿透触发 x64 编译报错；
5. **最致命的性能瓶颈**：即使 CTranslate2 已预编译，**400+ 个 Rust crate 的 `rustc` 和 LLVM 优化器全在 QEMU 用户态下逐指令翻译执行**，冷编耗时 **>2 小时**，且 `actions/cache@v4` 失败/取消时不保存缓存，排障反馈周期极慢。

### 5.3 终极提速方案：x86_64 宿主原生交叉编译 + Sysroot（2h+ → 15 min / 本机 4m22s）

**核心思路**：让 `rustc`、`clang`、`lld`、`bindgen` 全部跑在 **x86_64 原生 CPU** 上，只把 `rumi-loong-build:v1` 当作 **sysroot 来源**；QEMU 只在最后 3 秒用来跑 `check-linux-package.sh` 和冒烟执行。

#### 关键实现步骤（`release.yml` 与 `scripts/loong-release.sh` 完全同构）
1. **10 秒零启动导出 sysroot**（不启动 QEMU）：
   ```bash
   cid=$(docker create --platform linux/loong64 ghcr.io/dwdcth/rumi-loong-build:v1 true)
   docker export "$cid" | tar -xf - -C "$SYSROOT" \
     usr/include usr/lib usr/lib64 usr/share/pkgconfig lib lib64 opt/onnxruntime
   ```
2. **宿主安装轻量工具**：
   - Ubuntu runner：`sudo apt-get install -y clang lld llvm libclang-dev pkg-config libssl-dev`
   - Rust target：`rustup target add loongarch64-unknown-linux-gnu`
   - `clang`/`clang++` 包装脚本（Cargo linker 要求单一可执行文件路径）：
     ```sh
     #!/bin/sh
     exec clang --target=loongarch64-linux-gnu --sysroot=$SYSROOT --gcc-toolchain=$SYSROOT/usr -fuse-ld=lld "$@"
     ```
     （`--gcc-toolchain=$SYSROOT/usr` 让 `clang++` 直接复用 Debian trixie sysroot 里的 GCC 14 `libstdc++` 与 crt 文件，与预编译 GCC 14 的 `libctranslate2.a` ABI 无缝衔接。）
3. **最关键踩坑点 —— 宿主与目标环境变量必须严格隔离**：
   - **错误示范**：直接 export 全局 `PKG_CONFIG_LIBDIR` / `PKG_CONFIG_SYSROOT_DIR`。会导致构建期编译**宿主 x86_64** 的 build-script / `openssl-sys` 时误读龙芯 sysroot 头文件，因缺少多架构头目录报 `#error "OPENSSL_API_COMPAT expresses an impossible API compatibility level"`。
   - **正确写法（全套 target 前缀）**：
     ```bash
     export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER=/tmp/loong-cc
     export CC_loongarch64_unknown_linux_gnu=/tmp/loong-cc
     export CXX_loongarch64_unknown_linux_gnu=/tmp/loong-cxx
     export AR_loongarch64_unknown_linux_gnu=llvm-ar
     export PKG_CONFIG_ALLOW_CROSS=1
     export PKG_CONFIG_SYSROOT_DIR_loongarch64_unknown_linux_gnu="$SYSROOT"
     export PKG_CONFIG_LIBDIR_loongarch64_unknown_linux_gnu="$SYSROOT/usr/lib/loongarch64-linux-gnu/pkgconfig:$SYSROOT/usr/share/pkgconfig:$SYSROOT/opt/onnxruntime/lib/pkgconfig"
     export BINDGEN_EXTRA_CLANG_ARGS_loongarch64_unknown_linux_gnu="--target=loongarch64-linux-gnu --sysroot=$SYSROOT"
     export CT2RS_PREBUILT_DIR="$PWD/ct2-prebuilt"
     export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-Clink-arg=-Wl,--exclude-libs,ALL -Clink-arg=-Wl,--disable-new-dtags -Clink-arg=-Wl,-rpath,$ORIGIN/lib'
     cargo build --release --workspace --target loongarch64-unknown-linux-gnu --no-default-features
     ```
4. **打包与 QEMU 3 秒真机加载验包**：
   - 在宿主直接拷 4 个 bin + `cp -L` 解引用 sysroot 的 `libonnxruntime.so`（+ `ln -sf .so.1`）与 `libxdo.so.3`；
   - 拉起一次性 QEMU 容器验证：
     ```bash
     docker run --rm --platform linux/loong64 \
       -v "$PWD/scripts":/scripts:ro \
       -v "$PWD/dist/${stage}":/pkg:ro \
       ghcr.io/dwdcth/rumi-loong-build:v1 \
       bash -c 'set -euxo pipefail; unset LD_LIBRARY_PATH; bash /scripts/check-linux-package.sh /pkg; /pkg/rumi-translate-cli --help | awk "NR<=5"'
     ```
   - 额外收益：改用 `Swatinem/rust-cache@v2`（带 `cache-on-failure: true`），并与其余 5 平台完全共享 ThinLTO / `cgu=8` / `strip=symbols` 发布档位。

---

## 6. GitHub Actions 通用踩坑清单

1. **`concurrency.group` 漏写双大括号**：`${ github.ref }` 会被当成字面量字符串（所有 ref 共享一个组，推 tag `v0.1.0` 会把 `main` 上在跑的 run 杀掉），必须写 `${{ github.ref }}`。
2. **`Swatinem/rust-cache@v2` 输入参数名**：正确参数是 `prefix-key` 与 `env-vars`，误写成 `prefix` / `envVars` 会触发 warning 且被静默忽略。
3. **动态 matrix 表达式**：不要在 `matrix.target` 里写 `(A && s) || (B && s2) || default` 混 `fromJSON`，容易静默炸空 matrix；固定用独立 `setup` job 跑 Python 过滤输出纯 JSON 数组（同时剔除仅作开关的 `loongarch64` 别名）。
4. **`${{ inputs.* }}` 传参防引号截断**：不要把 `${{ github.event.inputs.targets }}` 直接嵌在 bash 双引号内，经步骤级 `env:` 传入。
5. **顶层 `env:` 上下文限制**：workflow 顶层 `env:` 不能引用 `runner` / `inputs`，否则触发 0-job 幽灵失败。
6. **`curl` 下载必带 `-f` 与 `--retry`**：无 `-f` 时 404/403 的 HTML 页会被当成 `.tar.gz` 落盘，拖到 `tar` 解压才报错。
7. **`gh run rerun --failed` 不读新 workflow**：rerun 复用旧 commit 的 workflow 定义；修复 workflow 后必须重新 `gh workflow run` 验证。

---

## 7. 运维与发版速查手册

### 7.1 正式发版（全 6 平台自动构建并汇入同一 Release）
```bash
git tag v0.2.0 && git push origin v0.2.0
```

### 7.2 手动打包 / 仅补跑指定平台
```bash
# 全 6 平台
gh workflow run release.yml -R dwdcth/rumi-ocr

# 仅跑指定平台（5 主流用完整 target，龙芯用 loongarch64）
gh workflow run release.yml -R dwdcth/rumi-ocr \
  -f targets='["aarch64-apple-darwin","x86_64-apple-darwin"]'
```

### 7.3 依赖升级时重建 `deps` 预编译库
仅在 `ct2rs` / `CTranslate2` / `oneDNN` 版本变更时需要跑一次：
```bash
# 全 6 平台
gh workflow run prebuilt-deps.yml -R dwdcth/rumi-ocr

# 仅重建单个主流平台（跳过龙芯）
gh workflow run prebuilt-deps.yml -R dwdcth/rumi-ocr \
  -f targets='["x86_64-apple-darwin"]' -f skip_loong=true
```

### 7.4 零编译复用已有绿包发 tag / 替换单平台资产
当某轮已产出干净的 CI artifact 且代码未变时：
1. 推 tag 后立即 `gh run cancel <id>` 取消自动触发的 `release.yml`；
2. `gh run download` 拉下 artifact，重命名顶层目录与压缩包名，重新生成 `SHA256SUMS.txt`；
3. 逐文件带重试执行 `gh release upload <tag> <file> --clobber`（大文件直传比一次性 `gh release create` 抗 EOF 断流强得多）；
4. 传完后 `gh release edit <tag> --draft=false --latest` 转正。

### 7.5 龙芯本地一键出包（交叉编译 ~4 min 冷编 / 秒级增量）
```bash
# 仅本地打包 + QEMU 验包（产物落 target/loong-out/）
./scripts/loong-release.sh v0.1.0

# 打包并通过校验后直接上传覆盖到指定 release
LOONG_RELEASE_UPLOAD=1 ./scripts/loong-release.sh v0.1.0
```
