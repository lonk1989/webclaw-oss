# webclaw-oss

**Deterministic browser RPA driven by natural-language recipes (Chinese first).**

This repository is the open-source fork of webclaw for the [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) community (`dsh-plugin` topic).

## Core

| Piece | Role |
|-------|------|
| `runtime/` | Rust **webclaw-lite** CLI — parse Chinese recipes, drive local Chrome via CDP, HITL for captcha/slider |
| `packages/webclaw/` | npm **`@lonk1989/webclaw`** — Host tools + Skill that spawn lite |

Same recipe → same business path. Prefer fixing the recipe over inventing CSS selectors.

## Non-goals (v1)

- Not a Playwright / Stagehand / element-tool MCP
- Does not register exclusive `ctx.browserUse`
- No enterprise corp-session skill cloud
- Desktop Tauri UI is not part of the OSS v1 publish surface
- English recipe keywords are planned later

## Quick start

### 1. Install the DSH plugin (recommended)

```sh
dsh plugin --profile web add @lonk1989/webclaw
```

On **Windows**, the plugin **auto-downloads** `webclaw-lite` from [GitHub Releases](https://github.com/lonk1989/webclaw-oss/releases) on first boot / tool call (SHA256 verified → `%LOCALAPPDATA%\webclaw\bin\webclaw-lite.exe`). No separate hand install.

Mount with an **`insert:`** patch (a bare `- id:` only updates an existing row):

```yaml
- insert:
    - id: webclaw
      name: '@lonk1989/webclaw'
      config:
        registerSkill: true
        recipesRel: recipes
```

```sh
pnpm dsh web --patch examples/cordis.patch.yml
```

Ask the agent to run a recipe such as:

```text
打开https://example.com，等待2，采集
```

### 2. Local dev (optional)

Build lite from source when hacking the Rust CLI:

```powershell
cd runtime
cargo build --release --bin webclaw-lite
# then set config.binaryPath to ../target/release/webclaw-lite.exe
```

Plugin package:

```sh
cd packages/webclaw
npm install
npm run build
```

See [examples/cordis.patch.yml](examples/cordis.patch.yml) and [packages/webclaw/cordis.patch.yml](packages/webclaw/cordis.patch.yml).
## vs official browser-use

| | Playwright / CDP MCP | webclaw |
|--|----------------------|---------|
| Abstraction | Element / snapshot tools | Fixed Chinese **recipe** |
| Targeting | CSS / a11y | Visible text |
| HITL | DIY | Built-in `need_human` |
| Window | Often headless | Visible Chrome by default |

## Layout

```text
runtime/                 # Rust crate (lite-first, desktop feature opt-in)
packages/webclaw/        # @lonk1989/webclaw
examples/                # sample recipes + cordis overlay
skills/webclaw-lite/     # CLI Skill docs (source of truth also copied into plugin assets)
```

## License

MIT. Forked from the former vision-coord-client / webclaw product tree; tenant-specific defaults (CDN, corp auth, Feishu) were removed for OSS.
