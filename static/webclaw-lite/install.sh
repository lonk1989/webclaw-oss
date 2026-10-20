#!/usr/bin/env bash
# webclaw-lite macOS/Linux 安装
set -euo pipefail

BASE="${WEBCLAW_LITE_BASE:-https://zhjmessaastest.clean-pro.cn:1081/agent/webclaw-server/webclaw-lite}"
BASE="${BASE%/}"
HOME_DIR="${WEBCLAW_HOME:-$HOME/.webclaw}"
BIN_DIR="$HOME_DIR/bin"
mkdir -p "$BIN_DIR" "$HOME_DIR/runs" "$HOME_DIR/recipes"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$OS-$ARCH" in
  darwin-arm64|darwin-aarch64) FILE="webclaw-lite-macos-arm64" ;;
  darwin-x86_64) FILE="webclaw-lite-macos-x64" ;;
  linux-x86_64|linux-amd64) FILE="webclaw-lite-linux-x64" ;;
  *) echo "不支持的平台: $OS $ARCH" >&2; exit 2 ;;
esac

URL="${WEBCLAW_WORKER_URL:-$BASE/release/$FILE}"
DEST="$BIN_DIR/webclaw-lite"
TMP="$BIN_DIR/webclaw-lite.download.$$"

echo "[webclaw-lite] download $URL"
curl -fsSL --connect-timeout 30 -o "$TMP" "$URL"
chmod +x "$TMP"

if [[ -f "$DEST" ]]; then
  mv -f "$DEST" "$DEST.bak" 2>/dev/null || true
fi
mv -f "$TMP" "$DEST"
chmod +x "$DEST"

export WEBCLAW_HOME="$HOME_DIR"
"$DEST" install
"$DEST" status
echo "[webclaw-lite] installed -> $DEST"
