# webclaw-lite CLI contract

| Command | Purpose | Exit |
|---------|---------|------|
| `install` | Create data dirs, probe Chrome, write install marker (JSON) | 0/2 |
| `status [--run-id id]` | Health JSON (`product`, `chromeOk`, …) | 0/1 |
| `probe [--url URL] [--types …] [--search …] [--limit N]` | Authoring page scan; **one JSON** on stdout | 0/2 |
| `recipe save --name <stem> (--stdin \| --file <path>) [--title …]` | Write `WEBCLAW_HOME/recipes/<stem>.webclaw` | 0/2 |
| `recipe list` | List frozen `.webclaw` files (JSON) | 0/2 |
| `run --recipe <file\|name>` / `run --stdin` | Execute recipe; stdout **JSONL** | 0/1/2 |
| `human-reply --text CODE` | Captcha text | 0/2 |
| `human-reply --ok` | Confirm human/slider done | 0/2 |
| `human-reply --cancel` | Cancel human wait (+ cancel flag) | 0/2 |
| `cancel` | Stop current `run` | 0/2 |
| `last-log [--lines N]` | Recent log text | 0 |
| `skills sync --from-dir <dir>` | Import local `.webclaw`/`.txt`/`.md` into skill cache | 0/2 |
| `skills list` | List cached skill names | 0/2 |
| `update` | Self-update (needs `WEBCLAW_LITE_BASE` or `WEBCLAW_WORKER_URL`) | 0/2 |
| `accounts list` / `accounts set …` | Site accounts | 0/2 |

## `recipe` (local library)

- Files live under `<WEBCLAW_HOME>/recipes/*.webclaw` (UTF-8; optional `# title` first line)
- `run --recipe baidu-hot` resolves that directory when the path is not found as given
- DSH `webclaw_freeze` prefers the **workspace** `recipes/` folder when session cwd is known

## `probe` (authoring)

Stdout is a single JSON object (`"type":"probe"`) with `candidates[]` (`textCmd` / `indexCmd` / …).

## JSONL (`run`)

`run_start` · `step_*` · `need_human` · `done` — success only from last `"type":"done"` with `ok` true.

## Env

| Variable | Default |
|----------|---------|
| `WEBCLAW_HOME` | `%LOCALAPPDATA%\webclaw` / `~/.webclaw` |
| `WEBCLAW_CHROME` | Auto-detect |
| `WEBCLAW_CDP_PORT` | `19222` |
| `WEBCLAW_RESUME_BROWSER` | `1` under lite |
| `WEBCLAW_HEADLESS` | unset = visible window |
| `WEBCLAW_LITE_BASE` | empty (no default CDN) |
| `WEBCLAW_WORKER_URL` | full download URL for update |
