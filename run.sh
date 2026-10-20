#!/usr/bin/env bash
# 启动 webclaw 客户端（macOS / Linux）
set -euo pipefail
cd "$(dirname "$0")/src-tauri"
exec cargo run "$@"
