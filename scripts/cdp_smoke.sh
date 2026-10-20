#!/usr/bin/env bash
# webclaw CDP 冒烟测试（无 Playwright）：无头 Chrome + remote debugging + HTTP/WS
# 用法（在飞书智能伙伴 Debian 容器内）：
#   bash cdp_smoke.sh
#   bash cdp_smoke.sh --install   # 尝试 apt 安装 chromium
#   PORT=9333 bash cdp_smoke.sh
set -euo pipefail

PORT="${PORT:-9222}"
PROFILE="${PROFILE:-$HOME/.webclaw-cdp-smoke}"
DO_INSTALL=0
for a in "$@"; do
  case "$a" in
    --install) DO_INSTALL=1 ;;
  esac
done

log() { printf '[cdp-smoke] %s\n' "$*"; }
fail() { log "FAIL: $*"; exit 1; }

find_browser() {
  local c
  for c in \
    google-chrome-stable google-chrome chromium chromium-browser \
    /usr/bin/google-chrome-stable /usr/bin/chromium /usr/bin/chromium-browser \
    "$HOME/.cache/ms-playwright/chromium-"*/chrome-linux*/chrome
  do
    # 最后一项可能是 glob；无匹配时跳过
    if [[ "$c" == *'*'* ]]; then
      # shellcheck disable=SC2086
      for g in $c; do
        [[ -x "$g" ]] && { echo "$g"; return 0; }
      done
      continue
    fi
    if command -v "$c" >/dev/null 2>&1; then
      command -v "$c"
      return 0
    fi
    [[ -x "$c" ]] && { echo "$c"; return 0; }
  done
  return 1
}

if [[ "$DO_INSTALL" -eq 1 ]]; then
  log "尝试 apt 安装 chromium…"
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq chromium || \
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq chromium-browser || true
fi

BROWSER="$(find_browser || true)"
[[ -n "${BROWSER:-}" ]] || fail "未找到 Chrome/Chromium。请执行: bash cdp_smoke.sh --install  或自行安装 chromium"

log "浏览器: $BROWSER"
mkdir -p "$PROFILE"
pkill -f "remote-debugging-port=${PORT}" 2>/dev/null || true
sleep 0.5

# 容器常见：无沙箱、无 GPU；与 webclaw headless 参数对齐
ARGS=(
  --headless=new
  --remote-debugging-port="${PORT}"
  --remote-allow-origins=*
  --user-data-dir="${PROFILE}"
  --no-first-run
  --no-default-browser-check
  --disable-dev-shm-usage
  --disable-gpu
  --no-sandbox
  --window-size=1280,720
  about:blank
)

log "启动 CDP 端口 ${PORT}…"
"${BROWSER}" "${ARGS[@]}" >/tmp/cdp-smoke-chrome.log 2>&1 &
CHROME_PID=$!
cleanup() {
  kill "$CHROME_PID" 2>/dev/null || true
  wait "$CHROME_PID" 2>/dev/null || true
}
trap cleanup EXIT

ok=0
for i in $(seq 1 40); do
  if curl -fsS "http://127.0.0.1:${PORT}/json/version" >/tmp/cdp-smoke-version.json 2>/dev/null; then
    ok=1
    break
  fi
  sleep 0.25
done
[[ "$ok" -eq 1 ]] || {
  log "Chrome 日志尾部:"
  tail -n 40 /tmp/cdp-smoke-chrome.log || true
  fail "HTTP http://127.0.0.1:${PORT}/json/version 无响应"
}

log "=== /json/version ==="
cat /tmp/cdp-smoke-version.json
echo

WS_URL="$(python3 - <<'PY'
import json
print(json.load(open("/tmp/cdp-smoke-version.json"))["webSocketDebuggerUrl"])
PY
)"
log "WebSocket: $WS_URL"

# Node 22 原生 WebSocket：连 Browser 级端点，创建目标页并 evaluate
node --input-type=module - <<NODE
import fs from "fs";

const port = process.env.PORT || "9222";
const ver = JSON.parse(fs.readFileSync("/tmp/cdp-smoke-version.json", "utf8"));
const browserWs = ver.webSocketDebuggerUrl;
if (!browserWs) {
  console.error("no webSocketDebuggerUrl");
  process.exit(1);
}

function rpc(ws, id, method, params = {}) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error("CDP timeout: " + method)), 15000);
    const onMsg = (ev) => {
      let msg;
      try { msg = JSON.parse(ev.data); } catch { return; }
      if (msg.id !== id) return;
      clearTimeout(t);
      ws.removeEventListener("message", onMsg);
      if (msg.error) reject(new Error(JSON.stringify(msg.error)));
      else resolve(msg.result);
    };
    ws.addEventListener("message", onMsg);
    ws.send(JSON.stringify({ id, method, params }));
  });
}

const ws = new WebSocket(browserWs);
await new Promise((res, rej) => {
  ws.addEventListener("open", res);
  ws.addEventListener("error", () => rej(new Error("WS open failed")));
});

const { targetId } = await rpc(ws, 1, "Target.createTarget", { url: "about:blank" });
const { sessionId } = await rpc(ws, 2, "Target.attachToTarget", {
  targetId,
  flatten: true,
});

const evalRes = await new Promise((resolve, reject) => {
  const id = 3;
  const t = setTimeout(() => reject(new Error("evaluate timeout")), 15000);
  const onMsg = (ev) => {
    let msg;
    try { msg = JSON.parse(ev.data); } catch { return; }
    if (msg.sessionId !== sessionId || msg.id !== id) return;
    clearTimeout(t);
    ws.removeEventListener("message", onMsg);
    if (msg.error) reject(new Error(JSON.stringify(msg.error)));
    else resolve(msg.result);
  };
  ws.addEventListener("message", onMsg);
  ws.send(JSON.stringify({
    id,
    sessionId,
    method: "Runtime.evaluate",
    params: {
      expression: "({ ua: navigator.userAgent, href: location.href, ok: true })",
      returnByValue: true,
      awaitPromise: false,
    },
  }));
});

const v = evalRes?.result?.value;
console.log("[cdp-smoke] Runtime.evaluate =>", JSON.stringify(v));
if (!v?.ok) {
  console.error("evaluate missing ok");
  process.exit(1);
}

ws.close();
console.log("[cdp-smoke] PASS: CDP HTTP + WebSocket + Runtime.evaluate 正常");
NODE

log "完成。若以上为 PASS，则本空间可用纯 CDP（与 webclaw 同协议）。"
log "注意：容器无 GUI，须 --headless=new；webclaw 迁入时走无头 + 飞书做人机交互。"
