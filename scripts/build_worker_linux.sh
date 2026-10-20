#!/usr/bin/env bash
# 产出可在伙伴空间（Ubuntu 22.04 / GLIBC 2.35）运行的 webclaw-worker。
# 用法: ./scripts/build_worker_linux.sh [out_dir]
#
# 必须在 Ubuntu 22.04 上编（或 Docker）。在 24.04 上裸编会要求 GLIBC 2.39，伙伴空间跑不起来。
# 且必须 --no-default-features，避免链入 WebKit/GTK。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/dist}"
mkdir -p "$OUT"

BIN_NAME="webclaw-worker"
CARGO_FLAGS=(build --release --bin "$BIN_NAME" --no-default-features)

build_in_docker() {
  echo "[build] Docker ubuntu:22.04（GLIBC 2.35，匹配伙伴空间）"
  docker build -f "$ROOT/Dockerfile.worker" -t webclaw-worker-builder "$ROOT"
  docker run --rm \
    -v "$ROOT:/src" \
    -w /src/src-tauri \
    webclaw-worker-builder \
    cargo "${CARGO_FLAGS[@]}"
}

build_native() {
  echo "[build] native --no-default-features"
  if [[ -f /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    echo "[build] host: ${PRETTY_NAME:-unknown}"
    if [[ "${VERSION_ID:-}" != "22.04" ]]; then
      echo "[build] 警告: 非 Ubuntu 22.04 编出的二进制可能无法在伙伴空间运行（GLIBC 过高）。" >&2
      echo "[build] 请安装 Docker 后重跑，或在 22.04 机器上构建。" >&2
    fi
  fi
  (cd "$ROOT/src-tauri" && cargo "${CARGO_FLAGS[@]}")
}

if command -v docker >/dev/null 2>&1; then
  build_in_docker
  SRC="$ROOT/src-tauri/target/release/$BIN_NAME"
else
  build_native
  SRC="$ROOT/src-tauri/target/release/$BIN_NAME"
fi

DEST="$OUT/webclaw-worker-linux-x64"
cp -f "$SRC" "$DEST"
chmod +x "$DEST"

# 兼容发布目录文件名 webclaw-worker
cp -f "$DEST" "$OUT/webclaw-worker"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$OUT" && sha256sum "webclaw-worker-linux-x64" | tee "webclaw-worker-linux-x64.sha256" > "webclaw-worker.sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "$OUT" && shasum -a 256 "webclaw-worker-linux-x64" | tee "webclaw-worker-linux-x64.sha256" > "webclaw-worker.sha256")
fi

VER="$(grep -m1 '^version' "$ROOT/src-tauri/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')"
PUBLISHED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
DIGEST="$(awk '{print $1}' "$OUT/webclaw-worker-linux-x64.sha256" 2>/dev/null || echo "")"
SIZE="$(wc -c <"$DEST" | tr -d ' ')"

cat >"$OUT/latest-worker-linux.json" <<EOF
{
  "version": "$VER",
  "notes": "headless CDP worker; Ubuntu 22.04 / GLIBC 2.35; no WebKit",
  "publishedAt": "$PUBLISHED",
  "platform": "linux",
  "arch": "x86_64",
  "glibc": "2.35",
  "file": "webclaw-worker",
  "sha256": "$DIGEST",
  "size": $SIZE
}
EOF

echo "[build] -> $DEST"
echo "[build] -> $OUT/webclaw-worker"
echo "[build] 上传 webclaw-worker（及 .sha256）到发布目录后，助手即可重新初始化。"
