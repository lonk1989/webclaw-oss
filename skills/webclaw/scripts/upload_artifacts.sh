#!/usr/bin/env bash
# 上传 run 目录下 downloads/ documents/ 到飞书云盘（尽力而为）
set -euo pipefail

RUN_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-dir) RUN_DIR="$2"; shift 2 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$RUN_DIR" || ! -d "$RUN_DIR" ]]; then
  echo "[webclaw] 无效 run-dir: $RUN_DIR" >&2
  exit 1
fi

upload_dir() {
  local d="$1"
  [[ -d "$d" ]] || return 0
  local count
  count="$(find "$d" -type f 2>/dev/null | wc -l | tr -d ' ')"
  if [[ "$count" == "0" ]]; then
    return 0
  fi
  echo "[webclaw] 产物目录 $d（$count 个文件）"
  if ! command -v lark-cli >/dev/null 2>&1; then
    echo "[webclaw] 无 lark-cli，请自行取文件: $d"
    find "$d" -type f 2>/dev/null | head -n 50
    return 0
  fi
  # 不同 lark-cli 版本命令不一；逐个文件尝试
  while IFS= read -r -d '' f; do
    echo "[webclaw] upload: $f"
    lark-cli drive upload --file "$f" 2>/dev/null \
      || lark-cli docs upload --file "$f" 2>/dev/null \
      || lark-cli file upload --path "$f" 2>/dev/null \
      || echo "[webclaw] 上传失败（保留本地）: $f" >&2
  done < <(find "$d" -type f -print0 2>/dev/null)
}

upload_dir "$RUN_DIR/downloads"
upload_dir "$RUN_DIR/documents"
echo "[webclaw] artifacts done: $RUN_DIR"
