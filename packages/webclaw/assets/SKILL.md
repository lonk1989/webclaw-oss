---
name: webclaw-lite
description: >-
  Local webclaw-lite browser RPA: author with multi-round probe + short runs,
  freeze UTF-8 .webclaw recipes under workspace recipes/, preview/edit via DSH
  Files + sidebar, replay with webclaw_run(recipePath). Never Playwright/CSS.
---

# webclaw-lite

Deterministic browser RPA driven by **Chinese natural-language recipes** stored as local `.webclaw` files.

## Modes

| Mode | When | Tools |
|------|------|-------|
| **Authoring** | New / unknown site | `webclaw_probe` ↔ short `webclaw_run(recipe=…)` |
| **Freeze** | Path works | `webclaw_freeze` → `recipes/<name>.webclaw` |
| **Replay** | Stable recipe | `webclaw_run(recipePath=…)` only |

## Rules

1. Prefer Host tools: `webclaw_status`, `webclaw_probe`, `webclaw_run`, `webclaw_freeze`, `webclaw_list_recipes`, `webclaw_import`, `webclaw_human_reply`, `webclaw_cancel`
2. **Forbidden**: Playwright / Puppeteer / CSS selectors / invented verbs / labels not in the latest probe
3. Chat **attachments** are one-shot (read-only). For durable library use `webclaw_import` or `webclaw_freeze`

## Authoring

1. Do not cold-guess control labels
2. `webclaw_probe` — optional `url` on first hop; later rounds omit url
3. Short `webclaw_run` from returned `textCmd` / `indexCmd` / `clickCmd` / `inputCmd`
4. Repeat until the business path works

## Freeze (local library)

After the flow works:

1. `webclaw_freeze(name=…, recipe=<full text>)` → writes workspace `recipes/<name>.webclaw` (or `WEBCLAW_HOME/recipes` if no session cwd)
2. Tell the user the **path**
3. User opens it in **Files** → right sidebar **plain-text preview**
4. Secondary edit: preview header **open-in-app**, or Agent `write`/`edit` on that path, then run again

Prefer URL deep-links in the frozen recipe when the site allows.

## Replay

- `webclaw_run(recipePath=recipes/foo.webclaw)` or absolute / attachment path
- Do **not** probe every step; probe only to diagnose and **edit the file**
- Uploaded `.webclaw`: run with `recipePath` once, or `webclaw_import` then freeze into the library

## Preview / edit (DSH generic)

| Action | How |
|--------|-----|
| Preview | Files → `recipes/*.webclaw` → sidebar plain text |
| Edit | open-in-app or Agent fs tools on the same path |
| @ mention | `@recipes/foo.webclaw` then `webclaw_run(recipePath=…)` |

## Recipe vocabulary (Chinese commas `，`)

| Step | Meaning |
|------|---------|
| `打开https://…` | Navigate |
| `等待2` | Sleep seconds |
| `等待登录` | Wait for visible text |
| `点击登录` | Click visible text |
| `输入=…` / `输入搜索框=…` | Type (labels from probe when authoring) |
| `按回车` | Key |
| `采集` | Save visible text to run documents |

Optional first line: `# title` (ignored by the runner except as documentation).

## HITL

On `need_human`: show captcha → `webclaw_human_reply` with `mode=text|ok|cancel`.
