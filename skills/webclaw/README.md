# webclaw OpenClaw Skill

**发给 C 端用户的只有一份说明书：** 本机「设置 → 飞书与伙伴」或「远程执行」确认框一键复制的 `豆包伙伴webclaw远程服务初始化.md`（源文件 [companion-skill.md](./companion-skill.md)）。  
用户粘贴发给飞书豆包工作伙伴即可完成初始化；**每次本机「远程执行」都会弹出 6 位任务码**，发给伙伴后短命 host 接单。长任务若伙伴提前退出，本机会再给 **继续任务码**（同样兑换即可从剩余步骤接着跑，并沿用同一产物目录与浏览器）。

| 文件 | 给谁 | 作用 |
|------|------|------|
| [companion-skill.md](./companion-skill.md) | 经本机一键复制 → 发到飞书对话 | C 端唯一说明书（初始化 + 四口令运维） |
| [SKILL.md](./SKILL.md) | 伙伴空间 / OpenClaw 技能库 | Skill 元数据 + 编排入口；**不要**当用户说明书导出 |

脚本：

| 脚本 | 作用 |
|------|------|
| `scripts/install_worker.sh` | 下载 / 更新 `webclaw-worker` 到 `$AILY_WORKDIR/bin` |
| `scripts/ensure_host.sh` | `check`/`status`/`start`/`stop`（禁止 bash 后台拉起） |
| `scripts/run_recipe.sh` | 跑菜谱、解析 JSONL、通知人机、上传产物 |
| `scripts/human_reply.sh` | 写回验证码 / 人机确认 |
| `scripts/notify_human.sh` | 把 `waiting_human` 发到飞书 |
| `scripts/upload_artifacts.sh` | 上传 downloads/documents |

内置运维命令（新版 worker）：`host status` / `host stop` / `update` / `last-log`。
