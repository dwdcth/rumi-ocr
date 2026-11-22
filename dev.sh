#!/usr/bin/env bash
# dev.sh — Rumi OCR 开发辅助脚本
# 用法:
#   ./dev.sh           → debug 构建并运行
#   ./dev.sh --release → release 构建并运行
#   ./dev.sh --build   → 仅构建（debug），不运行
#   ./dev.sh --build --release → 仅构建（release），不运行
#   ./dev.sh --watch   → 文件变动时自动重新构建（需要 cargo-watch）
#   ./dev.sh --ui      → 仅编译 ui crate（快速检查）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

RELEASE=false
BUILD_ONLY=false
WATCH=false
UI_ONLY=false

for arg in "$@"; do
  case "$arg" in
    --release)    RELEASE=true ;;
    --build)      BUILD_ONLY=true ;;
    --watch)      WATCH=true ;;
    --ui)         UI_ONLY=true ;;
    -h|--help)
      head -11 "$0" | tail -10
      exit 0
      ;;
  esac
done

CARGO_FLAGS=""
$RELEASE && CARGO_FLAGS="--release"

BIN_PATH="$SCRIPT_DIR/target/debug/rumi-ui"
$RELEASE && BIN_PATH="$SCRIPT_DIR/target/release/rumi-ui"

if $WATCH; then
  if ! command -v cargo-watch &>/dev/null; then
    echo "需要 cargo-watch：cargo install cargo-watch"
    exit 1
  fi
  echo "→ 监听文件变动，自动重新构建 ui..."
  exec cargo watch -x "build -p rumi-ui $CARGO_FLAGS"
fi

if $UI_ONLY; then
  echo "→ 编译 rumi-ui ..."
  exec cargo build -p rumi-ui $CARGO_FLAGS
fi

echo "→ 构建 rumi-ui ($( $RELEASE && echo release || echo debug ))..."
cargo build -p rumi-ui $CARGO_FLAGS

echo "→ 二进制: $BIN_PATH"
ls -lh "$BIN_PATH"

if ! $BUILD_ONLY; then
  echo "→ 启动..."
  exec "$BIN_PATH"
fi
