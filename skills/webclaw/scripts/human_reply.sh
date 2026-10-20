#!/usr/bin/env bash
# 将飞书用户回复写入 worker 人机交换目录
set -euo pipefail

RUN_DIR=""
TEXT=""
OK=0
CANCEL=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-dir) RUN_DIR="$2"; shift 2 ;;
    --text) TEXT="$2"; shift 2 ;;
    --ok) OK=1; shift ;;
    --cancel) CANCEL=1; shift ;;
    -h|--help)
      echo "Usage: human_reply.sh --run-dir <run> (--text CODE | --ok | --cancel)"
      exit 0
      ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$RUN_DIR" ]]; then
  echo "需要 --run-dir" >&2
  exit 2
fi

HUMAN="${WEBCLAW_HUMAN_DIR:-$RUN_DIR/human}"
mkdir -p "$HUMAN"

if [[ "$CANCEL" -eq 1 ]]; then
  printf '%s\n' '{"cancel":true}' >"$HUMAN/human_reply.json"
  echo "[webclaw] wrote cancel -> $HUMAN/human_reply.json"
  exit 0
fi

if [[ "$OK" -eq 1 ]]; then
  printf '%s\n' '{"ok":true}' >"$HUMAN/human_reply.json"
  echo "[webclaw] wrote ok -> $HUMAN/human_reply.json"
  exit 0
fi

if [[ -z "$TEXT" ]]; then
  echo "需要 --text / --ok / --cancel" >&2
  exit 2
fi

# 图形验证码：纯文本写 human_reply.txt（worker 优先认 txt 也可认 json text）
printf '%s\n' "$TEXT" >"$HUMAN/human_reply.txt"
echo "[webclaw] wrote text -> $HUMAN/human_reply.txt"
