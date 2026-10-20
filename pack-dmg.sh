#!/usr/bin/env bash
# 将已构建的 webclaw.app 打成 DMG（不依赖 create-dmg）
# 用法: ./pack-dmg.sh [version] [arch]
#   version 默认读 src-tauri/tauri.conf.json
#   arch    默认本机（arm64→aarch64）
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"

read_version() {
  python3 - <<'PY' "$ROOT/src-tauri/tauri.conf.json"
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8")).get("version", "").strip())
PY
}

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(read_version)"
fi
if [[ -z "$VERSION" ]]; then
  echo "无法确定版本号，请传入: ./pack-dmg.sh 0.1.1" >&2
  exit 1
fi

RAW_ARCH="${2:-$(uname -m)}"
case "$RAW_ARCH" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64|amd64|x64) ARCH="x86_64" ;;
  *) ARCH="$RAW_ARCH" ;;
esac

# workspace target 与 src-tauri/target 都可能存在
APP=""
for cand in \
  "$ROOT/target/release/bundle/macos/webclaw.app" \
  "$ROOT/src-tauri/target/release/bundle/macos/webclaw.app"
do
  if [[ -d "$cand" ]]; then
    APP="$cand"
    break
  fi
done

if [[ -z "$APP" ]]; then
  echo "未找到 webclaw.app，请先执行: cd src-tauri && cargo tauri build --bundles app" >&2
  exit 1
fi

# 输出到与 .app 同级的 bundle/dmg，方便发布脚本定位
BUNDLE_ROOT="$(cd "$(dirname "$APP")/.." && pwd)"
OUT_DIR="$BUNDLE_ROOT/dmg"
OUT="$OUT_DIR/webclaw_${VERSION}_${ARCH}.dmg"

STAGE="$(mktemp -d /tmp/webclaw-dmg-XXXXXX)"
RW_DMG="/tmp/webclaw-rw-$$.dmg"
cleanup() { rm -rf "$STAGE" "$RW_DMG" 2>/dev/null || true; }
trap cleanup EXIT

mkdir -p "$OUT_DIR"
cp -R "$APP" "$STAGE/"
ln -sf /Applications "$STAGE/Applications"
codesign --force --deep --sign - "$STAGE/webclaw.app" >/dev/null 2>&1 || true

hdiutil create -volname "webclaw" -srcfolder "$STAGE" -ov -format UDRW "$RW_DMG" >/dev/null
rm -f "$OUT"
hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$OUT" >/dev/null
echo "DMG: $OUT"
