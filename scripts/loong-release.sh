#!/usr/bin/env bash
# loong-release.sh — 龙芯（loongarch64 新世界）本地打包直传。
#
# 设计：环境全在本地（镜像 ghcr.io/dwdcth/rumi-loong-build:v1 由本机
# 构建推送；CTranslate2 预编译 .a 在 loong64-deps release），hosted CI
# 拿不到本机文件，索性本地出包：
#   容器（QEMU）cargo release 构建 → 打 tar.gz → 上传到指定 GitHub Release
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

if [[ -z "$LABEL" ]]; then
  if [[ "$TAG" == v* ]]; then LABEL="$TAG"; else LABEL="$(git -C "$ROOT" rev-parse --short HEAD)"; fi
fi
TARGET=loongarch64-unknown-linux-gnu
STAGE="rumi-${LABEL}-${TARGET}"

# 容器名冲突清理 + binfmt 确保（重启后 binfmt_misc 不持久）
docker rm -f rumi-loong-rel >/dev/null 2>&1 || true
if ! ls /proc/sys/fs/binfmt_misc/ | grep -q qemu-loongarch64; then
  docker run --privileged --rm tonistiagi/binfmt --install loong64 >/dev/null
fi

# CTranslate2 预编译 .a（loong64-deps release）
CT2_DIR="$ROOT/target/loong-ct2-prebuilt"
mkdir -p "$CT2_DIR" "$OUT"
if [[ ! -f "$CT2_DIR/libctranslate2.a" ]]; then
  curl -sSL -o /tmp/ct2.tgz \
    "https://github.com/${REPO}/releases/download/loong64-deps/libctranslate2-loong64.tar.gz"
  tar -xzf /tmp/ct2.tgz -C "$CT2_DIR"
fi
test -f "$CT2_DIR/libctranslate2.a"

# 容器内构建脚本（heredoc 落盘避免多层引号转义；容器路径恒定保
# build script fingerprint 有效 → target 挂载跨次增量）
cat > /tmp/loong-rel-build.sh <<'EOS'
set -euxo pipefail
. ~/.cargo/env
# tar 管道复制（排除挂载点 target 与 .git）：/src/target 是宿主的
# symlink，与容器挂载目录 /build/target 冲突，cp 直接报错
tar -C /src --exclude=./target --exclude=./.git -cf - . | tar -C /build -xf -
cd /build
export CT2RS_PREBUILT_DIR=/opt2/ct2-prebuilt
cargo build --release --workspace --target loongarch64-unknown-linux-gnu

target=loongarch64-unknown-linux-gnu
stage="rumi-${LABEL}-${target}"
mkdir -p "/out/${stage}/lib"
for b in rumi-ui rumi-server rumi-capture rumi-translate-cli; do
  cp "target/${target}/release/$b" "/out/${stage}/"
  printf '#!/bin/sh\nDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexport LD_LIBRARY_PATH="$DIR/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"\nexec "$DIR/%s" "$@"\n' "$b" > "/out/${stage}/run-$b.sh"
  chmod +x "/out/${stage}/run-$b.sh"
done
cp -P /opt/onnxruntime/lib/libonnxruntime.so /opt/onnxruntime/lib/libonnxruntime.so.1 "/out/${stage}/lib/"
cp README.md "/out/${stage}/" 2>/dev/null || true
tar czf "/out/${stage}.tar.gz" -C /out "$stage"
EOS

# 本地 target 持久目录（增量缓存；容器镜像不含）
mkdir -p "$ROOT/target/loong-target" "$ROOT/target/loong-cargo-registry" "$ROOT/target/loong-cargo-git"
docker run -d --name rumi-loong-rel --platform linux/loong64 \
  -v "$ROOT":/src:ro \
  -v "$ROOT/target/loong-target":/build/target \
  -v "$ROOT/target/loong-cargo-registry":/root/.cargo/registry \
  -v "$ROOT/target/loong-cargo-git":/root/.cargo/git \
  -v "$CT2_DIR":/opt2/ct2-prebuilt:ro \
  -v "$OUT":/out \
  ghcr.io/dwdcth/rumi-loong-build:v1 sleep infinity >/dev/null
trap 'docker rm -f rumi-loong-rel >/dev/null 2>&1 || true' EXIT

docker cp /tmp/loong-rel-build.sh rumi-loong-rel:/tmp/loong-rel-build.sh
docker exec -e LABEL="$LABEL" rumi-loong-rel bash /tmp/loong-rel-build.sh

TARBALL="$OUT/${STAGE}.tar.gz"
ls -la "$TARBALL"
echo "== 上传到 release $TAG =="
gh release upload "$TAG" "$TARBALL" --clobber -R "$REPO"
echo "完成: ${STAGE}.tar.gz → $TAG"
