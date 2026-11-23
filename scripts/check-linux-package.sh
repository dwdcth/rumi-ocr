#!/usr/bin/env bash
# check-linux-package.sh — Linux 分发包自检（打包后、上传前跑）。
#
# 背景（2026-11-23 实测）：CI 在 Ubuntu 上出的包拿到 Arch 上起不来——
#   - rumi-ui 依赖 libxdo.so.3（dioxus-desktop 的菜单栏/托盘后端链入），构建机
#     是 .so.3，Arch 的 xdotool 新版只有 .so.4；
#   - 直接运行（不经 run-*.sh）时 rumi-server / rumi-capture 用到系统里的
#     libonnxruntime.so.1，版本对不上（二进制要求符号版本 VERS_1.22.1）直接被
#     加载器拒绝。
# 因此规则是：每个 bin 的直接动态库依赖，要么随包放在 lib/，要么在下面的
# "宿主提供"允许清单里；并且二进制带 $ORIGIN/lib 的 DT_RPATH（RPATH 先于
# ld.so.cache 查找，直接运行也优先用包内库）。新增宿主依赖必须在这里显式
# 登记，否则打包失败。
#
# 用法: scripts/check-linux-package.sh <stage 目录>   （含 4 个 bin 与 lib/）
set -euo pipefail
# objdump / 加载器的输出随 locale 本地化（中文环境下 "Program Interpreter:"
# 等标签被翻译导致匹配不上），固定英文
export LC_ALL=C

# 绝对路径（加载器按 $ORIGIN 解析后打印的是绝对路径，下面要逐项比对）
stage=$(cd "${1:?用法: $0 <stage 目录>}" && pwd -P)

# 由宿主发行版提供、不随包的库：三个 Linux 目标（x64 / arm64 为 Ubuntu CI 产物，
# loongarch64 为 Debian trixie QEMU 产物）四个 bin 的 NEEDED 并集，扣除随包的
# libonnxruntime.so.1 与 libxdo.so.*。
# 末行五项目前只有 loongarch64 产物依赖，x64 / arm64 没有——差异来源未深究，
# 想收窄可先对比两边的链接器参数（x64/arm64 用 lld，龙芯用 GNU ld）。
allowed=(
  ld-linux-x86-64.so.2 ld-linux-aarch64.so.1 ld-linux-loongarch-lp64d.so.1
  libc.so.6 libm.so.6 libgcc_s.so.1 libstdc++.so.6 libgomp.so.1
  libssl.so.3 libcrypto.so.3
  libasound.so.2 libgbm.so.1 libwayland-client.so.0 libxcb.so.1
  libcairo.so.2 libgdk-3.so.0 libgdk_pixbuf-2.0.so.0
  libgio-2.0.so.0 libglib-2.0.so.0 libgobject-2.0.so.0 libgtk-3.so.0
  libjavascriptcoregtk-4.1.so.0 libsoup-3.0.so.0 libwebkit2gtk-4.1.so.0
  libcairo-gobject.so.2 libEGL.so.1 libpango-1.0.so.0 libpipewire-0.3.so.0 libwayland-server.so.0
)
is_allowed() { local l; for l in "${allowed[@]}"; do [ "$l" = "$1" ] && return 0; done; return 1; }

status=0
fail() { echo "  ✗ $*"; status=1; }

for b in rumi-ui rumi-server rumi-capture rumi-translate-cli; do
  bin="$stage/$b"
  echo "[$b]"
  if [ ! -f "$bin" ]; then fail "缺少 $bin"; continue; fi
  info=$(objdump -p "$bin")

  # 1) 直接依赖：随包 或 允许清单
  for lib in $(awk '/^[[:space:]]*NEEDED/ {print $2}' <<<"$info"); do
    if [ -e "$stage/lib/$lib" ]; then continue; fi
    if is_allowed "$lib"; then continue; fi
    fail "$lib 既不在包内 lib/，也不在宿主允许清单（随包带上或登记到 allowed）"
  done

  # 2) DT_RPATH=$ORIGIN/lib（RUNPATH 排在 ld.so.cache 之后，不够）
  # 兼容两代 binutils 的打印："RPATH  Library rpath: [$ORIGIN/lib]" / "RPATH  $ORIGIN/lib"
  if grep -E '^[[:space:]]*RPATH[[:space:]].*(\[|[[:space:]])[$]ORIGIN/lib(\]|$)' <<<"$info" | grep -q .; then
    echo "  RPATH \$ORIGIN/lib"
  else
    fail "缺 DT_RPATH \$ORIGIN/lib（直接运行不会优先用包内库）"
  fi

  # 3) 按直接运行的方式让加载器解析一遍（不执行程序）：不允许任何错误，且
  #    包内 lib/ 的库必须确实解析到包内副本
  # 解释器路径取 PT_INTERP：新版 binutils 的 objdump -p 不再打印它，readelf 有
  # （行尾带 "]"，去掉）
  loader=$(readelf -l "$bin" | awk '/Requesting program interpreter:/ {gsub(/\]/, "", $NF); print $NF}')
  needed=$(awk '/^[[:space:]]*NEEDED/ {print $2}' <<<"$info")
  if [ -z "$loader" ] || [ ! -x "$loader" ]; then
    fail "取不到或找不到程序解释器（loader='$loader'）"
    continue
  fi
  # 加载器对致命错误也可能返回 0（实测符号版本不匹配时 --list 退出码为 0
  # 但打印了 "version ... not found"），所以不看退出码，看有没有非正常行
  out=$(LC_ALL=C "$loader" --list "$bin" 2>&1) || true
  # 正常行形如 "lib => /path (0x...)"、"linux-vdso.so.1 (0x...)"、"/lib64/ld-*.so (0x...)"；
  # 加载器打印错误后，其余行会带制表符前缀，行首允许空白
  # grep -v 在"没有异常行"时退出码为 1，pipefail + set -e 下会中止脚本，故 || true
  abnormal=$({ grep -vE '^[[:space:]]*[^ ]+ => /[^ ]+ \(0x[0-9a-f]+\)$|^[[:space:]]*(linux-vdso\.so\.1|/[^ ]+ld-[^ ]*) \(0x[0-9a-f]+\)$' <<<"$out" || true; } | tr '\n' ' ')
  if [ -n "$abnormal" ]; then
    fail "直接运行加载异常：$abnormal"
    continue
  fi
  echo "  加载器解析通过"
  # 仅对本 bin 实际依赖且随包的库比对：必须解析到包内副本（RPATH 先于
  # ld.so.cache，所以即便构建机系统里也有同名库也应命中包内）
  for lib in $needed; do
    [ -e "$stage/lib/$lib" ] || continue
    case "$out" in
      *"$lib => $stage/lib/$lib "*) ;;
      *) fail "包内库未被直接运行用到: $lib（用到了别处的同名库）" ;;
    esac
  done
done

if [ "$status" -ne 0 ]; then
  echo "分发包自检失败（见上）" >&2
else
  echo "分发包自检通过"
fi
exit "$status"
