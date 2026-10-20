#!/usr/bin/env bash
# 安装 / 更新 webclaw-worker 到 $WEBCLAW_HOME/bin/
# 布局：webclaw-worker（loader 长驻）+ webclaw-worker.bin（实际二进制，update 只换它）
# 优先：当前任务工作目录 $AILY_WORKDIR
set -euo pipefail

HOME_DIR="${HOME:-/home/gem}"
if [[ -n "${AILY_WORKDIR:-}" ]]; then
  WEBCLAW_HOME="${WEBCLAW_HOME:-$AILY_WORKDIR}"
else
  WEBCLAW_HOME="${WEBCLAW_HOME:-$HOME_DIR/webclaw}"
fi
BIN_DIR="$WEBCLAW_HOME/bin"
mkdir -p "$BIN_DIR"

# 默认从发布目录拉取；可由环境变量覆盖（内网 / 飞书云盘直链）
DEFAULT_URL="${WEBCLAW_WORKER_URL:-https://zhjmessaastest.clean-pro.cn:1081/agent/webclaw-server/release/webclaw-worker}"
SHA_URL="${WEBCLAW_WORKER_SHA_URL:-${DEFAULT_URL}.sha256}"
REAL="$BIN_DIR/webclaw-worker.bin"
LOADER="$BIN_DIR/webclaw-worker"

echo "[webclaw] WEBCLAW_HOME=$WEBCLAW_HOME"
echo "[webclaw] download: $DEFAULT_URL"

TMP="$(mktemp)"
TMP_SHA="$(mktemp)"
cleanup() { rm -f "$TMP" "$TMP_SHA"; }
trap cleanup EXIT

if ! curl -fsSL --connect-timeout 30 -o "$TMP" "$DEFAULT_URL"; then
  echo "[webclaw] 下载失败。请设置 WEBCLAW_WORKER_URL 为可达地址（飞书云盘/内网）。" >&2
  exit 1
fi

if curl -fsSL --connect-timeout 15 -o "$TMP_SHA" "$SHA_URL" 2>/dev/null; then
  expect="$(awk '{print $1}' "$TMP_SHA" | head -n1)"
  if command -v sha256sum >/dev/null 2>&1 && [[ -n "$expect" ]]; then
    got="$(sha256sum "$TMP" | awk '{print $1}')"
    if [[ "$got" != "$expect" ]]; then
      echo "[webclaw] sha256 不匹配: got=$got expect=$expect" >&2
      exit 1
    fi
    echo "[webclaw] sha256 ok"
  fi
else
  echo "[webclaw] 未找到 .sha256，跳过校验"
fi

# 若入口仍是旧版整包 ELF，先挪成 .bin 再写 loader
if [[ -f "$LOADER" ]] && ! head -c 2 "$LOADER" | grep -q '#!'; then
  mv -f "$LOADER" "$REAL.legacy" 2>/dev/null || true
fi

install -m 755 "$TMP" "$REAL"
printf '%s\n' '#!/bin/sh' 'exec "$(dirname "$0")/webclaw-worker.bin" "$@"' > "$LOADER"
chmod 755 "$LOADER"
rm -f "$REAL.legacy"

echo "[webclaw] installed: $LOADER -> $REAL"
"$LOADER" --help 2>&1 | head -n 5 || true

# 持久化到 ~/.webclaw/bin（workdir 会被清时仍可找回）
PERSIST="$HOME_DIR/.webclaw/bin"
mkdir -p "$PERSIST"
cp -f "$REAL" "$PERSIST/webclaw-worker.bin"
cp -f "$LOADER" "$PERSIST/webclaw-worker"
chmod 755 "$PERSIST/webclaw-worker" "$PERSIST/webclaw-worker.bin"
echo "[webclaw] persistent: $PERSIST/webclaw-worker"

# 方便 PATH
if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
  echo "[webclaw] 建议: export PATH=\"$BIN_DIR:\$PATH\""
fi
