#!/usr/bin/env bash
# loong-release.sh — 龙芯（loongarch64 新世界）本地打包直传。
#
# 设计：x86_64 宿主原生 rustc + clang/lld 交叉编译（从 ghcr.io/dwdcth/rumi-loong-build:v1
# 导出并缓存 Debian trixie sysroot，deps release 提供预编译 libctranslate2.a），
# 最后仅用 QEMU 容器跑 check-linux-package.sh 与冒烟执行验包（全量冷编 ~4-5min，
# 增量几十秒）。
#
# 宿主前置：rustup target add loongarch64-unknown-linux-gnu、clang、lld、llvm-ar、pkg-config、docker
#
# 用法:
#   ./scripts/loong-release.sh <release-tag> [label]
#     <release-tag>  目标 release（已存在则追加资产，如 ci-966db1f / v0.1.0）
#     [label]        包名里的版本段（默认 git short sha；tag 以 v 开头时用 tag 名）
#
# 示例:
#   ./scripts/loong-release.sh ci-966db1f                 # 补龙芯包到 CI 产物 release
#   ./scripts/loong-release.sh v0.1.0                     # 正式版（label 取 v0.1.0）
#   ./scripts/loong-release.sh draft-test $(git rev-parse --short HEAD)-beta1
set -euo pipefail

TAG="${1:?用法: $0 <release-tag> [label]}"
LABEL="${2:-}"
REPO=dwdcth/rumi-ocr
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/target/loong-out"
SYSROOT="$ROOT/target/loong-sysroot"
CT2_DIR="$ROOT/target/loong-ct2-prebuilt"

if [[ -z "$LABEL" ]]; then
  if [[ "$TAG" == v* ]]; then LABEL="$TAG"; else LABEL="$(git -C "$ROOT" rev-parse --short HEAD)"; fi
fi
TARGET=loongarch64-unknown-linux-gnu
STAGE="rumi-${LABEL}-${TARGET}"

for cmd in clang clang++ ld.lld llvm-ar pkg-config docker rustup cargo; do
  command -v "$cmd" >/dev/null || { echo "缺少宿主工具: $cmd" >&2; exit 1; }
done
rustup target list --installed | grep -qx "$TARGET" || rustup target add "$TARGET"

# 1) 导出并缓存龙芯 sysroot（仅首次）
if [[ ! -f "$SYSROOT/opt/onnxruntime/lib/pkgconfig/libonnxruntime.pc" ]]; then
  echo "== 从 ghcr.io/dwdcth/rumi-loong-build:v1 导出 sysroot → $SYSROOT =="
  rm -rf "$SYSROOT" && mkdir -p "$SYSROOT"
  cid=$(docker create --platform linux/loong64 ghcr.io/dwdcth/rumi-loong-build:v1 true)
  trap 'docker rm -f "$cid" >/dev/null 2>&1 || true' EXIT
  docker export "$cid" | tar -xf - -C "$SYSROOT" \
    usr/include usr/lib usr/lib64 usr/share/pkgconfig lib lib64 opt/onnxruntime
  docker rm -f "$cid" >/dev/null 2>&1 || true
  trap - EXIT
fi

# 2) CTranslate2 预编译 .a（deps release）
mkdir -p "$CT2_DIR" "$OUT"
if [[ ! -f "$CT2_DIR/libctranslate2.a" ]]; then
  curl -fsSL --retry 3 --retry-delay 5 -o /tmp/ct2.tgz \
    "https://github.com/${REPO}/releases/download/deps/ct2-prebuilt-loongarch64-unknown-linux-gnu.tar.gz"
  tar -xzf /tmp/ct2.tgz --strip-components=1 -C "$CT2_DIR"
fi
test -f "$CT2_DIR/libctranslate2.a"

# 3) 交叉编译包装脚本（Cargo linker 需单一可执行文件路径）
WRAP_DIR="$ROOT/target/loong-wrappers"
mkdir -p "$WRAP_DIR"
cat > "$WRAP_DIR/cc" <<EOF
#!/bin/sh
exec clang --target=loongarch64-linux-gnu --sysroot=$SYSROOT --gcc-toolchain=$SYSROOT/usr -fuse-ld=lld "\$@"
EOF
cat > "$WRAP_DIR/cxx" <<EOF
#!/bin/sh
exec clang++ --target=loongarch64-linux-gnu --sysroot=$SYSROOT --gcc-toolchain=$SYSROOT/usr -fuse-ld=lld "\$@"
EOF
chmod +x "$WRAP_DIR/cc" "$WRAP_DIR/cxx"

# 4) 宿主原生交叉编译（目标级 env 隔离，不污染宿主 build-script/proc-macro）
export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER="$WRAP_DIR/cc"
export CC_loongarch64_unknown_linux_gnu="$WRAP_DIR/cc"
export CXX_loongarch64_unknown_linux_gnu="$WRAP_DIR/cxx"
export AR_loongarch64_unknown_linux_gnu=llvm-ar
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR_loongarch64_unknown_linux_gnu="$SYSROOT"
export PKG_CONFIG_LIBDIR_loongarch64_unknown_linux_gnu="$SYSROOT/usr/lib/loongarch64-linux-gnu/pkgconfig:$SYSROOT/usr/share/pkgconfig:$SYSROOT/opt/onnxruntime/lib/pkgconfig"
export BINDGEN_EXTRA_CLANG_ARGS_loongarch64_unknown_linux_gnu="--target=loongarch64-linux-gnu --sysroot=$SYSROOT"
export CT2RS_PREBUILT_DIR="$CT2_DIR"
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-Clink-arg=-Wl,--exclude-libs,ALL -Clink-arg=-Wl,--disable-new-dtags -Clink-arg=-Wl,-rpath,$ORIGIN/lib'

(cd "$ROOT" && cargo build --release --workspace --target "$TARGET" --no-default-features)

# 5) 打包（bins + ORT/libxdo + run-*.sh）
STAGE_DIR="$OUT/$STAGE"
rm -rf "$STAGE_DIR" && mkdir -p "$STAGE_DIR/lib"
for b in rumi-ui rumi-server rumi-capture rumi-translate-cli; do
  cp "$ROOT/target/${TARGET}/release/$b" "$STAGE_DIR/"
  printf '#!/bin/sh\nDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexport LD_LIBRARY_PATH="$DIR/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"\nexec "$DIR/%s" "$@"\n' "$b" > "$STAGE_DIR/run-$b.sh"
  chmod +x "$STAGE_DIR/run-$b.sh"
done
cp -L "$SYSROOT/opt/onnxruntime/lib/libonnxruntime.so" "$STAGE_DIR/lib/libonnxruntime.so"
ln -sf libonnxruntime.so "$STAGE_DIR/lib/libonnxruntime.so.1"
xdo=$(find "$SYSROOT/usr/lib/loongarch64-linux-gnu" -maxdepth 1 -name 'libxdo.so.[0-9]*' | sort | awk 'NR==1')
test -n "$xdo"
cp -L "$xdo" "$STAGE_DIR/lib/"
cp "$ROOT/README.md" "$STAGE_DIR/" 2>/dev/null || true

# 6) 仅在 QEMU 容器内做加载器自检与冒烟执行（重启后 binfmt_misc 需确保安装）
if ! ls /proc/sys/fs/binfmt_misc/ 2>/dev/null | grep -q qemu-loongarch64; then
  docker run --privileged --rm tonistiigi/binfmt --install loong64 >/dev/null
fi
docker run --rm --platform linux/loong64 \
  -v "$ROOT/scripts":/scripts:ro \
  -v "$STAGE_DIR":/pkg:ro \
  ghcr.io/dwdcth/rumi-loong-build:v1 \
  bash -c 'set -euxo pipefail; unset LD_LIBRARY_PATH; bash /scripts/check-linux-package.sh /pkg; /pkg/rumi-translate-cli --help | awk "NR<=5"'

TARBALL="$OUT/${STAGE}.tar.gz"
tar czf "$TARBALL" -C "$OUT" "$STAGE"
ls -lh "$TARBALL"
echo "== 龙芯出包与验包通过: $TARBALL =="

# 上传是显式动作（测试跑通不需要传）：LOONG_RELEASE_UPLOAD=1 ./scripts/loong-release.sh ...
if [[ "${LOONG_RELEASE_UPLOAD:-0}" == "1" ]]; then
  echo "== 上传到 release $TAG =="
  gh release upload "$TAG" "$TARBALL" --clobber -R "$REPO"
  echo "完成: ${STAGE}.tar.gz → $TAG"
else
  echo "未设 LOONG_RELEASE_UPLOAD=1，跳过上传（产物在 $TARBALL）"
fi
