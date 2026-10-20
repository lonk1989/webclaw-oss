#!/usr/bin/env bash
# 按需短命 host：默认 check；start= host --daemon；stop= host stop
# 禁止 bash setsid/nohup 拉起 host（伙伴沙箱必超时失败）。
set -euo pipefail

: "${AILY_WORKDIR:?缺少 AILY_WORKDIR}"
export WEBCLAW_HOME="${WEBCLAW_HOME:-$AILY_WORKDIR}"
export PATH="$WEBCLAW_HOME/bin:${PATH}"

WORKER="$WEBCLAW_HOME/bin/webclaw-worker"
LOG="$WEBCLAW_HOME/host.log"
MODE="${1:-check}"

if [[ ! -x "$WORKER" ]]; then
  echo "[ensure_host] missing worker" >&2
  exit 2
fi

case "$MODE" in
  status|check)
    if out="$("$WORKER" host status 2>/dev/null)"; then
      echo "$out"
      if echo "$out" | grep -q '"phase":"running"'; then
        echo "[ensure_host] online"
        exit 0
      fi
      if echo "$out" | grep -q '"phase":"starting"'; then
        echo "[ensure_host] starting_or_connecting"
        exit 7
      fi
      if echo "$out" | grep -q '"phase":"need_pair"'; then
        echo "[ensure_host] need_pair"
        exit 3
      fi
      echo "[ensure_host] not_running"
      exit 8
    fi
    # 旧版 worker 无 host status 时回退
    if [[ -f "$LOG" ]] && grep -qiE 'HOST_AUTH_FAILED|host auth failed|host_hello 失败|连接运行中继失败' "$LOG"; then
      echo "[ensure_host] auth_failed"
      exit 5
    fi
    if [[ -f "$LOG" ]] && grep -q '\[host\] online' "$LOG"; then
      echo "[ensure_host] log_online_maybe_stale"
      exit 4
    fi
    echo "[ensure_host] not_running"
    exit 8
    ;;
  start)
    if [[ ! -f "$WEBCLAW_HOME/host-pair.json" ]]; then
      echo "[ensure_host] missing host-pair.json" >&2
      exit 3
    fi
    "$WORKER" host --daemon
    ;;
  stop)
    "$WORKER" host stop
    ;;
  *)
    echo "usage: $0 status|check|start|stop" >&2
    exit 1
    ;;
esac
