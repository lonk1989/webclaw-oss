#!/usr/bin/env bash
# 读取 waiting_human.json，经 lark-cli（若可用）通知飞书用户
set -euo pipefail

RUN_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-dir) RUN_DIR="$2"; shift 2 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

HUMAN="${WEBCLAW_HUMAN_DIR:-${RUN_DIR:+$RUN_DIR/human}}"
WAITING="${HUMAN}/waiting_human.json"
IMG="${HUMAN}/captcha.png"

if [[ ! -f "$WAITING" ]]; then
  echo "[webclaw] 无 waiting_human.json: $WAITING" >&2
  exit 1
fi

HINT="$(python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print(d.get('hint') or d.get('kind') or '需要人工')" "$WAITING" 2>/dev/null || echo "需要人工协助")"
KIND="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('kind',''))" "$WAITING" 2>/dev/null || true)"

MSG="【webclaw 人机】kind=${KIND}
${HINT}

请回复验证码文字；人机完成请回复「完成」或「ok」；取消请回复「取消」。
写回目录: ${HUMAN}"

echo "$MSG"

if command -v lark-cli >/dev/null 2>&1; then
  if [[ -f "$IMG" ]]; then
    lark-cli im message send --help >/dev/null 2>&1 || true
    # 尽力发送：不同版本子命令可能不同，失败不阻断
    lark-cli im +send --text "$MSG" 2>/dev/null \
      || lark-cli message send --text "$MSG" 2>/dev/null \
      || echo "[webclaw] lark-cli 发文本失败，请手动把上方提示发到飞书" >&2
    lark-cli im +send --image "$IMG" 2>/dev/null \
      || lark-cli message send --image "$IMG" 2>/dev/null \
      || echo "[webclaw] 请手动发送图片: $IMG" >&2
  else
    lark-cli im +send --text "$MSG" 2>/dev/null \
      || lark-cli message send --text "$MSG" 2>/dev/null \
      || true
  fi
else
  echo "[webclaw] 未找到 lark-cli；请把提示与图片发到飞书会话"
  [[ -f "$IMG" ]] && echo "[webclaw] 图片: $IMG"
fi

if [[ "$KIND" == "slider" ]]; then
  echo "[webclaw] 警告: 滑块类远程暂不支持纯文本完成。建议 Cookie 预登录或本机执行。" >&2
fi
