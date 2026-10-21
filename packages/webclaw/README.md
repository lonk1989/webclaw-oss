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
```

```yaml
- insert:
    - id: webclaw
      name: '@lonk1989/webclaw'
      config:
        binaryPath: 'C:/path/to/webclaw-lite.exe'
        registerSkill: true
        recipesRel: recipes
```

## Model experience

1. New site: `webclaw_probe` → short `webclaw_run` → freeze with `webclaw_freeze`
2. User opens `recipes/*.webclaw` in Files for preview; edits via open-in-app or fs tools
3. Replay: `webclaw_run(recipePath=…)` only
4. Uploaded `.webclaw`: `recipePath` once, or `webclaw_import` into the library

## License

MIT
