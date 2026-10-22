---
name: webclaw-lite
description: >-
  Local webclaw-lite browser RPA. DEFAULT: full user recipe or .webclaw path →
  one webclaw_run (entire body / recipePath), never probe step-by-step. Probe
  only when authoring a NEW unknown site. Freeze under recipes/; replay by path.
metadata:
  os: ["windows", "macos", "linux"]
---

# webclaw-lite

## Prefer Host tools (DSH)

- `webclaw_status` / `webclaw_accounts_status` / `webclaw_inspect_params` / `webclaw_probe` (authoring only)
- `webclaw_run` — **full** inline `recipe` or `recipePath`; if `needParams`, show form then re-run with `paramsConfirmed=true`
- `webclaw_freeze` / `webclaw_list_recipes` / `webclaw_import`
- `webclaw_human_reply` / `webclaw_cancel`

## Decision

| User input | Action | Forbidden |
|------------|--------|-----------|
| Complete recipe / `.webclaw` | `webclaw_run` with full body or path | Probe between steps; split the recipe |
| Recipe has `参数…=` | Show returned form → wait → `paramsConfirmed=true` | Silent defaults; skip form |
| Login / passwords | Deploy may `config.systems` for URLs; local `accounts set` for passwords; recipe `登录…` only | Paste password into chat |
| New unknown site, no recipe | Authoring: probe ↔ short run | Invent labels; Playwright |

## Three stages

1. **Replay (default):** user gave full recipe → single `webclaw_run`
2. **Authoring:** only when discovering a new flow — probe ↔ short run; no cold-guessed labels
3. **Freeze:** `webclaw_freeze` → `recipes/<name>.webclaw`; open in Files for sidebar preview

## CLI fallback

```powershell
$lite = "path\to\webclaw-lite.exe"
& $lite recipe save --name baidu-hot --file .\draft.webclaw
& $lite recipe list
& $lite run --recipe baidu-hot          # resolves WEBCLAW_HOME/recipes/baidu-hot.webclaw
& $lite run --recipe .\recipes\baidu-hot.webclaw
```

## Recipe vocabulary

Steps separated by Chinese commas `，`. UTF-8. Optional `# title` first line.

**Forbidden:** Playwright / CSS / labels not in the latest probe (authoring only).
