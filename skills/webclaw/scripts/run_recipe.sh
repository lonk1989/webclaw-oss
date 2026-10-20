#!/usr/bin/env bash
# 拉起 webclaw-worker，解析 JSONL，处理 need_human，结束后上传产物
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR="${HOME:-/home/gem}"
if [[ -n "${AILY_WORKDIR:-}" ]]; then
  export WEBCLAW_HOME="${WEBCLAW_HOME:-$AILY_WORKDIR}"
else
  export WEBCLAW_HOME="${WEBCLAW_HOME:-$HOME_DIR/webclaw}"
fi
export WEBCLAW_CDP_PORT="${WEBCLAW_CDP_PORT:-19222}"
export WEBCLAW_CONTAINER="${WEBCLAW_CONTAINER:-1}"
export WEBCLAW_CHROME="${WEBCLAW_CHROME:-/usr/bin/chromium-browser}"
export PATH="$WEBCLAW_HOME/bin:${PATH}"

RECIPE=""
JOB=""
TITLE="worker"
PARAMS=""
STDIN=0
WORKER="${WEBCLAW_WORKER_BIN:-webclaw-worker}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --recipe|-r) RECIPE="$2"; shift 2 ;;
    --job|-j) JOB="$2"; shift 2 ;;
    --title|-t) TITLE="$2"; shift 2 ;;
    --params|-p) PARAMS="$2"; shift 2 ;;
    --stdin) STDIN=1; shift ;;
    --worker) WORKER="$2"; shift 2 ;;
    -h|--help)
      echo "Usage: run_recipe.sh (--recipe FILE | --job FILE.webclawjob | --stdin) [--title T] [--params JSON]"
      exit 0
      ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

if ! command -v "$WORKER" >/dev/null 2>&1; then
  echo "[webclaw] 未找到 $WORKER，尝试安装…"
  bash "$SCRIPT_DIR/install_worker.sh"
fi

mkdir -p "$WEBCLAW_HOME/runs"

if [[ -n "$JOB" ]]; then
  if [[ "$STDIN" -eq 1 || -n "$RECIPE" ]]; then
    echo "--job 不能与 --recipe / --stdin 同时使用" >&2
    exit 2
  fi
  CMD=("$WORKER" job --file "$JOB")
else
  CMD=("$WORKER" run --title "$TITLE")
  if [[ -n "$PARAMS" ]]; then
    CMD+=(--params "$PARAMS")
  fi
  if [[ "$STDIN" -eq 1 ]]; then
    CMD+=(--stdin)
  elif [[ -n "$RECIPE" ]]; then
    CMD+=(--recipe "$RECIPE")
  else
    echo "需要 --recipe、--job 或 --stdin" >&2
    exit 2
  fi
fi

echo "[webclaw] CDP port=$WEBCLAW_CDP_PORT chrome=$WEBCLAW_CHROME home=$WEBCLAW_HOME"
echo "[webclaw] exec: ${CMD[*]}"

RUN_DIR=""
LAST_NEED=""

# 逐行读 JSONL；need_human 时通知飞书并阻塞等回复（由外层会话写 human_reply，或本脚本轮询）
handle_line() {
  local line="$1"
  [[ -z "$line" ]] && return 0
  echo "$line"

  local typ
  typ="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('type',''))" "$line" 2>/dev/null || true)"

  case "$typ" in
    run_start)
      if command -v lark-cli >/dev/null 2>&1; then
        local rid title
        rid="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('run_id',''))" "$line")"
        title="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('title',''))" "$line")"
        lark-cli im +send --text "开始执行「${title}」run=${rid}" 2>/dev/null || true
      fi
      ;;
    step_ok|step_fail)
      if command -v lark-cli >/dev/null 2>&1; then
        local idx total detail
        idx="$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(d.get('index',''))" "$line")"
        total="$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(d.get('total',''))" "$line")"
        if [[ "$typ" == step_ok ]]; then
          detail="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('detail','')[:120])" "$line")"
          lark-cli im +send --text "✓ ${idx}/${total} ${detail}" 2>/dev/null || true
        else
          detail="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('error','')[:200])" "$line")"
          lark-cli im +send --text "✗ ${idx}/${total} ${detail}" 2>/dev/null || true
        fi
      fi
      ;;
    need_human)
      LAST_NEED="$line"
      echo ""
      echo "======== NEED_HUMAN：立刻把验证码图发到当前对话 ========"
      echo "$line"
      local img human_root reply inbox
      img="$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(d.get('image_path') or d.get('inbox_image') or '')" "$line" 2>/dev/null || true)"
      reply="$(python3 -c "import json,sys; d=json.loads(sys.argv[1]); print(d.get('reply_file') or '')" "$line" 2>/dev/null || true)"
      inbox="${WEBCLAW_HOME}/last-captcha.png"
      if [[ -z "$img" && -f "$inbox" ]]; then
        img="$inbox"
      fi
      if [[ -n "$reply" ]]; then
        human_root="$(dirname "$reply")"
      elif [[ -n "${WEBCLAW_HUMAN_DIR:-}" ]]; then
        human_root="$WEBCLAW_HUMAN_DIR"
      elif [[ -n "$img" && "$img" != "$inbox" ]]; then
        human_root="$(dirname "$img")"
      else
        human_root=""
      fi
      echo "[webclaw] 图片文件: ${img:-无}"
      echo "[webclaw] 用户回复后写入: ${reply:-$human_root/human_reply.txt}"
      echo "========================================================="
      if [[ -n "$human_root" ]]; then
        bash "$SCRIPT_DIR/notify_human.sh" --run-dir "$(dirname "$human_root")" || true
        echo "[webclaw] 等待人机回复（最多 10 分钟）。助手必须先把图片发到飞书会话。"
        local n=0
        while [[ $n -lt 600 ]]; do
          if [[ -f "$human_root/human_reply.txt" || -f "$human_root/human_reply.json" ]]; then
            echo "[webclaw] 检测到人机回复"
            break
          fi
          sleep 1
          n=$((n + 1))
        done
      fi
      ;;
    done)
      RUN_DIR="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('run_dir') or '')" "$line" 2>/dev/null || true)"
      local err
      err="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('error') or '')" "$line" 2>/dev/null || true)"
      if [[ -n "$RUN_DIR" ]]; then
        bash "$SCRIPT_DIR/upload_artifacts.sh" --run-dir "$RUN_DIR" || true
      fi
      if command -v lark-cli >/dev/null 2>&1; then
        if [[ -n "$err" ]]; then
          lark-cli im +send --text "任务结束（失败）: $err
目录: $RUN_DIR" 2>/dev/null || true
        else
          lark-cli im +send --text "任务完成。目录: $RUN_DIR" 2>/dev/null || true
        fi
      fi
      ;;
  esac
}

# worker 在 run 开始后会设 WEBCLAW_HUMAN_DIR；子进程继承我们预导出的 HOME 即可
set +e
if [[ "$STDIN" -eq 1 ]]; then
  "${CMD[@]}" 2> >(while IFS= read -r err; do echo "[stderr] $err" >&2; done) \
    | while IFS= read -r line; do handle_line "$line"; done
  EC=${PIPESTATUS[0]}
else
  "${CMD[@]}" 2> >(while IFS= read -r err; do echo "[stderr] $err" >&2; done) \
    | while IFS= read -r line; do handle_line "$line"; done
  EC=${PIPESTATUS[0]}
fi
set -e
exit "${EC:-0}"
