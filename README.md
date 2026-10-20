# webclaw-oss

**Deterministic browser RPA driven by natural-language recipes (Chinese first).**

This repository is the open-source fork of webclaw for the [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) community (`dsh-plugin` topic).

## Core

| Piece | Role |
|-------|------|
| `runtime/` | Rust **webclaw-lite** CLI — parse Chinese recipes, drive local Chrome via CDP, HITL for captcha/slider |
| `packages/dsh-plugin/` | npm **`@webclaw/dsh-plugin`** — Host tools + Skill that spawn lite |

Same recipe → same business path. Prefer fixing the recipe over inventing CSS selectors.

## Non-goals (v1)

- Not a Playwright / Stagehand / element-tool MCP
- Does not register exclusive `ctx.browserUse`
- No enterprise corp-session skill cloud
- Desktop Tauri UI is not part of the OSS v1 publish surface
- English recipe keywords are planned later

## Quick start

### 1. Build webclaw-lite

```powershell
cd runtime
cargo build --release --bin webclaw-lite
# binary: ../target/release/webclaw-lite.exe
```

### 2. Install the DSH plugin

```sh
cd packages/dsh-plugin
npm install
npm run build
# or: npm install @webclaw/dsh-plugin  (after publish)
```

### 3. Mount in DSH

See [examples/cordis.patch.yml](examples/cordis.patch.yml) and [packages/dsh-plugin/cordis.patch.yml](packages/dsh-plugin/cordis.patch.yml).

```yaml
- id: webclaw
  name: '@webclaw/dsh-plugin'
  config:
    binaryPath: 'E:/Workspace/webclaw-oss/target/release/webclaw-lite.exe'
    registerSkill: true
```

```sh
dsh web --patch examples/cordis.patch.yml
```

Ask the agent to run a recipe such as:

```text
打开https://example.com，等待2，采集
```

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
packages/dsh-plugin/     # @webclaw/dsh-plugin
examples/                # sample recipes + cordis overlay
skills/webclaw-lite/     # CLI Skill docs (source of truth also copied into plugin assets)
```

## License

MIT. Forked from the former vision-coord-client / webclaw product tree; tenant-specific defaults (CDN, corp auth, Feishu) were removed for OSS.
