---
name: webclaw-lite
description: >-
  Local webclaw-lite browser RPA: open pages, click visible text, wait, type,
  download/export. Prefer webclaw_* tools or the CLI recipe language — do not
  rewrite steps with Playwright, Puppeteer, or CSS selectors.
---

# webclaw-lite

Deterministic browser RPA driven by **Chinese natural-language recipes**.

## Rules

1. Prefer Host tools: `webclaw_status`, `webclaw_run`, `webclaw_human_reply`, `webclaw_cancel`
2. If using the CLI directly, call `webclaw-lite` with the same verbs
3. **Forbidden**: Playwright / Puppeteer / Stagehand / CSS selectors / invented verbs like `填写`

## Recipe vocabulary (Chinese commas `，`)

| Step | Meaning |
|------|---------|
| `打开https://…` | Navigate |
| `等待2` | Sleep 2 seconds |
| `等待登录` | Wait until text appears |
| `点击登录` | Click visible text (not CSS) |
| `输入=北京天气` | Type into focused field |
| `输入搜索框=北京天气` | Type into labeled field |
| `按回车` | Key press |
| `采集` | Save visible page text under run `documents/` |

Write recipes as UTF-8 files. Do not pipe Chinese through PowerShell encodings.

## HITL

On `need_human`: show the user any captcha image, then `webclaw_human_reply` with `mode=text|ok|cancel`.

## Stability

Same recipe should replay the same business path. Prefer fixing the recipe over inventing selectors.
