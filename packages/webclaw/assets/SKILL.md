---
name: webclaw-lite
description: >-
  Local webclaw-lite browser RPA. DEFAULT: full recipe → webclaw_run once.
  Prefab 参数…= → needParams: show editable table once (users usually must
  change values);「用默认」only if user says so. No field-by-field interview.
  Feishu/SSO: URL only, no password. Never invent accounts-set CLI / probe a
  complete recipe / Playwright.
---

# webclaw-lite

Deterministic browser RPA driven by **Chinese natural-language recipes** stored as local `.webclaw` files.

## Decision (read first — do not skip)

| User input | What you MUST do | Forbidden |
|------------|------------------|-----------|
| Pastes / attaches a **complete** recipe (many `，` steps, or `.webclaw`) | `webclaw_run` with the **entire** body / `recipePath` | Split into steps; `webclaw_probe` between steps |
| Recipe declares **`参数…=`** (prefab placeholders) | First `webclaw_run` → `needParams` → **show editable table once** → user edits → `paramsConfirmed=true` + `params` JSON | Silently use placeholders; skip form; push「用默认」; ask each field verbally |
| User already gave **all** business fields in the same turn | Merge into `params` and run (`paramsConfirmed=true`) | Re-interview; ask「是否确认」「看下浏览器」before running |
| Points at `recipes/*.webclaw` | Same as above | Probe “to be safe” |
| New unknown site, **no** complete recipe | Authoring: `webclaw_probe` ↔ short `webclaw_run` | Invent labels; Playwright |
| Login / EHR / 飞书 SSO | `登录EHR系统` via `webclaw_run`. Vault needs **correct SSO URL** only (`…/login?sso=lark&…`). No password. | Ask password; paste `webclaw-lite.exe accounts set`; block because `has_password=false`; invent password form |

**Rule of thumb:** One (or two with param confirm) `webclaw_run` — not probe loops, not password chats, not extra Q&A.

## Modes

| Mode | When | Tools |
|------|------|-------|
| **Replay (default)** | Full recipe / path / frozen file | `webclaw_run` (+ param confirm when needed) |
| **Authoring** | New / unknown site, no complete recipe | `webclaw_probe` ↔ short `webclaw_run` |
| **Freeze** | Path works and should be kept | `webclaw_freeze` |

## Rules

1. Prefer Host tools: `webclaw_status`, `webclaw_accounts_status`, `webclaw_inspect_params`, `webclaw_probe`, `webclaw_run`, `webclaw_freeze`, `webclaw_list_recipes`, `webclaw_import`, `webclaw_human_reply`, `webclaw_cancel`
2. **Forbidden**: Playwright / Puppeteer / CSS / invented verbs / labels not in the latest probe (authoring only)
3. Chat attachments are one-shot. Durable library: `webclaw_import` / `webclaw_freeze`
4. **Install**: Windows / macOS (arm64) plugin auto-downloads lite. Intel Mac / Linux: build `runtime/` or set `binaryPath`. Gatekeeper: `xattr -d com.apple.quarantine ~/.webclaw/bin/webclaw-lite`
5. Never turn a long user recipe into multi-round probe
6. **参数**: Prefab values are usually wrong for this run — show one editable table, let the user change them, then run. Do **not** verbally ask field-by-field. Do **not** suggest「用默认」unless the user asks. Only skip editing when the user already gave all fields in the same turn.
7. **Secrets / SSO**: Never ask for or accept passwords in chat. Never invent `webclaw-lite accounts set` / `--password`. Feishu SSO = URL with `sso=lark` in **设置 → 网站账号**. If `passwordRequired` is ever true, ignore it for SSO sites and run anyway.
8. **Recipe newlines**: Keep `登录…` as its own step; do not glue param values onto the next verb.

## Site accounts (local vault)

| Do | Don't |
|----|--------|
| `登录EHR系统` with vault URL = SSO entry e.g. `https://…/login?sso=lark&next=…` | Ask for EHR password; CLI `accounts set --password` |
| `webclaw_run` when `has_password=false` if URL is set / SSO | Refuse to open for missing password |
| If URL wrong: tell user to fix **设置 → 网站账号** (URL field only) | Probe about:blank; invent Playwright login |

`webclaw_accounts_status` may show `sso: true` / `passwordRequired: false`. That means **run**, do not configure a password.

## Replay

1. Full recipe as-is → `webclaw_run`
2. If `needParams`: show the table once for editing → wait for the edited values → `paramsConfirmed=true` + `params` JSON.「用默认」only if the user explicitly says so
3. If the user already gave all field values in the same turn: put them in `params` and run with `paramsConfirmed=true`
4. Report `ok` / `error` briefly. Do not ask the user to eyeball the browser unless the run **failed** and you need a human decision
5. Optional `webclaw_freeze` after success

## Authoring (only when no complete recipe)

`webclaw_probe` → short `webclaw_run` from probe cmds → freeze when stable.

## Recipe vocabulary

| Step | Meaning |
|------|---------|
| `参数公司=科技事业部` | Form field default/placeholder; body `{{公司}}` |
| `登录EHR系统` | Open vault URL (SSO OK, no password in recipe) |
| `打开https://…` | Navigate |
| `等待2` | Sleep seconds |
| `点击…` / `输入…=` | UI actions |

## HITL

On `need_human`: captcha → `webclaw_human_reply` with `mode=text|ok|cancel`.
