---
name: webclaw-lite
description: >-
  Local webclaw-lite browser RPA: author with probe + short runs, freeze
  .webclaw under recipes/, preview/edit via DSH Files, replay with recipePath.
metadata:
  os: ["windows", "macos", "linux"]
---

# webclaw-lite

## Prefer Host tools (DSH)

- `webclaw_status` / `webclaw_probe`
- `webclaw_run` — inline `recipe` (authoring) **or** `recipePath` (replay)
- `webclaw_freeze` / `webclaw_list_recipes` / `webclaw_import`
- `webclaw_human_reply` / `webclaw_cancel`

## Three stages

1. **Authoring:** probe ↔ short run; no cold-guessed labels
2. **Freeze:** `webclaw_freeze` → `recipes/<name>.webclaw`; open in Files for sidebar preview; edit via open-in-app or fs tools
3. **Replay:** only `webclaw_run(recipePath=…)`; import attachments before treating them as the library

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

**Forbidden:** Playwright / CSS / labels not in the latest probe.
