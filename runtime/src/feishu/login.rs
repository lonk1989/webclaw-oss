//! 飞书扫码登录：检测二维码 → 截图弹窗 → 等待用户扫码完成

use crate::cdp::CdpClient;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

const DETECT_JS: &str = r#"(() => {
  const url = location.href;
  const host = location.hostname || '';
  const text = (document.body && document.body.innerText) || '';
  const lower = text.toLowerCase();

  const isPassport = /passport\.feishu\.cn|accounts\.feishu\.cn|login\.feishu\.cn/i.test(url);
  const hasQrCopy = /扫码登录|扫描二维码|飞书扫码|扫码/.test(text);
  const onFeishuHost = /feishu\.cn|larksuite\.com/i.test(host);

  const isBitableLoaded = /\/base\/[A-Za-z0-9]+/.test(url)
    && !isPassport
    && !hasQrCopy
    && (document.querySelector('[class*="bitable"], [class*="Bitable"], [class*="base-view"]')
      || document.querySelector('[data-e2e*="bitable"]')
      || lower.includes('多维表格')
      || document.readyState === 'complete');

  if (isBitableLoaded) {
    return { state: 'logged_in' };
  }

  const needsQr = isPassport || (onFeishuHost && hasQrCopy);
  if (!needsQr) {
    // 多维表格：等 bitable 壳子出来（上方 isBitableLoaded 已覆盖已加载）
    if (onFeishuHost && /\/base\//.test(url)) {
      return { state: 'loading' };
    }
    // 审批/云盘等已登录业务页：无扫码文案且文档已 complete 即视为已登录
    if (onFeishuHost && document.readyState === 'complete') {
      return { state: 'logged_in' };
    }
    return { state: 'unknown' };
  }

  const dpr = window.devicePixelRatio || 1;
  const pickRect = (el) => {
    if (!el || !(el instanceof Element)) return null;
    const r = el.getBoundingClientRect();
    if (r.width < 80 || r.height < 80) return null;
    return { x: r.x, y: r.y, w: r.width, h: r.height, dpr };
  };

  let target = null;
  const candidates = [
    ...document.querySelectorAll('canvas'),
    ...document.querySelectorAll('img[src*="qr"], img[alt*="码"], img[class*="qr" i]'),
    ...document.querySelectorAll('[class*="qr" i], [class*="QR" i], [data-testid*="qr" i]'),
  ];
  for (const el of candidates) {
    const rect = pickRect(el);
    if (rect) { target = rect; break; }
  }

  if (!target) {
    const nodes = [...document.querySelectorAll('div, section')];
    for (const el of nodes) {
      const t = (el.innerText || '').trim();
      if (/扫码登录|扫描二维码/.test(t) && t.length < 80) {
        let p = el;
        for (let i = 0; i < 4 && p; i++) {
          const rect = pickRect(p);
          if (rect && rect.w >= 160 && rect.h >= 160) {
            target = rect;
            break;
          }
          p = p.parentElement;
        }
        if (target) break;
      }
    }
  }

  return {
    state: 'qr_login',
    clip: target,
  };
})()"#;

#[derive(Debug, Deserialize)]
struct DetectResult {
    state: String,
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
pub struct FeishuQrPayload {
    pub prompt_id: String,
    pub task_id: String,
    pub image_base64: String,
    pub hint: String,
}

#[derive(Clone, Serialize)]
struct FeishuQrResolvedPayload {
    task_id: String,
    prompt_id: String,
}

async fn detect(client: &mut CdpClient) -> Result<DetectResult, String> {
    client.ensure_attached().await?;
    let v = client.evaluate_resilient(DETECT_JS).await?;
    serde_json::from_value(v).map_err(|e| format!("解析飞书登录检测失败: {e}"))
}

async fn capture_qr_image(client: &mut CdpClient, info: &DetectResult) -> Result<Vec<u8>, String> {
    if let Some(clip) = &info.clip {
        if clip.w >= 40.0 && clip.h >= 40.0 {
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

/// 打开/登录飞书相关 URL 后是否需要等待扫码登录。
pub fn url_needs_login_wait(url: &str) -> bool {
    let u = url.to_lowercase();
    u.contains("feishu.cn") || u.contains("larksuite.com")
}

fn emit_qr_required<R: Runtime>(app: &AppHandle<R>, payload: &FeishuQrPayload) {
    let _ = app.emit("feishu-qr-required", payload);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        let _ = win.emit("feishu-qr-required", payload);
    }
    let label = crate::task_run::window_label(&payload.task_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.emit("feishu-qr-required", payload);
    }
}

fn emit_qr_resolved<R: Runtime>(app: &AppHandle<R>, task_id: &str, prompt_id: &str) {
    let payload = FeishuQrResolvedPayload {
        task_id: task_id.to_string(),
        prompt_id: prompt_id.to_string(),
    };
    let _ = app.emit("feishu-qr-resolved", &payload);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.emit("feishu-qr-resolved", &payload);
    }
    let label = crate::task_run::window_label(task_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.emit("feishu-qr-resolved", &payload);
    }
}

/// 打开飞书多维表后调用：若需扫码则截图弹窗并等待，直到登录完成或超时。
pub async fn ensure_feishu_logged_in<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    client: &mut CdpClient,
    cancel: Option<&AtomicBool>,
    push_log: impl Fn(String),
) -> Result<(), String> {
    const TIMEOUT: Duration = Duration::from_secs(600);
    const POLL: Duration = Duration::from_secs(2);
    const QR_REFRESH: Duration = Duration::from_secs(25);

    let started = Instant::now();
    let mut active_prompt: Option<String> = None;
    let mut last_qr_shot = Instant::now() - QR_REFRESH;
    let mut logged_waiting = false;

    loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            if let Some(pid) = active_prompt.take() {
                emit_qr_resolved(app, task_id, &pid);
            }
            return Err("已取消".into());
        }
        if started.elapsed() > TIMEOUT {
            if let Some(pid) = active_prompt.take() {
                emit_qr_resolved(app, task_id, &pid);
            }
            return Err("等待飞书扫码登录超时（10 分钟）".into());
        }

        let info = detect(client).await?;
        match info.state.as_str() {
            "logged_in" => {
                if let Some(pid) = active_prompt.take() {
                    emit_qr_resolved(app, task_id, &pid);
                }
                push_log("飞书已登录".into());
                return Ok(());
            }
            "qr_login" => {
                let need_shot = active_prompt.is_none() || last_qr_shot.elapsed() >= QR_REFRESH;
                if need_shot {
                    let png = capture_qr_image(client, &info).await?;
                    let prompt_id = active_prompt
                        .clone()
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    if active_prompt.is_none() {
                        push_log("检测到飞书扫码登录，请在弹窗中扫描二维码".into());
                        active_prompt = Some(prompt_id.clone());
                    } else {
                        push_log("已刷新飞书登录二维码".into());
                    }
                    last_qr_shot = Instant::now();
                    let payload = FeishuQrPayload {
                        prompt_id: prompt_id.clone(),
                        task_id: task_id.to_string(),
                        image_base64: base64::engine::general_purpose::STANDARD.encode(&png),
                        hint: "请使用飞书 App 扫描下方二维码完成登录。登录成功后窗口将自动关闭。".into(),
                    };
                    emit_qr_required(app, &payload);
                }
            }
            "loading" | "unknown" => {
                if active_prompt.is_none() && !logged_waiting {
                    push_log("等待飞书页面加载…".into());
                    logged_waiting = true;
                }
            }
            other => {
                push_log(format!("飞书页面状态: {other}"));
            }
        }

        tokio::time::sleep(POLL).await;
    }
}
