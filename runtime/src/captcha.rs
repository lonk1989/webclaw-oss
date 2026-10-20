//! 登录验证码：检测 → 截图 → 弹窗等待用户输入 → 写入 Chrome

use crate::cdp::CdpClient;
use base64::Engine;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use crate::ui_host::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug)]
pub enum CaptchaResponse {
    Submitted(String),
    Cancelled,
}

pub type CaptchaWaiters = Arc<Mutex<HashMap<String, oneshot::Sender<CaptchaResponse>>>>;

pub fn new_waiters() -> CaptchaWaiters {
    Arc::new(Mutex::new(HashMap::new()))
}

/// 本任务已执行的登录表单字段（来自「输入手机=…」「输入密码=…」步骤）
#[derive(Debug, Default, Clone)]
pub struct LoginFormSnapshot {
    pub phone: Option<String>,
    pub password: Option<String>,
}

impl LoginFormSnapshot {
    pub fn absorb_input_step(&mut self, args: &str) {
        let (text, field) = crate::commands::scripts::parse_input_step_args(args);
        if text.is_empty() {
            return;
        }
        if field.contains("密码") {
            self.password = Some(text);
        } else if field.contains("手机")
            || field.contains("账号")
            || field.contains("帐户")
            || field.contains("用户名")
            || field.contains("user")
        {
            self.phone = Some(text);
        }
    }

    pub fn has_credentials(&self) -> bool {
        self.phone.is_some() || self.password.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DetectResult {
    #[serde(default, rename = "hasCaptcha")]
    has_captcha: bool,
    #[serde(default, rename = "inputSelector")]
    input_selector: Option<String>,
    #[serde(default)]
    clip: Option<ClipRect>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClipRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    #[serde(default = "default_dpr")]
    dpr: f64,
}

fn default_dpr() -> f64 {
    1.0
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptchaRequiredPayload {
    pub prompt_id: String,
    pub task_id: String,
    /// `image`：图形验证码；`slider`：需在浏览器中拖动滑块
    pub kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub image_base64: String,
    pub hint: String,
}

fn captcha_kind_image() -> String {
    "image".into()
}

fn captcha_kind_slider() -> String {
    "slider".into()
}

fn captcha_kind_human() -> String {
    "human".into()
}

const DETECT_JS: &str = r#"(() => {
  const kw = ['验证码', '校验码', 'captcha', 'verifycode', 'verify-code', 'vcode', 'graphiccode', 'checkcode', 'authcode', 'rand'];
  const imgKw = ['验证码', 'captcha', 'verify', 'kaptcha', 'vcode', 'validate', 'checkcode', 'authcode', 'rand', 'verifycanvas', 'vcodeimg'];
  const KNOWN_GRAPHIC = ['#verifyCanvas', '#vCodeImg canvas', '#vCodeImg', '[id="vCodeImg"]', '[id="verifyCanvas"]'];
  const matchMeta = (s) => {
    const t = String(s || '').toLowerCase();
    return kw.some((k) => t.includes(k));
  };
  const matchImgMeta = (s) => {
    const t = String(s || '').toLowerCase();
    return imgKw.some((k) => t.includes(k));
  };
  // 无头下不要求落在视口内；canvas 可能 CSS 尺寸暂为 0 但有位图宽高
  const isDisplayed = (el, loose) => {
    if (!el || !(el instanceof Element)) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    const min = loose ? 4 : 8;
    if (r.width >= min && r.height >= min) return true;
    if (el.offsetWidth >= min && el.offsetHeight >= min) return true;
    if (el.tagName === 'CANVAS') {
      const cw = Number(el.width) || 0;
      const ch = Number(el.height) || 0;
      if (cw >= 20 && ch >= 10) return true;
    }
    return false;
  };
  const layoutRect = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width >= 4 && r.height >= 4) {
      return { x: r.left, y: r.top, w: r.width, h: r.height };
    }
    if (el.tagName === 'CANVAS') {
      const cw = Number(el.width) || 0;
      const ch = Number(el.height) || 0;
      if (cw >= 20 && ch >= 10) {
        // 位图有尺寸但 CSS 未铺开：用属性尺寸兜底，位置仍用 getBoundingClientRect
        return { x: r.left, y: r.top, w: Math.max(r.width, cw), h: Math.max(r.height, ch) };
      }
    }
    return { x: r.left, y: r.top, w: Math.max(r.width, el.offsetWidth || 0), h: Math.max(r.height, el.offsetHeight || 0) };
  };
  const adjacentTo = (a, b) => {
    const r = a.getBoundingClientRect();
    const ir = b.getBoundingClientRect();
    const gapX = Math.min(Math.abs(r.left - ir.right), Math.abs(ir.left - r.right));
    const gapY = Math.min(Math.abs(r.top - ir.bottom), Math.abs(ir.top - r.bottom));
    const cy = (r.top + r.bottom) / 2;
    const icy = (ir.top + ir.bottom) / 2;
    const cx = (r.left + r.right) / 2;
    const icx = (ir.left + ir.right) / 2;
    return (gapX < 180 && Math.abs(cy - icy) < 64)
      || (gapY < 80 && Math.abs(cx - icx) < 220);
  };
  const sameFormRow = (a, b) => {
    const row = b.closest('.ant-form-item-control-input-content, .ant-form-item, form, .login-form, .loginForm, .login-box');
    return !!(row && row.contains(a));
  };
  const graphicMeta = (el) => {
    const p = el.parentElement;
    return [el.id, el.className, el.getAttribute('aria-label'), el.title, el.getAttribute('alt'),
      p && p.id, p && p.className, p && p.getAttribute('aria-label')].join(' ');
  };
  const resolveGraphic = (el) => {
    if (!el) return null;
    if (el.tagName === 'CANVAS' || el.tagName === 'IMG') return el;
    return el.querySelector('canvas, img') || el;
  };
  const dpr = window.devicePixelRatio || 1;

  const findKeywordInput = () => {
    for (const inp of document.querySelectorAll('input:not([type=hidden]):not([type=password])')) {
      if (!isDisplayed(inp, true)) continue;
      const type = (inp.type || 'text').toLowerCase();
      if (['tel', 'email', 'number', 'search'].includes(type) && !matchMeta(inp.placeholder)) continue;
      const meta = [inp.placeholder, inp.name, inp.id, inp.className, inp.getAttribute('aria-label'), inp.title].join(' ');
      if (matchMeta(meta)) return inp;
    }
    for (const inp of document.querySelectorAll('input[type=text], input:not([type])')) {
      if (!isDisplayed(inp, true)) continue;
      const ph = (inp.placeholder || '').trim();
      if (/验证码|校验码|captcha|verify\s*code|vcode/i.test(ph)) return inp;
    }
    return null;
  };

  const findInputNearGraphic = (imgEl) => {
    if (!imgEl) return null;
    const row = imgEl.closest('.ant-form-item-control-input-content, .ant-form-item, form, .login-form, .loginForm, .login-box')
      || imgEl.parentElement;
    if (row) {
      for (const inp of row.querySelectorAll('input:not([type=hidden]):not([type=password])')) {
        if (!isDisplayed(inp, true)) continue;
        const type = (inp.type || 'text').toLowerCase();
        if (type === 'tel' || type === 'email') continue;
        return inp;
      }
    }
    let best = null;
    let bestDist = Infinity;
    const ir = imgEl.getBoundingClientRect();
    const icy = (ir.top + ir.bottom) / 2;
    const icx = (ir.left + ir.right) / 2;
    for (const inp of document.querySelectorAll('input:not([type=hidden]):not([type=password])')) {
      if (!isDisplayed(inp, true)) continue;
      const type = (inp.type || 'text').toLowerCase();
      if (['tel', 'email', 'password'].includes(type)) continue;
      if (!adjacentTo(inp, imgEl) && !sameFormRow(inp, imgEl)) continue;
      const r = inp.getBoundingClientRect();
      const dist = Math.abs(((r.top + r.bottom) / 2) - icy) + Math.abs(((r.left + r.right) / 2) - icx) * 0.25;
      if (dist < bestDist) { bestDist = dist; best = inp; }
    }
    return best;
  };

  // 1) 先找关键词输入框；2) 旧 MES 等可先找已知图形再反推输入框（无头下关键词框偶发未就绪）
  let inputEl = findKeywordInput();
  let imgEl = null;
  let bestImgScore = 0;
  const scoreGraphic = (el) => {
    let score = 0;
    const meta = graphicMeta(el).toLowerCase();
    if (matchImgMeta(meta)) score += 10;
    if (/verifycanvas|vcodeimg|#?vcode/i.test(meta)) score += 8;
    const r = layoutRect(el);
    if (r.w >= 28 && r.w <= 360 && r.h >= 12 && r.h <= 140) score += 3;
    if (inputEl && adjacentTo(el, inputEl)) score += 6;
    if (inputEl && sameFormRow(el, inputEl)) score += 8;
    return score;
  };

  for (const sel of KNOWN_GRAPHIC) {
    const el = document.querySelector(sel);
    if (!el) continue;
    const target = resolveGraphic(el);
    if (!target || !isDisplayed(target, true)) continue;
    const s = Math.max(scoreGraphic(target), 16);
    if (s > bestImgScore) { bestImgScore = s; imgEl = target; }
  }
  if (!inputEl && imgEl) {
    inputEl = findInputNearGraphic(imgEl);
  }
  if (!inputEl && !imgEl) return { hasCaptcha: false };

  for (const img of document.querySelectorAll('img')) {
    if (!isDisplayed(img, false)) continue;
    const r = layoutRect(img);
    if (r.w < 28 || r.h < 14) continue;
    if ((img.src || '').startsWith('data:') && r.w < 40) continue;
    const s = scoreGraphic(img);
    if (s > bestImgScore) { bestImgScore = s; imgEl = img; }
  }
  for (const c of document.querySelectorAll('canvas')) {
    if (!isDisplayed(c, true)) continue;
    const r = layoutRect(c);
    if ((r.w < 28 && !(Number(c.width) >= 20)) || r.h < 12 || r.w > 360) continue;
    const s = scoreGraphic(c);
    if (s > bestImgScore) { bestImgScore = s; imgEl = c; }
  }
  if (!inputEl && imgEl) {
    inputEl = findInputNearGraphic(imgEl);
  }

  // 已知图形强制分 ≥16；须同时有可填输入框才弹窗（避免误报）
  const hasCaptcha = !!imgEl && !!inputEl && bestImgScore >= 8;
  if (!hasCaptcha) return { hasCaptcha: false };

  const pad = 6;
  let box = null;
  if (imgEl) {
    const r = layoutRect(imgEl);
    box = { x: r.x - pad, y: r.y - pad, w: r.w + pad * 2, h: r.h + pad * 2 };
  } else if (inputEl) {
    const row = inputEl.closest('.ant-form-item-control-input-content, .ant-form-item') || inputEl;
    const r = layoutRect(row);
    box = { x: r.x - pad, y: r.y - pad, w: r.w + pad * 2, h: r.h + pad * 2 };
  }
  if (box) {
    const vw = Math.max(window.innerWidth || 0, 800);
    const vh = Math.max(window.innerHeight || 0, 600);
    box.x = Math.max(0, box.x);
    box.y = Math.max(0, box.y);
    // 无头下 innerWidth 偶发偏小，避免把 clip 裁成 0
    box.w = Math.max(16, Math.min(box.w, vw - box.x));
    box.h = Math.max(16, Math.min(box.h, vh - box.y));
  }
  let inputSelector = '';
  if (inputEl) {
    if (inputEl.id) inputSelector = '#' + CSS.escape(inputEl.id);
    else if (inputEl.name) inputSelector = 'input[name="' + inputEl.name.replace(/"/g, '\\"') + '"]';
    else if ((inputEl.placeholder || '').trim()) {
      inputSelector = 'input[placeholder="' + (inputEl.placeholder || '').replace(/"/g, '\\"') + '"]';
    } else {
      inputSelector = 'input[placeholder*="验证码"]';
    }
  }
  return {
    hasCaptcha: true,
    inputSelector: inputSelector || null,
    clip: box ? { x: box.x, y: box.y, w: box.w, h: box.h, dpr } : null,
  };
})()"#;

/// DOM 上已有验证码线索但尚未布局完成（无头常见）：短等再检，避免无验证码页白等
const CAPTCHA_DOM_HINT_JS: &str = r#"(() => {
  const sels = [
    '#verifyCanvas', '#vCodeImg', '#vCodeImg canvas', '[id="verifyCanvas"]', '[id="vCodeImg"]',
    'input[placeholder*="验证码"]', 'input[placeholder*="校验码"]',
    'input[name*="captcha" i]', 'input[id*="vcode" i]', 'input[id*="captcha" i]',
  ];
  for (const sel of sels) {
    try { if (document.querySelector(sel)) return true; } catch (_) {}
  }
  const pwd = document.querySelector('input[type="password"]');
  if (!pwd) return false;
  for (const c of document.querySelectorAll('canvas')) {
    const r = c.getBoundingClientRect();
    const w = Math.max(r.width, Number(c.width) || 0);
    const h = Math.max(r.height, Number(c.height) || 0);
    if (w >= 20 && w <= 360 && h >= 10 && h <= 140) return true;
  }
  return false;
})()"#;

const NUDGE_CAPTCHA_JS: &str = r#"(() => {
  const el = document.querySelector('#verifyCanvas, #vCodeImg, #vCodeImg canvas, canvas');
  if (el) {
    try { el.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }); } catch (_) {}
  }
  return true;
})()"#;

async fn detect_once(client: &mut CdpClient) -> Result<Option<DetectResult>, String> {
    let v = client.evaluate_resilient(DETECT_JS).await?;
    let parsed: DetectResult = serde_json::from_value(v)
        .map_err(|e| format!("解析验证码检测结果失败: {e}"))?;
    if parsed.has_captcha {
        Ok(Some(parsed))
    } else {
        Ok(None)
    }
}

async fn captcha_dom_hint(client: &mut CdpClient) -> bool {
    match client.evaluate_resilient(CAPTCHA_DOM_HINT_JS).await {
        Ok(v) => v.as_bool() == Some(true),
        Err(_) => false,
    }
}

pub async fn detect(client: &mut CdpClient) -> Result<Option<DetectResult>, String> {
    client.ensure_attached().await?;
    if let Some(hit) = detect_once(client).await? {
        return Ok(Some(hit));
    }
    // 仅当页面已有验证码 DOM 线索时短等绘制（无头 canvas 常晚于账号框就绪）
    if !captcha_dom_hint(client).await {
        return Ok(None);
    }
    for _ in 0..8 {
        let _ = client.evaluate_resilient(NUDGE_CAPTCHA_JS).await;
        tokio::time::sleep(Duration::from_millis(220)).await;
        if let Some(hit) = detect_once(client).await? {
            return Ok(Some(hit));
        }
        if !captcha_dom_hint(client).await {
            break;
        }
    }
    Ok(None)
}

/// 页面当前是否存在可处理的图形验证码（供登录流程决定是否先点登录）
pub async fn page_has_captcha(client: &mut CdpClient) -> Result<bool, String> {
    Ok(detect(client).await?.is_some())
}

async fn capture_captcha_image(
    client: &mut CdpClient,
    info: &DetectResult,
) -> Result<Vec<u8>, String> {
    // 无头下 Page.captureScreenshot 对 canvas 偶发空白，优先直接导出 canvas 位图
    if let Ok(png) = capture_captcha_from_canvas(client).await {
        if png.len() > 80 {
            return Ok(png);
        }
    }
    if let Some(clip) = &info.clip {
        if clip.w >= 16.0 && clip.h >= 16.0 {
            if let Ok(png) = client
                .capture_screenshot_clip(clip.x, clip.y, clip.w, clip.h, clip.dpr)
                .await
            {
                return Ok(png);
            }
        }
    }
    client.capture_screenshot_png().await
}

const CANVAS_DATA_URL_JS: &str = r#"(() => {
  const pick = () => {
    for (const sel of ['#verifyCanvas', '#vCodeImg canvas', '#vCodeImg', 'canvas']) {
      const el = document.querySelector(sel);
      if (!el) continue;
      const c = el.tagName === 'CANVAS' ? el : el.querySelector('canvas');
      if (c && c.width >= 20 && c.height >= 10) return c;
    }
    return null;
  };
  const c = pick();
  if (!c || typeof c.toDataURL !== 'function') return null;
  try {
    const url = c.toDataURL('image/png');
    if (!url || !url.startsWith('data:image/png;base64,')) return null;
    return url.slice('data:image/png;base64,'.length);
  } catch (_) {
    return null;
  }
})()"#;

async fn capture_captcha_from_canvas(client: &mut CdpClient) -> Result<Vec<u8>, String> {
    let v = client.evaluate_resilient(CANVAS_DATA_URL_JS).await?;
    let Some(b64) = v.as_str() else {
        return Err("canvas 无导出".into());
    };
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("canvas 验证码解码失败: {e}"))
}

/// 聚焦并清空验证码框，返回中心坐标；供后续 CDP insertText 使用
async fn prepare_captcha_input(client: &mut CdpClient, selector: &str) -> Result<(f64, f64), String> {
    client.ensure_attached().await?;
    let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into());
    let expr = format!(
        r#"(() => {{
  const sel = {sel};
  const isVisible = (el) => {{
    if (!el || !(el instanceof Element)) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width >= 4 && r.height >= 4;
  }};
  const isCaptchaInput = (inp) => {{
    if (!inp || (inp.type || '').toLowerCase() === 'password') return false;
    const meta = [inp.placeholder, inp.name, inp.id, inp.getAttribute('aria-label'), inp.className].join(' ');
    if (/验证码|校验码|captcha|vcode|verifycode/i.test(meta)) return true;
    // 旧 MES：验证码框在 #vCodeImg / #verifyCanvas 同一行
    const row = inp.closest('.ant-form-item-control-input-content, .ant-form-item');
    if (row && (row.querySelector('#verifyCanvas, #vCodeImg, canvas') || document.getElementById('verifyCanvas'))) {{
      if (row.contains(document.getElementById('verifyCanvas'))
          || row.contains(document.getElementById('vCodeImg'))
          || row.querySelector('canvas')) {{
        return (inp.type || 'text').toLowerCase() === 'text' || !inp.type;
      }}
    }}
    return false;
  }};
  let el = null;
  try {{ el = document.querySelector(sel); }} catch (_) {{}}
  if (el && !isCaptchaInput(el)) el = null;
  if (!el) {{
    for (const inp of document.querySelectorAll('input:not([type=hidden]):not([type=password])')) {{
      if (!isVisible(inp)) continue;
      if (isCaptchaInput(inp)) {{ el = inp; break; }}
    }}
  }}
  if (!el) {{
    // 最后兜底：与 canvas 同排的 text input
    const canvas = document.querySelector('#verifyCanvas, #vCodeImg canvas, canvas');
    if (canvas) {{
      const row = canvas.closest('.ant-form-item-control-input-content, .ant-form-item') || canvas.parentElement;
      if (row) {{
        for (const inp of row.querySelectorAll('input:not([type=hidden]):not([type=password])')) {{
          if (isVisible(inp)) {{ el = inp; break; }}
        }}
      }}
    }}
  }}
  if (!el) return {{ ok: false, error: '找不到验证码输入框' }};
  try {{ el.scrollIntoView({{ block: 'center', inline: 'nearest', behavior: 'instant' }}); }} catch (_) {{}}
  try {{ if (el.readOnly) el.readOnly = false; }} catch (_) {{}}
  el.focus();
  el.click();
  try {{
    if (typeof el.select === 'function') el.select();
    else if (typeof el.setSelectionRange === 'function') {{
      const len = (el.value || '').length;
      el.setSelectionRange(0, len);
    }}
  }} catch (_) {{}}
  // 标记，供后续写入校验
  try {{ el.setAttribute('data-webclaw-captcha', '1'); }} catch (_) {{}}
  const r = el.getBoundingClientRect();
  return {{
    ok: true,
    x: r.left + r.width / 2,
    y: r.top + r.height / 2,
    placeholder: el.placeholder || '',
  }};
}})()"#
    );
    let v = client.evaluate_resilient(&expr).await?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("找不到验证码输入框")
            .to_string());
    }
    let x = v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let y = v.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0);
    Ok((x, y))
}

/// React/Ant Design 受控输入：重置 _valueTracker + 原生 value setter + InputEvent
async fn fill_captcha_via_dom(client: &mut CdpClient, code: &str) -> Result<bool, String> {
    let code_json = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".into());
    let expr = format!(
        r#"(() => {{
  const code = {code_json};
  let el = document.querySelector('input[data-webclaw-captcha="1"]');
  if (!el) {{
    for (const inp of document.querySelectorAll('input:not([type=hidden]):not([type=password])')) {{
      const meta = [inp.placeholder, inp.name, inp.id].join(' ');
      if (/验证码|captcha|vcode/i.test(meta)) {{ el = inp; break; }}
    }}
  }}
  if (!el) return {{ ok: false, stuck: true, error: '找不到验证码输入框' }};
  try {{ if (el.readOnly) el.readOnly = false; }} catch (_) {{}}
  el.focus();
  el.click();
  const proto = HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, 'value');
  const setValue = (val) => {{
    if (desc && desc.set) desc.set.call(el, val);
    else el.value = val;
  }};
  // 打断 React 受控组件对旧值的缓存
  try {{
    const tracker = el._valueTracker;
    if (tracker && typeof tracker.setValue === 'function') tracker.setValue('');
  }} catch (_) {{}}
  setValue('');
  try {{
    el.dispatchEvent(new InputEvent('input', {{ bubbles: true, data: '', inputType: 'deleteContentBackward' }}));
  }} catch (_) {{
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  }}
  try {{
    const tracker = el._valueTracker;
    if (tracker && typeof tracker.setValue === 'function') tracker.setValue('');
  }} catch (_) {{}}
  setValue(code);
  try {{
    if (typeof el.setCustomValidity === 'function') el.setCustomValidity('');
  }} catch (_) {{}}
  try {{
    el.dispatchEvent(new CompositionEvent('compositionend', {{ bubbles: true, data: code }}));
  }} catch (_) {{}}
  try {{
    el.dispatchEvent(new InputEvent('input', {{ bubbles: true, data: code, inputType: 'insertText' }}));
  }} catch (_) {{
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  }}
  el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  el.dispatchEvent(new Event('blur', {{ bubbles: true }}));
  el.focus();
  const actual = String(el.value || '');
  return {{ ok: actual === code, stuck: actual !== code, actual }};
}})()"#
    );
    let v = client.evaluate_resilient(&expr).await?;
    if v.get("error").and_then(|x| x.as_str()).is_some() {
        return Err(v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("写入验证码失败")
            .to_string());
    }
    Ok(v.get("ok").and_then(|x| x.as_bool()) == Some(true))
}

async fn captcha_value_matches(client: &mut CdpClient, code: &str) -> Result<bool, String> {
    let code_json = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".into());
    let expr = format!(
        r#"(() => {{
  const code = {code_json};
  let el = document.querySelector('input[data-webclaw-captcha="1"]');
  if (!el) {{
    for (const inp of document.querySelectorAll('input:not([type=hidden]):not([type=password])')) {{
      const meta = [inp.placeholder, inp.name, inp.id].join(' ');
      if (/验证码|captcha|vcode/i.test(meta)) {{ el = inp; break; }}
    }}
  }}
  if (!el) return {{ ok: false }};
  return {{ ok: String(el.value || '') === code }};
}})()"#
    );
    let v = client.evaluate_resilient(&expr).await?;
    Ok(v.get("ok").and_then(|x| x.as_bool()) == Some(true))
}

async fn fill_captcha(client: &mut CdpClient, selector: &str, code: &str) -> Result<(), String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("验证码不能为空".into());
    }
    let (x, y) = prepare_captcha_input(client, selector).await?;

    // 1) DOM + React _valueTracker
    if fill_captcha_via_dom(client, code).await? {
        return Ok(());
    }

    // 2) CDP 真实键入（isTrusted），兼容顽固受控组件
    let _ = client.click_at(x, y).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    let _ = prepare_captcha_input(client, selector).await?;
    // 清空后插入
    let _ = client
        .evaluate_resilient(
            r#"(() => {
  const el = document.querySelector('input[data-webclaw-captcha="1"]') || document.activeElement;
  if (!el || !('value' in el)) return { ok: false };
  try {
    const desc = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
    if (desc && desc.set) desc.set.call(el, '');
    else el.value = '';
    const tracker = el._valueTracker;
    if (tracker && typeof tracker.setValue === 'function') tracker.setValue('x');
    el.dispatchEvent(new Event('input', { bubbles: true }));
  } catch (_) {}
  try { el.select(); } catch (_) {}
  return { ok: true };
})()"#,
        )
        .await;
    client.insert_text(code).await?;
    tokio::time::sleep(Duration::from_millis(120)).await;

    if captcha_value_matches(client, code).await? {
        return Ok(());
    }
    // 再试一次纯 DOM
    if fill_captcha_via_dom(client, code).await? {
        return Ok(());
    }
    Err("验证码未能写入输入框（Ant/React 受控组件未接受）".into())
}

async fn refill_login_form(client: &mut CdpClient, creds: &LoginFormSnapshot) -> Result<(), String> {
    if !creds.has_credentials() {
        return Ok(());
    }
    client.ensure_attached().await?;
    let phone = creds
        .phone
        .as_deref()
        .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()))
        .unwrap_or_else(|| "null".into());
    let password = creds
        .password
        .as_deref()
        .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()))
        .unwrap_or_else(|| "null".into());
    let expr = format!(
        r#"(() => {{
  const phone = {phone};
  const password = {password};
  const kwPhone = ['手机', 'phone', 'mobile', 'tel', '账号', '帐户', '用户名', 'username'];
  const kwPwd = ['密码', 'password', 'pwd'];
  const isVisible = (el) => {{
    if (!el || !(el instanceof Element)) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden') return false;
    const r = el.getBoundingClientRect();
    return r.width >= 2 && r.height >= 2;
  }};
  const isCaptcha = (el) => {{
    const meta = [el.placeholder, el.name, el.id, el.getAttribute('aria-label')].join(' ');
    return /验证码|校验码|captcha|vcode|verifycode/i.test(meta);
  }};
  const matchField = (el, kws) => {{
    const meta = [el.placeholder, el.name, el.id, el.getAttribute('aria-label'), el.title, el.type].join(' ').toLowerCase();
    return kws.some((k) => meta.includes(k.toLowerCase()));
  }};
  const setVal = (el, val) => {{
    el.focus();
    try {{
      const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const desc = Object.getOwnPropertyDescriptor(proto, 'value');
      try {{
        const tracker = el._valueTracker;
        if (tracker && typeof tracker.setValue === 'function') tracker.setValue('');
      }} catch (_) {{}}
      if (desc && desc.set) desc.set.call(el, val);
      else el.value = val;
    }} catch (_) {{ el.value = val; }}
    try {{
      el.dispatchEvent(new InputEvent('input', {{ bubbles: true, data: val, inputType: 'insertText' }}));
    }} catch (_) {{
      el.dispatchEvent(new Event('input', {{ bubbles: true }}));
    }}
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  }};
  let filled = [];
  if (phone) {{
    let target = document.querySelector('input#username, input[name="username"]');
    if (!target || !isVisible(target) || isCaptcha(target)) {{
      target = null;
      for (const inp of document.querySelectorAll('input:not([type=hidden])')) {{
        if (!isVisible(inp) || isCaptcha(inp)) continue;
        const type = (inp.type || 'text').toLowerCase();
        if (type === 'password') continue;
        if (matchField(inp, kwPhone) || type === 'tel') {{ target = inp; break; }}
      }}
    }}
    if (target) {{ setVal(target, phone); filled.push('phone'); }}
  }}
  if (password) {{
    let target = document.querySelector('input#password, input[name="password"], input[type=password]');
    if (!target || !isVisible(target)) {{
      target = null;
      for (const inp of document.querySelectorAll('input[type=password], input')) {{
        if (!isVisible(inp) || isCaptcha(inp)) continue;
        if ((inp.type || '').toLowerCase() === 'password' || matchField(inp, kwPwd)) {{
          target = inp; break;
        }}
      }}
    }}
    if (target) {{ setVal(target, password); filled.push('password'); }}
  }}
  return {{ ok: true, filled }};
}})()"#
    );
    let v = client.evaluate_resilient(&expr).await?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("回填登录表单失败")
            .to_string());
    }
    Ok(())
}

async fn click_login_button(client: &mut CdpClient) -> Result<(), String> {
    let expr = r#"(() => {
  const kw = ['登录', 'login', 'sign in', 'signin'];
  const normText = (s) => (s || '').trim().toLowerCase().replace(/\s+/g, '');
  const isVisible = (el) => {
    if (!el || !(el instanceof Element)) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden') return false;
    const r = el.getBoundingClientRect();
    return r.width >= 2 && r.height >= 2;
  };
  const preferred = document.querySelector('button.ant-btn-primary[type="submit"], form button[type="submit"], button.ant-btn-primary');
  if (preferred && isVisible(preferred)) {
    try { preferred.click(); return { ok: true }; } catch (_) {}
  }
  const nodes = document.querySelectorAll('button, input[type=submit], a, [role=button], [ng-click]');
  for (const el of nodes) {
    if (!isVisible(el)) continue;
    const t = normText(el.innerText || el.value || el.textContent || '');
    if (kw.some((k) => t === normText(k) || t.includes(normText(k)))) {
      try { el.click(); return { ok: true }; } catch (_) {}
    }
  }
  return { ok: false, error: '未找到登录按钮' };
})()"#;
    let v = client.evaluate_resilient(expr).await?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("点击登录失败")
            .to_string());
    }
    Ok(())
}

pub fn emit_captcha_required<R: Runtime>(app: &AppHandle<R>, payload: &CaptchaRequiredPayload) {
    let _ = app.emit("captcha-required", payload);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        let _ = win.emit("captcha-required", payload);
    }
    if payload.task_id != "main" {
        let label = crate::task_run::window_label(&payload.task_id);
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.show();
            let _ = win.unminimize();
            let _ = win.emit("captcha-required", payload);
        }
    }
}

pub fn emit_captcha_resolved<R: Runtime>(app: &AppHandle<R>, task_id: &str, prompt_id: &str) {
    let payload = serde_json::json!({ "taskId": task_id, "promptId": prompt_id });
    let _ = app.emit("captcha-resolved", &payload);
    if task_id == "main" {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.emit("captcha-resolved", &payload);
        }
    } else if let Some(win) = app.get_webview_window(&crate::task_run::window_label(task_id)) {
        let _ = win.emit("captcha-resolved", &payload);
    }
}

async fn prompt_user<R: Runtime>(
    app: Option<&AppHandle<R>>,
    waiters: &CaptchaWaiters,
    task_id: &str,
    png: &[u8],
    cancel: &AtomicBool,
) -> Result<String, String> {
    if let Some(bridge) = crate::human_bridge::FileHumanBridge::from_env() {
        let prompt_id = Uuid::new_v4().to_string();
        let req = crate::human_bridge::HumanRequest {
            prompt_id: prompt_id.clone(),
            task_id: task_id.to_string(),
            kind: captcha_kind_image(),
            hint: "请看验证码图片，回复图上的字符".into(),
            image_path: None,
            image_base64: Some(base64::engine::general_purpose::STANDARD.encode(png)),
        };
        bridge.write_request(&req, Some(png))?;
        match bridge
            .wait_reply(cancel, Duration::from_secs(600))
            .await?
        {
            crate::human_bridge::HumanReply::Text(t) if !t.is_empty() => return Ok(t),
            crate::human_bridge::HumanReply::Cancelled => return Err("已取消验证码输入".into()),
            crate::human_bridge::HumanReply::Confirmed => {
                return Err("图形验证码需要回复验证码文字，不能只确认".into())
            }
            crate::human_bridge::HumanReply::Text(_) => {
                return Err("验证码为空".into());
            }
        }
    }

    let app = app.ok_or_else(|| {
        "无法弹出验证码窗口：未启用文件人机桥（WEBCLAW_HUMAN_DIR），且无桌面 AppHandle".to_string()
    })?;

    let prompt_id = Uuid::new_v4().to_string();
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let (tx, rx) = oneshot::channel();
    waiters.lock().insert(prompt_id.clone(), tx);

    let payload = CaptchaRequiredPayload {
        prompt_id: prompt_id.clone(),
        task_id: task_id.to_string(),
        kind: captcha_kind_image(),
        image_base64: b64,
        hint: "请查看验证码图片并输入；提交后将自动回填账号密码、填入验证码并点击登录".into(),
    };
    emit_captcha_required(app, &payload);

    let result = tokio::select! {
        _ = async {
            while !cancel.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        } => {
            waiters.lock().remove(&prompt_id);
            emit_captcha_resolved(app, task_id, &prompt_id);
            return Err("已取消".into());
        }
        msg = rx => msg.map_err(|_| "验证码窗口已关闭".to_string())?,
    };

    emit_captcha_resolved(app, task_id, &prompt_id);

    match result {
        CaptchaResponse::Submitted(code) if !code.trim().is_empty() => Ok(code.trim().to_string()),
        CaptchaResponse::Submitted(_) => Err("验证码不能为空".into()),
        CaptchaResponse::Cancelled => Err("已取消输入验证码".into()),
    }
}

/// 若页面存在验证码则弹窗让用户输入并写入浏览器。
/// `app` 在桌面端传入；伙伴空间可传 `None`（依赖 `WEBCLAW_HUMAN_DIR` 文件桥）。
/// `retry_login` 为 true 时在填入验证码后回填登录凭据并再次点击登录。
/// 滑块验证：置前浏览器，等人拖完（或点「我已完成」）后再继续。
pub async fn resolve_if_present<R: Runtime>(
    app: Option<&AppHandle<R>>,
    waiters: &CaptchaWaiters,
    task_id: &str,
    client: &mut CdpClient,
    cancel: &AtomicBool,
    retry_login: bool,
    login_form: &LoginFormSnapshot,
    push_log: impl Fn(String),
) -> Result<(), String> {
    // 登录点击后常先出滑块；图形码之前/之后都可能再出现
    wait_slider_if_present(app, waiters, task_id, client, cancel, &push_log).await?;

    const MAX_ROUNDS: u32 = 3;
    for round in 0..MAX_ROUNDS {
        let Some(info) = detect(client).await? else {
            // 无图形码时再扫一次滑块（部分站点点登录后才弹出）
            wait_slider_if_present(app, waiters, task_id, client, cancel, &push_log).await?;
            return Ok(());
        };

        let line = if round == 0 {
            "检测到登录验证码，等待用户输入…".to_string()
        } else {
            format!("验证码仍可见，请重新输入（第 {} 次）", round + 1)
        };
        push_log(line);

        let png = capture_captcha_image(client, &info).await?;
        let code = prompt_user(app, waiters, task_id, &png, cancel).await?;

        let selector = info
            .input_selector
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "input")
            .unwrap_or("input[placeholder*=\"验证码\"]");

        // 先填验证码，再回填账号密码，避免 refill 抢焦点导致码被冲掉
        fill_captcha(client, selector, &code).await?;
        if !captcha_value_matches(client, &code).await.unwrap_or(false) {
            return Err("验证码写入后校验失败，页面输入框仍为空".into());
        }
        push_log("验证码已填入浏览器".into());

        if retry_login && login_form.has_credentials() {
            refill_login_form(client, login_form).await?;
            push_log("已重新填入账号与密码".into());
            // 回填后重新确认验证码仍在（部分站点会清空）
            if !captcha_value_matches(client, &code).await.unwrap_or(false) {
                fill_captcha(client, selector, &code).await?;
                push_log("回填账号后验证码被清空，已重新写入".into());
            }
        }

        if retry_login {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Err(e) = click_login_button(client).await {
                push_log(format!("自动点击登录: {e}（可手动点击）"));
            } else {
                push_log("已再次点击登录".into());
                let _ = client.wait_after_navigation_cancel(15, Some(cancel)).await;
            }
            wait_slider_if_present(app, waiters, task_id, client, cancel, &push_log).await?;
        }

        tokio::time::sleep(Duration::from_millis(800)).await;
        if detect(client).await?.is_none() {
            push_log("验证码已通过".into());
            wait_slider_if_present(app, waiters, task_id, client, cancel, &push_log).await?;
            return Ok(());
        }
    }
    Err("验证码多次尝试仍未通过".into())
}

const DETECT_SLIDER_JS: &str = r#"(() => {
  const textRe = /向右滑动|拖动滑块|请拖动|滑动验证|完成验证|安全验证|按住左边滑块|拖动完成验证|slide\s*to\s*(verify|complete)|drag\s*(the\s*)?slider|swipe\s*to\s*verify/i;
  const sels = [
    '.geetest_slider', '.geetest_btn', '.geetest_panel', '.geetest_widget',
    '#nc_1_n1z', '.nc-container', '.nc_scale', '.nc_iconfont.btn_slide',
    '.slidetounlock', '.slide-verify', '.slider-verify', '.sliderCaptcha',
    '.captcha-slider', '.yidun_slider', '.yidun_panel', '.yidun_bgimg',
    '#aliyunCaptcha-sliding-body', '.aliyunCaptcha-sliding',
    '[class*="slideVerify"]', '[class*="sliderCaptcha"]', '[class*="slide-captcha"]',
    '[id*="nc_1"]', '[class*="shumei"]'
  ];
  const isShown = (el) => {
    if (!el || !(el instanceof Element)) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width >= 20 && r.height >= 12;
  };
  for (const sel of sels) {
    try {
      for (const el of document.querySelectorAll(sel)) {
        if (isShown(el)) return { hasSlider: true, via: sel };
      }
    } catch (_) {}
  }
  const nodes = document.querySelectorAll('div, span, p, button, label, strong, h1, h2, h3, h4');
  for (const el of nodes) {
    if (!isShown(el)) continue;
    const t = (el.innerText || el.textContent || '').trim();
    if (!t || t.length > 80) continue;
    if (textRe.test(t)) return { hasSlider: true, via: 'text:' + t.slice(0, 40) };
  }
  return { hasSlider: false };
})()"#;

#[derive(Debug, Clone, Deserialize)]
struct SliderDetectResult {
    #[serde(default, rename = "hasSlider")]
    has_slider: bool,
}

/// 页面是否存在拖动滑块类人机验证。
pub async fn page_has_slider(client: &mut CdpClient) -> Result<bool, String> {
    let v = client.evaluate_resilient(DETECT_SLIDER_JS).await?;
    let parsed: SliderDetectResult = serde_json::from_value(v)
        .map_err(|e| format!("解析滑块检测结果失败: {e}"))?;
    Ok(parsed.has_slider)
}

async fn wait_slider_if_present<R: Runtime>(
    app: Option<&AppHandle<R>>,
    waiters: &CaptchaWaiters,
    task_id: &str,
    client: &mut CdpClient,
    cancel: &AtomicBool,
    push_log: &impl Fn(String),
) -> Result<(), String> {
    // 登录点击后滑块可能晚几百毫秒才挂载
    let mut present = page_has_slider(client).await.unwrap_or(false);
    if !present {
        for _ in 0..6 {
            if cancel.load(Ordering::SeqCst) {
                return Err("已取消".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            if page_has_slider(client).await.unwrap_or(false) {
                present = true;
                break;
            }
        }
    }
    if !present {
        return Ok(());
    }

    push_log("检测到滑块验证，请在浏览器中拖动完成…".into());
    let _ = client.activate_tab().await;

    if let Some(bridge) = crate::human_bridge::FileHumanBridge::from_env() {
        let prompt_id = Uuid::new_v4().to_string();
        let req = crate::human_bridge::HumanRequest {
            prompt_id,
            task_id: task_id.to_string(),
            kind: captcha_kind_slider(),
            hint: "请完成滑块验证后回复「完成」；若页面滑块已消失也会自动继续".into(),
            image_path: None,
            image_base64: None,
        };
        bridge.write_request(&req, None)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        loop {
            if cancel.load(Ordering::SeqCst) {
                bridge.clear_request();
                return Err("已取消".into());
            }
            if std::time::Instant::now() >= deadline {
                bridge.clear_request();
                return Err("等待滑块验证超时（5 分钟）".into());
            }
            if !page_has_slider(client).await.unwrap_or(false) {
                bridge.clear_request();
                push_log("滑块验证已通过".into());
                return Ok(());
            }
            let has_reply = bridge.dir.join("human_reply.txt").is_file()
                || bridge.dir.join("human_reply.json").is_file();
            if has_reply {
                match bridge
                    .wait_reply(cancel, Duration::from_millis(100))
                    .await
                {
                    Ok(crate::human_bridge::HumanReply::Cancelled) => {
                        return Err("已取消滑块验证".into());
                    }
                    Ok(_) => {
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        if !page_has_slider(client).await.unwrap_or(false) {
                            push_log("滑块验证已通过".into());
                            return Ok(());
                        }
                        push_log("滑块似乎仍在，请确认已拖到位；将继续等待…".into());
                        bridge.write_request(&req, None)?;
                    }
                    Err(e) if e.contains("超时") => {}
                    Err(e) => return Err(e),
                }
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
    }

    let app = app.ok_or_else(|| {
        "无法弹出滑块提示：未启用文件人机桥（WEBCLAW_HUMAN_DIR），且无桌面 AppHandle".to_string()
    })?;

    let prompt_id = Uuid::new_v4().to_string();
    let (tx, mut rx) = oneshot::channel();
    waiters.lock().insert(prompt_id.clone(), tx);

    let image_base64 = match client.capture_screenshot_png().await {
        Ok(png) => base64::engine::general_purpose::STANDARD.encode(&png),
        Err(_) => String::new(),
    };
    let hint_text = if image_base64.is_empty() {
        "请到已置前的浏览器窗口中拖动滑块完成验证。完成后可点「我已完成」，或稍候程序自动继续。"
    } else {
        "请根据截图在浏览器中完成滑块验证。完成后可点「我已完成」，或稍候程序自动继续。"
    };

    let payload = CaptchaRequiredPayload {
        prompt_id: prompt_id.clone(),
        task_id: task_id.to_string(),
        kind: captcha_kind_slider(),
        image_base64,
        hint: hint_text.into(),
    };
    emit_captcha_required(app, &payload);

    const TIMEOUT: Duration = Duration::from_secs(300);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut user_said_done = false;
    let mut channel_open = true;

    loop {
        if cancel.load(Ordering::SeqCst) {
            waiters.lock().remove(&prompt_id);
            emit_captcha_resolved(app, task_id, &prompt_id);
            return Err("已取消".into());
        }
        if tokio::time::Instant::now() >= deadline {
            waiters.lock().remove(&prompt_id);
            emit_captcha_resolved(app, task_id, &prompt_id);
            return Err("等待滑块验证超时（5 分钟）".into());
        }

        if channel_open {
            match rx.try_recv() {
                Ok(CaptchaResponse::Cancelled) => {
                    emit_captcha_resolved(app, task_id, &prompt_id);
                    return Err("已取消滑块验证".into());
                }
                Ok(CaptchaResponse::Submitted(_)) => {
                    user_said_done = true;
                    channel_open = false;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => {
                    channel_open = false;
                    waiters.lock().remove(&prompt_id);
                    emit_captcha_resolved(app, task_id, &prompt_id);
                    return Err("滑块提示窗口已关闭".into());
                }
            }
        }

        let still = page_has_slider(client).await.unwrap_or(false);
        if !still {
            break;
        }
        if user_said_done {
            user_said_done = false;
            push_log("滑块似乎仍在，请确认已拖到位；将继续等待…".into());
        }

        tokio::time::sleep(Duration::from_millis(700)).await;
    }

    waiters.lock().remove(&prompt_id);
    emit_captcha_resolved(app, task_id, &prompt_id);
    push_log("滑块验证已通过".into());
    Ok(())
}

/// 解析「人机验证」参数：可选前缀超时秒 + 提示文案。
/// 例：`300 请拖动滑块` → (300, "请拖动滑块")；无秒数则默认 300。
pub fn parse_human_assist_args(args: &str) -> (u64, String) {
    let t = args.trim();
    if t.is_empty() {
        return (300, String::new());
    }
    let mut parts = t.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    if let Ok(n) = first.parse::<u64>() {
        if n >= 10 {
            let hint = parts.next().unwrap_or("").trim().to_string();
            return (n.min(3600), hint);
        }
    }
    (300, t.to_string())
}

/// 显式「人机验证」步骤：置前浏览器，等人点「我已完成」（不自动根据页面猜测结束）。
pub async fn wait_human_assist<R: Runtime>(
    app: &AppHandle<R>,
    waiters: &CaptchaWaiters,
    task_id: &str,
    client: &mut CdpClient,
    cancel: &AtomicBool,
    hint: &str,
    timeout_secs: u64,
    push_log: impl Fn(String),
) -> Result<(), String> {
    let hint = {
        let h = hint.trim();
        if h.is_empty() {
            "请在浏览器中完成需要人工操作的步骤（滑块、勾选协议等）。登录按钮常因此置灰，完成后再点「我已完成」，然后继续点击登录。".to_string()
        } else {
            h.to_string()
        }
    };
    push_log(format!("人机验证：{hint}"));
    // 放行新建页签暂停，避免用户手动点按钮 window.open 后整页假死
    crate::browser_network::force_release_new_target_pause(client.port).await;
    let _ = client.activate_tab().await;

    if let Some(bridge) = crate::human_bridge::FileHumanBridge::from_env() {
        let prompt_id = Uuid::new_v4().to_string();
        let req = crate::human_bridge::HumanRequest {
            prompt_id,
            task_id: task_id.to_string(),
            kind: captcha_kind_human(),
            hint: hint.clone(),
            image_path: None,
            image_base64: None,
        };
        bridge.write_request(&req, None)?;
        let timeout = Duration::from_secs(timeout_secs.clamp(10, 3600));
        match bridge.wait_reply(cancel, timeout).await? {
            crate::human_bridge::HumanReply::Cancelled => return Err("已取消人机验证".into()),
            crate::human_bridge::HumanReply::Confirmed | crate::human_bridge::HumanReply::Text(_) => {
                push_log("人机验证已确认完成".into());
                return Ok(());
            }
        }
    }

    let prompt_id = Uuid::new_v4().to_string();
    let (tx, mut rx) = oneshot::channel();
    waiters.lock().insert(prompt_id.clone(), tx);

    let image_base64 = match client.capture_screenshot_png().await {
        Ok(png) => base64::engine::general_purpose::STANDARD.encode(&png),
        Err(_) => String::new(),
    };

    let payload = CaptchaRequiredPayload {
        prompt_id: prompt_id.clone(),
        task_id: task_id.to_string(),
        kind: captcha_kind_human(),
        image_base64,
        hint: hint.clone(),
    };
    emit_captcha_required(app, &payload);

    let timeout_secs = timeout_secs.clamp(10, 3600);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if cancel.load(Ordering::SeqCst) {
            waiters.lock().remove(&prompt_id);
            emit_captcha_resolved(app, task_id, &prompt_id);
            return Err("已取消".into());
        }
        if tokio::time::Instant::now() >= deadline {
            waiters.lock().remove(&prompt_id);
            emit_captcha_resolved(app, task_id, &prompt_id);
            return Err(format!("人机验证超时（{timeout_secs} 秒）"));
        }

        match rx.try_recv() {
            Ok(CaptchaResponse::Cancelled) => {
                emit_captcha_resolved(app, task_id, &prompt_id);
                return Err("已取消人机验证".into());
            }
            Ok(CaptchaResponse::Submitted(_)) => {
                emit_captcha_resolved(app, task_id, &prompt_id);
                push_log("人机验证已确认完成".into());
                return Ok(());
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                waiters.lock().remove(&prompt_id);
                emit_captcha_resolved(app, task_id, &prompt_id);
                return Err("人机验证窗口已关闭".into());
            }
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod human_assist_args_tests {
    use super::parse_human_assist_args;

    #[test]
    fn parse_timeout_and_hint() {
        let (n, h) = parse_human_assist_args("");
        assert_eq!(n, 300);
        assert!(h.is_empty());

        let (n, h) = parse_human_assist_args("请拖动滑块");
        assert_eq!(n, 300);
        assert_eq!(h, "请拖动滑块");

        let (n, h) = parse_human_assist_args("120 请完成验证");
        assert_eq!(n, 120);
        assert_eq!(h, "请完成验证");
    }
}
