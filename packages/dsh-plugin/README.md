# @lonk1989/dsh-plugin

DeepSeek Harness community plugin for **deterministic browser RPA** driven by Chinese natural-language recipes (English vocabulary planned later).

## What this is

- Task-level tools: `webclaw_run` / `webclaw_status` / `webclaw_human_reply` / `webclaw_cancel`
- Bundled Skill that teaches the recipe language
- Spawns the local **webclaw-lite** CLI (CDP + visible Chrome by default)

## What this is not

- Not a Playwright / CDP element-tool MCP
- Does **not** register `ctx.browserUse` (use official browser-use providers for DOM probing)
- Not an enterprise skill-cloud client

## Install

```sh
npm install @lonk1989/dsh-plugin
```

Build or download `webclaw-lite` from this repository’s Releases, then mount:

```yaml
- id: webclaw
  name: '@lonk1989/dsh-plugin'
  config:
    binaryPath: 'C:/path/to/webclaw-lite.exe'
    registerSkill: true
```

```sh
dsh web --patch examples/cordis.patch.yml
```

## Model experience

The model should write a short Chinese recipe and call `webclaw_run`. On captcha/slider pauses it uses `webclaw_human_reply`. It must not invent CSS selectors.

## Known limitations

- Requires a local Chrome/Edge and the webclaw-lite binary
- Windows is the primary tested host; other OS builds are experimental
- Recipe vocabulary is Chinese-first in v1
- Long runs need sandbox/full-access policy if your profile confines subprocesses

## License

MIT
