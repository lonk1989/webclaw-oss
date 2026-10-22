---
name: webclaw-lite
description: >-
  Local webclaw-lite browser RPA. DEFAULT: full recipe/.webclaw → webclaw_run
  once (never probe step-by-step). 参数…= placeholders → needParams form first
  (edit or「用默认」). Site passwords stay in LOCAL vault—NEVER ask in chat.
  Missing password does NOT block opening the page. Probe only when authoring
  a NEW unknown site. Never Playwright/CSS.
---

# webclaw-lite

Deterministic browser RPA driven by **Chinese natural-language recipes** stored as local `.webclaw` files.

## Decision (read first — do not skip)

| User input | What you MUST do | Forbidden |
|------------|------------------|-----------|
| Pastes / attaches a **complete** recipe (many `，` steps, or `.webclaw`) | `webclaw_run` with the **entire** body / `recipePath` | Split into steps; `webclaw_probe` between steps |
| Recipe declares **`参数…=`** (prefab placeholders) | First `webclaw_run` → `needParams` / form → **show fields; wait** → edit or「用默认」→ again with `params` + `paramsConfirmed=true` | Silently run with placeholder defaults; skip the form; probe instead |
| Points at `recipes/*.webclaw` or `@…webclaw` | Same as above (param form if declared) | Probe “to be safe”; re-author |
| Asks to **write/discover** a new flow on an unknown site | Authoring: `webclaw_probe` ↔ short `webclaw_run` | Invent labels; Playwright |
| Run **failed** mid-way | Diagnose with **one** probe (optional), **edit** the recipe, then re-run **full** body | Resume by probing every remaining step |
| Login / 网站账号 / 密码 | Still `webclaw_run` to **open the page** even if `has_password=false`. Only if login fill fails for missing creds → guide **设置 → 网站账号** (or `webclaw-accounts.cmd`) | Block run for no password; ask password in chat; probe; `输入密码=…` |

**Rule of thumb:** Replay finishes in one run call (or two when params need confirmation)—not dozens of probe rounds. **Passwords never enter DSH chat.** Prefab `参数` values are placeholders—confirm first.

## Modes

| Mode | When | Tools |
|------|------|-------|
| **Replay (default)** | User supplied full recipe / path / frozen file | `webclaw_run` (+ param confirm when needed) |
| **Authoring** | New / unknown site, **no** complete recipe yet | `webclaw_probe` ↔ short `webclaw_run(recipe=…)` |
| **Freeze** | Path works and should be kept | `webclaw_freeze` → `recipes/<name>.webclaw` |

## Rules

1. Prefer Host tools: `webclaw_status`, `webclaw_accounts_status`, `webclaw_inspect_params`, `webclaw_probe`, `webclaw_run`, `webclaw_freeze`, `webclaw_list_recipes`, `webclaw_import`, `webclaw_human_reply`, `webclaw_cancel`
2. **Forbidden**: Playwright / Puppeteer / CSS selectors / invented verbs / labels not in the latest probe (authoring only)
3. Chat **attachments** are one-shot (read-only). For durable library use `webclaw_import` or `webclaw_freeze`
4. **Install**: on Windows the `@lonk1989/webclaw` plugin auto-downloads `webclaw-lite` from GitHub Releases. If `webclaw_status` reports missing binary on non-Windows, point the user at building `runtime/` or setting `binaryPath`. Do not invent Playwright fallbacks.
5. **Never** turn a user-provided long recipe into a multi-round probe session.
6. **参数 form**: Prefab recipes always ship placeholder defaults. When `needParams` is returned, copy the form into chat and **wait**. Do not set `paramsConfirmed=true` until they edit or say「用默认」.
7. **Secrets**: Site URL/user/password live in the **local** vault. Recipes use `登录EHR系统` only. Never solicit, store, or echo passwords in chat. Missing password ≠ refuse to open the site.

## Site accounts (local vault — not DSH cloud)

DSH chat goes to the model over the network. **Treat passwords as toxic in chat.**

| Do | Don't |
|----|--------|
| Recipe: `登录EHR系统` / `登录旧MES系统` | `输入密码=明文` or `{{ehr.密码}}` in Files/chat |
| Deploy `config.systems` seeds URL only | Paste password into the agent |
| `webclaw_run` even when `has_password=false` (open page / SSO / session) | Refuse to open because vault has no password |
| After login fill fails: user fills **设置 → 网站账号** | Ask them to type the password in chat |
| `webclaw_accounts_status` → soft advisory | Invent a tool call that sends `--password` through the model |

Login URLs may be prefilled by the deployer's cordis `config.systems`. Passwords: **设置 → 网站账号** or `%LOCALAPPDATA%\webclaw\bin\webclaw-accounts.cmd`.

## Replay (default path)

1. Take the user's full recipe text **as-is** (preserve Chinese commas `，` and step order)
2. Call `webclaw_run(recipe=…)` or `webclaw_run(recipePath=…)`
3. If result has `needParams` / `form`:
   - Present the parameter form (name + placeholder default for each field)
   - Wait for edits or「用默认」— **do not** silently use placeholders
   - Call `webclaw_run` again with the same recipe/path, `paramsConfirmed=true`, and `params` JSON if values changed (e.g. `{"公司":"山西阳泉","月份":"2026-07"}`)
4. Report `ok` / `error` / `eventsTail`. If error asks for 网站账号, show `setupHint` and wait—do not probe. Otherwise do not probe unless the run failed and you need labels to **edit** the recipe
5. Optional: `webclaw_freeze` after a successful run if the user wants it in the library

`webclaw_inspect_params` can preview the form without starting a run.

Uploaded `.webclaw`: run with `recipePath` once (with param confirm if needed), or `webclaw_import` then freeze into the library.

## Authoring (only when no complete recipe)

1. Do not cold-guess control labels
2. `webclaw_probe` — optional `url` on first hop; later rounds omit url
3. Short `webclaw_run` from returned `textCmd` / `indexCmd` / `clickCmd` / `inputCmd`
4. Repeat until the business path works, then freeze

## Freeze (local library)

After the flow works:

1. `webclaw_freeze(name=…, recipe=<full text>)` → writes workspace `recipes/<name>.webclaw` (or `WEBCLAW_HOME/recipes` if no session cwd)
2. Tell the user the **path**
3. User opens it in **Files** → right sidebar **plain-text preview**
4. Secondary edit: preview header **open-in-app**, or Agent `write`/`edit` on that path, then run again

Prefer URL deep-links in the frozen recipe when the site allows.

## Preview / edit (DSH generic)

| Action | How |
|--------|-----|
| Preview | Files → `recipes/*.webclaw` → sidebar plain text |
| Edit | open-in-app or Agent fs tools on the same path |
| @ mention | `@recipes/foo.webclaw` then `webclaw_run(recipePath=…)` |

## Recipe vocabulary (Chinese commas `，`)

| Step | Meaning |
|------|---------|
| `参数公司=科技事业部` | Editable run input (form field `公司`, default/placeholder 科技事业部); body uses `{{公司}}` — not a password |
| `登录EHR系统` | Open vaulted site URL; fill credentials locally if needed (never put password in the recipe) |
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
