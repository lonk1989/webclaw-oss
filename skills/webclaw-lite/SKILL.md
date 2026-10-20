---
name: webclaw-lite
description: >-
  Local webclaw-lite browser RPA: open pages, click visible text, wait, type,
  download/export. Prefer webclaw_* Host tools or this CLI — do not rewrite
  steps with Playwright or CSS selectors.
metadata:
  os: ["windows", "macos", "linux"]
---

# webclaw-lite

Deterministic browser RPA driven by **Chinese natural-language recipes**.

## Prefer Host tools (DSH)

When `@lonk1989/dsh-plugin` is mounted, use:

- `webclaw_status`
- `webclaw_run` (pass the full recipe string)
- `webclaw_human_reply` / `webclaw_cancel`

## CLI fallback

```powershell
$env:WEBCLAW_HOME = Join-Path $env:LOCALAPPDATA "webclaw"
$lite = "path\to\webclaw-lite.exe"   # or from PATH
& $lite status
& $lite run --recipe .\recipe.webclaw
```

Install / update requires `WEBCLAW_LITE_BASE` or `WEBCLAW_WORKER_URL` pointing at your Release static site (no built-in tenant CDN).

## Recipe vocabulary

Steps separated by Chinese commas `，`. UTF-8 file required.

| Step | Meaning |
|------|---------|
| `打开https://…` | Navigate |
| `等待2` | Sleep seconds |
| `等待登录` | Wait for visible text |
| `点击登录` | Click visible text (not CSS) |
| `输入=内容` / `输入搜索框=内容` | Type |
| `按回车` | Key |
| `采集` | Save visible text to run documents |

**Forbidden:** Playwright / Puppeteer / CSS selectors / invented verbs.

## HITL

On `need_human`: show captcha image to the user → `human-reply --text` or `--ok`.
