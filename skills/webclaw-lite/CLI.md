# webclaw-lite CLI contract

| Command | Purpose | Exit |
|---------|---------|------|
| `install` | Create data dirs, probe Chrome, write install marker (JSON) | 0/2 |
| `status [--run-id id]` | Health JSON (`product`, `chromeOk`, …) | 0/1 |
| `run --recipe <file>` / `run --stdin` | Execute recipe; stdout **JSONL** | 0/1/2 |
| `human-reply --text CODE` | Captcha text | 0/2 |
| `human-reply --ok` | Confirm human/slider done | 0/2 |
| `human-reply --cancel` | Cancel human wait (+ cancel flag) | 0/2 |
| `cancel` | Stop current `run` | 0/2 |
| `last-log [--lines N]` | Recent log text | 0 |
| `skills sync --from-dir <dir>` | Import local `.webclaw`/`.txt`/`.md` into skill cache | 0/2 |
| `skills list` | List cached skill names | 0/2 |
| `update` | Self-update (needs `WEBCLAW_LITE_BASE` or `WEBCLAW_WORKER_URL`) | 0/2 |
| `accounts list` / `accounts set …` | Site accounts | 0/2 |

## JSONL (`run`)

`run_start` · `step_start` / `step_ok` / `step_fail` · `need_human` · `need_accounts` · `log` · `done`

Judge success only from the last `"type":"done"` line (`ok` true).

## Env

| Variable | Default |
|----------|---------|
| `WEBCLAW_HOME` | `%LOCALAPPDATA%\webclaw` / `~/.webclaw` |
| `WEBCLAW_CHROME` | Auto-detect |
| `WEBCLAW_CDP_PORT` | `19222` |
| `WEBCLAW_HEADLESS` | unset = visible window |
| `WEBCLAW_LITE_BASE` | empty (no default CDN) |
| `WEBCLAW_WORKER_URL` | full download URL for update |
