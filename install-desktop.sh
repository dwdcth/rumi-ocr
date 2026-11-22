#!/usr/bin/env bash
# install-desktop.sh — 安装 rumi-ocr.desktop 到用户启动器
# 用法: ./install-desktop.sh [--uninstall]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_SRC="$SCRIPT_DIR/rumi-ocr.desktop"
DESKTOP_DST="$HOME/.local/share/applications/rumi-ocr.desktop"
ICON_SRC="$SCRIPT_DIR/ui/assets/icon.png"
ICON_DST="$HOME/.local/share/icons/hicolor/256x256/apps/rumi-ocr.png"

install_app() {
  mkdir -p "$(dirname "$DESKTOP_DST")" "$(dirname "$ICON_DST")"
  cp "$ICON_SRC" "$ICON_DST"
  cp "$DESKTOP_SRC" "$DESKTOP_DST"
  chmod +x "$DESKTOP_DST"
  update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
  gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
  echo "✓ 已安装 rumi-ocr.desktop"
  echo "  图标: $ICON_DST"
  echo "  入口: $DESKTOP_DST"
}

uninstall_app() {
  rm -f "$DESKTOP_DST" "$ICON_DST"
  update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
  echo "✓ 已卸载 rumi-ocr.desktop"
}

case "${1:-}" in
  --uninstall) uninstall_app ;;
  *)           install_app   ;;
esac
