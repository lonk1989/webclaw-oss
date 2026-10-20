---
name: webclaw
description: >-
  豆包工作伙伴 Debian 空间：在 $AILY_WORKDIR 安装 webclaw-worker、用 6 位任务码 pair-redeem 按需短命拉起 host；
  可查状态/停止/更新/日志。日常任务由本机「远程执行」经中继直达；跑完自动退出，不常驻。
metadata:
  openclaw:
    os: ["linux"]
    requires:
      anyBins: ["webclaw-worker", "curl"]
    emoji: "🦞"
---

# webclaw（豆包工作伙伴空间）

> **本文件是什么**  
> OpenClaw / 伙伴空间侧的 **Skill 入口**（YAML 头 + 编排说明）。平台按 `name`/`description` 匹配何时启用本 Skill，并按下文指挥助手在 `$AILY_WORKDIR` 里执行安装与运维。  
> **不是**发给 C 端用户的那份说明书。用户拿到的是本机一键复制的 `豆包伙伴webclaw远程服务初始化.md`（源文件 `companion-skill.md`）。开发者把本目录整包挂进伙伴技能库即可；日常改行为优先改 `companion-skill.md`（用户会复制它），本文件与之保持口径一致。

本 Skill **只做编排**：在 **`$AILY_WORKDIR`** 安装 worker、**用 6 位码 `pair-redeem` 按需拉起短命 host**、**回答状态 / 停止 / 更新 / 日志**。  
host **不常驻**：接一单后退出并关闭 19222 浏览器。日常任务主路径是本机 **「远程执行」**。

## 何时使用

- 「按说明书初始化」「重新初始化」
- 用户口播/粘贴 **6 位任务码**（含继续任务码）
- 「查看服务状态」「在线吗」「状态怎样」
- 「停止当前服务」「停掉」
- 「更新程序版本」「升级」
- 「查看最近一次执行日志」「看日志」
- 兜底：旧分文件 / `.webclawjob`

用户只说「导出考勤」等日常差事 → 提示去本机点「远程执行」并把任务码发给你。

## 目录与环境

| 路径 / 变量 | 说明 |
|-------------|------|
| `$AILY_WORKDIR` | 当前任务工作目录（唯一根） |
| `WEBCLAW_HOME=$AILY_WORKDIR` | worker / host 数据根 |
| `$AILY_WORKDIR/bin/webclaw-worker` | worker + 短命 host |
| `$AILY_WORKDIR/host-pair.json` | 宿主机配对 |
| `WEBCLAW_CDP_PORT` | 默认 **19222**；禁止动平台 9222 |

```bash
: "${AILY_WORKDIR:?缺少 AILY_WORKDIR}"
export WEBCLAW_HOME="$AILY_WORKDIR"
export WEBCLAW_CHROME="${WEBCLAW_CHROME:-/usr/bin/chromium-browser}"
export WEBCLAW_CDP_PORT="${WEBCLAW_CDP_PORT:-19222}"
export WEBCLAW_CONTAINER=1
export PATH="$WEBCLAW_HOME/bin:$PATH"
```

## 安装

```bash
bash {baseDir}/scripts/install_worker.sh
"$WEBCLAW_HOME/bin/webclaw-worker" host stop >/dev/null 2>&1 || true
```

## 任务码接通（每次远程执行 / 继续执行）

**拆成短命令**，禁止 `pair-redeem` + `sleep` 轮询写在同一条工具调用里。  
`pair-redeem` 工具 **timeout = 10**。沙箱因 host 仍在进程树里报 timeout 是**预期现象**；随后 `host status` 为 running 即算接通成功，勿重复兑换。

```bash
# 1）只兑换（timeout=10；超时可忽略，以 status 为准）
"$WEBCLAW_HOME/bin/webclaw-worker" pair-redeem --code 123456

# 2）另一次短调用再查（可重复多次，每次单独调用）
"$WEBCLAW_HOME/bin/webclaw-worker" host status
# 或：bash {baseDir}/scripts/ensure_host.sh check
```

ONLINE（`phase=running`）后停。跑完 host 会自己退出；`idle` 是正常空闲。

**禁止** bash `setsid` / `nohup` / `host &` / 同命令内 `for…sleep`。  
仅当兑换后多次仍无进程：才允许一次 `webclaw-worker host --daemon`。

## 运维四口令

| 用户说 | 命令 |
|--------|------|
| 查看服务状态 | `"$WEBCLAW_HOME/bin/webclaw-worker" host status` |
| 停止当前服务 | `"$WEBCLAW_HOME/bin/webclaw-worker" host stop` |
| 更新程序版本 | `"$WEBCLAW_HOME/bin/webclaw-worker" update` |
| 查看最近一次执行日志 | `"$WEBCLAW_HOME/bin/webclaw-worker" last-log --lines 80` |

也可用：`ensure_host.sh check|stop`。

## 兜底：任务包

```bash
"$WEBCLAW_HOME/bin/webclaw-worker" job --file /path/to/task.webclawjob
# 看守到 done 后：
"$WEBCLAW_HOME/bin/webclaw-worker" host stop
```

## 话术约定

1. 初始化 → 只装程序，不常驻；说准备好了并停
2. 6 位码 → `pair-redeem`；ONLINE 后停
3. 四口令 → 上表；用人话回报，不念路径/端口
4. 日常差事 → 提示本机「远程执行」+ 发任务码
5. 禁止常驻 / bash setsid / 打开密文 / 念 secret

## 禁止事项

- 不要 `pkill` 乱杀 / 占用 **9222**
- 不要并行起两个 worker；不要常驻 host
- 不要装到 `$HOME/webclaw` 等其它目录
- 不要打开/转发加密包内容
- 不要在对话里要网站密码
