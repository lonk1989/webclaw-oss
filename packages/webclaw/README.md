# @lonk1989/webclaw

DeepSeek Harness community plugin for **deterministic browser RPA** driven by Chinese natural-language recipes (English vocabulary planned later).

Cordis plugin id: `webclaw` (fiber `id` in patch). npm package: **`@lonk1989/webclaw`**.

## What this is

- Task tools: `webclaw_probe` / `webclaw_run` / `webclaw_inspect_params` / `webclaw_accounts_status` / `webclaw_freeze` / `webclaw_list_recipes` / `webclaw_import` / `webclaw_status` / `webclaw_human_reply` / `webclaw_cancel`
- Bundled Skill: **replay** (default) → **param form** when `参数…=` → **freeze** / **author** only for new sites
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
        # systems:   # private DSH fork only — real login URLs, no passwords
        #   - id: ehr
        #     name: EHR系统
        #     url: https://example.com/ehr-login
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

1. **Default / replay:** user pastes a full recipe or points at `.webclaw` → `webclaw_run` — never probe step-by-step
2. **参数 form:** recipes like `参数公司=…，参数月份=…` (see `examples/整理6月考勤.webclaw`) → first `webclaw_run` returns `needParams` (no browser yet) → model shows the fields → user edits or says「用默认」→ second `webclaw_run` with `paramsConfirmed=true` and optional `params` JSON. Config: `requireParamConfirm` (default true)
3. New unknown site (no complete recipe): `webclaw_probe` → short `webclaw_run` → `webclaw_freeze`
4. User opens `recipes/*.webclaw` in Files — Client tints the icon and shows a log-style preview
5. Uploaded `.webclaw`: `recipePath` once (with param confirm if needed), or `webclaw_import` into the library

## Site accounts / passwords (local only)

DSH sends chat to the model over the network. **Do not put site passwords in recipes or chat.**

### Deploy overlay: seed login URLs (no passwords)

Private DSH forks can prefill **id / name / URL** on boot (OSS catalog only has placeholder URLs):

```yaml
config:
  systems:
    - id: ehr
      name: EHR系统
      url: https://your-corp-ehr-login/
    - id: old-mes
      name: 旧MES系统
      url: https://your-corp-mes-login/
```

This runs `webclaw-lite accounts ensure` and never writes passwords. Keep real URLs in your private patch, not in public OSS.

### User credentials (once per machine) — settings form

Open **Settings → 网站账号** in DSH (local Host must be on this machine). The form talks to a **loopback-only** service (`127.0.0.1:19229` by default); passwords are written into the webclaw vault (DPAPI on Windows) and **never** enter chat or the model.

Fallback (no PATH needed):

```text
%LOCALAPPDATA%\webclaw\bin\webclaw-accounts.cmd
```

That launcher is created on plugin boot. A `webclaw-lite.cmd` wrapper with an absolute path is written next to it so you need not add `webclaw-lite` to PATH.

Config knobs: `accountsUi` (default true), `accountsUiPort` (default 19229).

Recipes keep alias login only, e.g. `登录EHR系统`. The agent may call `webclaw_accounts_status` (no secrets); it must never ask you to paste a password into the conversation.

## License

MIT
