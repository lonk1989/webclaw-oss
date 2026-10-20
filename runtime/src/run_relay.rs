//! 远程运行进度中继客户端（对接 `apps/webclaw-run-relay`）。
//!
//! - 桌面：短连接 `register`/`auth` + `expect_run`，把 `runToken` 写入任务包
//! - worker：长连接 `worker_hello` + 镜像 `run_event`；可选收 `human_reply` 写人机目录

use crate::run_sink::{RunEvent, RunSink};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[cfg(feature = "desktop")]
#[derive(Debug, Clone)]
pub struct RelayCredentials {
    pub client_id: String,
    pub client_secret: String,
}

/// 解析中继 WS 地址：环境变量优先，否则读设置（空则内置公网）。
pub fn resolve_ws_url() -> Option<String> {
    if let Ok(v) = std::env::var("WEBCLAW_RUN_RELAY_WS") {
        let t = v.trim();
        if !t.is_empty() {
            return Some(normalize_ws_url(t));
        }
    }
    let s = crate::settings::load_settings().ok()?;
    if !s.run_relay_enabled {
        return None;
    }
    let t = crate::settings::resolve_run_relay_ws_url(&s);
    if t.trim().is_empty() {
        return None;
    }
    Some(normalize_ws_url(&t))
}

pub fn normalize_ws_url(raw: &str) -> String {
    let u = raw.trim().trim_end_matches('/');
    if u.ends_with("/ws") {
        u.to_string()
    } else {
        format!("{u}/ws")
    }
}

/// 从中继 WS 地址推导 HTTP 根（去掉 `/ws`，`wss`→`https`）。
pub fn relay_http_base_from_ws(ws: &str) -> String {
    let u = normalize_ws_url(ws);
    let base = u.trim_end_matches('/').trim_end_matches("/ws");
    if let Some(rest) = base.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = base.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        base.to_string()
    }
}

#[cfg(feature = "desktop")]
pub fn generate_run_token() -> Result<String, String> {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).map_err(|e| format!("生成 runToken 失败: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(feature = "desktop")]
fn load_or_empty_creds() -> RelayCredentials {
    let s = crate::settings::load_settings().unwrap_or_default();
    RelayCredentials {
        client_id: s.run_relay_client_id,
        client_secret: s.run_relay_client_secret,
    }
}

#[cfg(feature = "desktop")]
fn save_creds(creds: &RelayCredentials) -> Result<(), String> {
    let mut s = crate::settings::load_settings()?;
    s.run_relay_client_id = creds.client_id.clone();
    s.run_relay_client_secret = creds.client_secret.clone();
    crate::settings::save_settings(&s)
}

#[cfg(feature = "desktop")]
fn save_host_token_from_msg(msg: &Value) {
    if let Some(tok) = msg.get("hostToken").and_then(|v| v.as_str()) {
        let t = tok.trim();
        if t.is_empty() {
            return;
        }
        if let Ok(mut s) = crate::settings::load_settings() {
            if s.run_relay_host_token != t {
                s.run_relay_host_token = t.to_string();
                let _ = crate::settings::save_settings(&s);
            }
        }
    }
}

#[cfg(feature = "desktop")]
async fn read_json_msg(
    read: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err("中继响应超时".into());
        }
        let item = tokio::time::timeout(left, read.next())
            .await
            .map_err(|_| "中继响应超时".to_string())?
            .ok_or_else(|| "中继连接已关闭".to_string())?
            .map_err(|e| format!("读中继消息失败: {e}"))?;
        match item {
            Message::Text(t) => {
                return serde_json::from_str(&t).map_err(|e| format!("中继 JSON 无效: {e}"));
            }
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => continue,
            Message::Close(_) => return Err("中继连接已关闭".into()),
            Message::Frame(_) => continue,
        }
    }
}

/// 桌面短连接：确保凭证并 `expect_run`。成功返回中继建议写入任务包的 `relayWsUrl`。
#[cfg(feature = "desktop")]
pub async fn desktop_expect_run(
    ws_url: &str,
    job_id: &str,
    run_token: &str,
    title: &str,
) -> Result<String, String> {
    let url = normalize_ws_url(ws_url);
    let (ws, _) = connect_async(&url)
        .await
        .map_err(|e| format!("连接运行中继失败 ({url}): {e}"))?;
    let (mut write, mut read) = ws.split();

    let mut creds = load_or_empty_creds();
    if creds.client_id.trim().is_empty() || creds.client_secret.trim().is_empty() {
        write
            .send(Message::Text(
                json!({ "type": "register", "displayName": "webclaw-desktop" }).to_string(),
            ))
            .await
            .map_err(|e| format!("中继 register 失败: {e}"))?;
        let resp = read_json_msg(&mut read, Duration::from_secs(8)).await?;
        if resp.get("type").and_then(|v| v.as_str()) != Some("register_ok") {
            return Err(format!(
                "中继注册失败: {}",
                resp.get("message").and_then(|v| v.as_str()).unwrap_or("unknown")
            ));
        }
        creds.client_id = resp
            .get("clientId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        creds.client_secret = resp
            .get("clientSecret")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if creds.client_id.is_empty() || creds.client_secret.is_empty() {
            return Err("中继 register_ok 缺少凭证".into());
        }
        let _ = save_creds(&creds);
        save_host_token_from_msg(&resp);
        // register 后还会推 auth_ok，吞掉
        if let Ok(auth) = read_json_msg(&mut read, Duration::from_secs(3)).await {
            save_host_token_from_msg(&auth);
        }
    } else {
        write
            .send(Message::Text(
                json!({
                    "type": "auth",
                    "clientId": creds.client_id,
                    "clientSecret": creds.client_secret,
                })
                .to_string(),
            ))
            .await
            .map_err(|e| format!("中继 auth 失败: {e}"))?;
        let resp = read_json_msg(&mut read, Duration::from_secs(8)).await?;
        match resp.get("type").and_then(|v| v.as_str()) {
            Some("auth_ok") => {
                save_host_token_from_msg(&resp);
            }
            Some("error") => {
                // 凭证失效则重新注册
                write
                    .send(Message::Text(
                        json!({ "type": "register", "displayName": "webclaw-desktop" }).to_string(),
                    ))
                    .await
                    .map_err(|e| format!("中继 register 失败: {e}"))?;
                let reg = read_json_msg(&mut read, Duration::from_secs(8)).await?;
                if reg.get("type").and_then(|v| v.as_str()) != Some("register_ok") {
                    return Err("中继重新注册失败".into());
                }
                creds.client_id = reg
                    .get("clientId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                creds.client_secret = reg
                    .get("clientSecret")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let _ = save_creds(&creds);
                save_host_token_from_msg(&reg);
                if let Ok(auth) = read_json_msg(&mut read, Duration::from_secs(3)).await {
                    save_host_token_from_msg(&auth);
                }
            }
            _ => {
                return Err(format!(
                    "中继 auth 异常: {}",
                    resp.get("message").and_then(|v| v.as_str()).unwrap_or("unknown")
                ));
            }
        }
    }

    write
        .send(Message::Text(
            json!({
                "type": "expect_run",
                "jobId": job_id,
                "runToken": run_token,
                "title": title,
            })
            .to_string(),
        ))
        .await
        .map_err(|e| format!("中继 expect_run 失败: {e}"))?;
    let resp = read_json_msg(&mut read, Duration::from_secs(8)).await?;
    if resp.get("type").and_then(|v| v.as_str()) != Some("expect_ok") {
        return Err(format!(
            "中继 expect_run 失败: {}",
            resp.get("message").and_then(|v| v.as_str()).unwrap_or("unknown")
        ));
    }
    let hinted = resp
        .get("relayWsUrl")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| url.clone());
    let _ = write.close().await;
    Ok(hinted)
}

/// 同步封装：供 `dispatch_remote_job` 使用。
#[cfg(feature = "desktop")]
pub fn desktop_expect_run_blocking(
    ws_url: &str,
    job_id: &str,
    run_token: &str,
    title: &str,
) -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("创建运行时失败: {e}"))?;
    rt.block_on(desktop_expect_run(ws_url, job_id, run_token, title))
}

struct RelayRunSink {
    tx: mpsc::UnboundedSender<Value>,
}

impl RunSink for RelayRunSink {
    fn emit(&self, event: RunEvent) {
        let event = enrich_need_human_for_relay(event);
        if let Ok(v) = serde_json::to_value(&event) {
            let _ = self.tx.send(v);
        }
    }
}

fn enrich_need_human_for_relay(event: RunEvent) -> RunEvent {
    match event {
        RunEvent::NeedHuman {
            kind,
            prompt_id,
            hint,
            image_path,
            reply_file,
            inbox_image,
            image_base64: None,
        } => {
            let path = image_path.as_ref().or(inbox_image.as_ref());
            let image_base64 = path.and_then(|p| {
                std::fs::read(p).ok().map(|b| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD.encode(b)
                })
            });
            RunEvent::NeedHuman {
                kind,
                prompt_id,
                hint,
                image_path,
                reply_file,
                inbox_image,
                image_base64,
            }
        }
        other => other,
    }
}

pub struct WorkerRelaySession {
    pub sink: Arc<dyn RunSink>,
    _shutdown: oneshot::Sender<()>,
}

fn apply_human_reply(msg: &Value) {
    let dir = match std::env::var("WEBCLAW_HUMAN_DIR") {
        Ok(d) if !d.trim().is_empty() => std::path::PathBuf::from(d),
        _ => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Some(text) = msg.get("text").and_then(|v| v.as_str()) {
        let t = text.trim();
        if !t.is_empty() {
            let _ = std::fs::write(dir.join("human_reply.txt"), t);
            return;
        }
    }
    if msg.get("cancel").and_then(|v| v.as_bool()) == Some(true) {
        let _ = std::fs::write(dir.join("human_reply.json"), r#"{"cancel":true}"#);
        return;
    }
    if msg.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        let _ = std::fs::write(dir.join("human_reply.json"), r#"{"ok":true}"#);
    }
}

/// worker 挂上中继；失败不阻断本地 JSONL 执行。
pub async fn attach_worker(
    ws_url: &str,
    job_id: &str,
    run_token: &str,
) -> Result<WorkerRelaySession, String> {
    let url = normalize_ws_url(ws_url);
    let (ws, _) = connect_async(&url)
        .await
        .map_err(|e| format!("worker 连接运行中继失败 ({url}): {e}"))?;
    let (mut write, mut read) = ws.split();
    write
        .send(Message::Text(
            json!({
                "type": "worker_hello",
                "jobId": job_id,
                "runToken": run_token,
            })
            .to_string(),
        ))
        .await
        .map_err(|e| format!("worker_hello 发送失败: {e}"))?;

    // 等 hello_ok / error（短超时，超时仍继续推事件）
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(Message::Text(t))) = read.next().await {
            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                match v.get("type").and_then(|x| x.as_str()) {
                    Some("worker_hello_ok") => return Ok(()),
                    Some("error") => {
                        return Err(v
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("hello failed")
                            .to_string());
                    }
                    _ => continue,
                }
            }
        }
        Err("中继无响应".into())
    })
    .await;

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let job_id = job_id.to_string();
    let run_token = run_token.to_string();

    tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(25));
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    let _ = write.close().await;
                    break;
                }
                _ = ping.tick() => {
                    let _ = write.send(Message::Text(json!({ "type": "ping" }).to_string())).await;
                }
                ev = rx.recv() => {
                    match ev {
                        Some(event) => {
                            let msg = json!({
                                "type": "run_event",
                                "jobId": job_id,
                                "runToken": run_token,
                                "event": event,
                            });
                            if write.send(Message::Text(msg.to_string())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(t))) => {
                            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                                match v.get("type").and_then(|x| x.as_str()) {
                                    Some("human_reply") => {
                                        apply_human_reply(&v);
                                        if v.get("cancel").and_then(|x| x.as_bool()) == Some(true)
                                        {
                                            crate::task_run::cancel_current_job();
                                        }
                                    }
                                    Some("cancel_job") => {
                                        apply_human_reply(&json!({ "cancel": true }));
                                        crate::task_run::cancel_current_job();
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = write.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(_)) => break,
                        _ => {}
                    }
                }
            }
        }
    });

    Ok(WorkerRelaySession {
        sink: Arc::new(RelayRunSink { tx }),
        _shutdown: shutdown_tx,
    })
}

/// 可选：把最近一次连接错误记到 stderr，便于助手排查。
pub fn warn_relay(err: &str) {
    eprintln!("[webclaw-run-relay] {err}");
}
