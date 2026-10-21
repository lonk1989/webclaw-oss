# @lonk1989/webclaw

DeepSeek Harness community plugin for **deterministic browser RPA** driven by Chinese natural-language recipes (English vocabulary planned later).

Cordis plugin id: `webclaw` (fiber `id` in patch). npm package: **`@lonk1989/webclaw`**.

## What this is

- Task tools: `webclaw_probe` / `webclaw_run` / `webclaw_freeze` / `webclaw_list_recipes` / `webclaw_import` / `webclaw_status` / `webclaw_human_reply` / `webclaw_cancel`
- Bundled Skill: **authoring** → **freeze** `.webclaw` → **replay** by path
- Local library: workspace `recipes/*.webclaw` (or `WEBCLAW_HOME/recipes`)
- Preview/edit: DSH Files + sidebar plain text + open-in-app (no custom viewer)
- Spawns local **webclaw-lite** (CDP; shared browser across probe/run)

## What this is not

- Not Playwright / CSS-selector MCP
- Does **not** register `ctx.browserUse`
- Not a cloud skill store

## Install

```sh
cd /path/to/deepseek-harness
pnpm add @lonk1989/webclaw
# or: dsh plugin --profile web add @lonk1989/webclaw
```

```yaml
- insert:
    - id: webclaw
      name: '@lonk1989/webclaw'
      config:
        # defaults: ensureOnBoot true → download lite (Windows, multi-source)
        registerSkill: true
        recipesRel: recipes
        # Optional overrides:
        # binaryPath: 'C:/path/to/webclaw-lite.exe'
        # liteBaseUrl: 'https://cdn.example.com/webclaw/v0.1.8'  # tried first
```

### webclaw-lite binary (Windows)

You do **not** need to install lite by hand when using the defaults:

1. Plugin boot (and first tool call) downloads `webclaw-lite-windows-x64.exe` for **this package version**
2. Sources (in order): non-GitHub `liteBaseUrl` → `WEBCLAW_LITE_MIRRORS` → built-in mirrors (`ghfast.top`, then `mirror.ghproxy.com`) → official GitHub (last, so CN users are not stuck waiting on a dead GitHub)
3. Verifies the companion `.sha256`, then installs to `%LOCALAPPDATA%\webclaw\bin\webclaw-lite.exe`
4. Runs `webclaw-lite install` (Chrome probe + marker)

Each source times out after ~60s before the next. Set `WEBCLAW_LITE_MIRRORS` to comma-separated release dirs or proxy prefixes if you need extra fallbacks.

Non-Windows: set `binaryPath` to a locally built binary (Release assets are Windows-first for now), or build from `runtime/`.

Manual one-liner (optional):

```powershell
irm https://raw.githubusercontent.com/lonk1989/webclaw-oss/main/static/webclaw-lite/install.ps1 | iex
```

## Model experience

1. New site: `webclaw_probe` → short `webclaw_run` → freeze with `webclaw_freeze`
2. User opens `recipes/*.webclaw` in Files — the Client half tints the file icon and opens a log-style recipe preview (no engine patches)
3. Replay: `webclaw_run(recipePath=…)` only
4. Uploaded `.webclaw`: `recipePath` once, or `webclaw_import` into the library

## License

MIT
