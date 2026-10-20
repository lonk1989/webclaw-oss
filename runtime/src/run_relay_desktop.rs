//! 桌面端长连运行中继：远程执行 / 进度 / 网站验证码回填。

#![cfg(feature = "desktop")]

use crate::recipe::Step;
use crate::remote_job::{self, JobAccount, JobRelayMeta, RemoteJobPlain};
use crate::run_relay::{self, generate_run_token, normalize_ws_url};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRelayStatus {
    pub connected: bool,
    pub host_online: bool,
    pub host_busy: bool,
    pub client_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteExecuteResult {
    pub job_id: String,
    pub run_token: String,
    pub title: String,
    pub accounts: Vec<String>,
    /// 宿主机离线：已生成任务码，等待伙伴兑换后自动下发
    #[serde(default)]
    pub need_pairing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair_expires_in_sec: Option<u32>,
    /// 伙伴提前退出后的续跑（只下发剩余步骤）
    #[serde(default)]
    pub is_continue: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slice_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairCodeResult {
    pub code: String,
    pub expires_in_sec: u32,
}

struct PendingRemoteJob {
    job_id: String,
    run_token: String,
    title: String,
    job_b64: String,
    accounts: Vec<String>,
    ws_url: String,
}

/// 整份任务留在桌面；每次只把一小段下发给短命 host。
struct ActiveRemoteRun {
    base_title: String,
    params: HashMap<String, String>,
    job_accounts: Vec<JobAccount>,
    steps: Vec<Step>,
    completed_through: usize,
    slice_origin: usize,
    slice_len: usize,
    job_id: String,
    run_token: String,
    /// 仍接受进度的那一轮 job（续跑换新 jobId 后，旧轮迟到日志仍要记上）
    progress_job_id: String,
    progress_slice_origin: usize,
    slice_started: bool,
    continue_prepared: bool,
    finished: bool,
    /// 整次远程执行共用，产物目录 / 浏览器 profile 都按它
    run_task_id: String,
}

static PENDING_REMOTE: Mutex<Option<PendingRemoteJob>> = Mutex::new(None);
static ACTIVE_REMOTE: Mutex<Option<ActiveRemoteRun>> = Mutex::new(None);

struct SessionInner {
    outbound: mpsc::UnboundedSender<String>,
    host_online: Arc<AtomicBool>,
    host_busy: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    client_id: Arc<Mutex<String>>,
}

static SESSION: Mutex<Option<SessionInner>> = Mutex::new(None);

fn emit_status(app: &AppHandle) {
    let st = status_snapshot();
    let _ = app.emit("run-relay-status", &st);
}

pub fn status_snapshot() -> RunRelayStatus {
    let snapshot = {
        let g = SESSION.lock();
        g.as_ref().map(|s| {
            (
                s.connected.load(Ordering::SeqCst),
                s.host_online.load(Ordering::SeqCst),
                s.host_busy.load(Ordering::SeqCst),
                s.client_id.clone(),
            )
        })
    };
    let Some((connected, host_online, host_busy, cid)) = snapshot else {
        return RunRelayStatus {
            connected: false,
            host_online: false,
            host_busy: false,
            client_id: String::new(),
        };
    };
    let client_id = cid.lock().clone();
    RunRelayStatus {
        connected,
        host_online,
        host_busy,
        client_id,
    }
}

/// 启动后台长连（幂等）。禁止在持有 SESSION 锁时再调 status_snapshot。
pub fn ensure_connected(app: AppHandle) {
    if SESSION.lock().is_some() {
        emit_status(&app);
        return;
    }
    let Some(ws_url) = run_relay::resolve_ws_url() else {
        return;
    };
    let url = normalize_ws_url(&ws_url);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let host_online = Arc::new(AtomicBool::new(false));
    let host_busy = Arc::new(AtomicBool::new(false));
    let connected = Arc::new(AtomicBool::new(false));
    let client_id = Arc::new(Mutex::new(String::new()));
    {
        let mut g = SESSION.lock();
        if g.is_some() {
            drop(g);
            emit_status(&app);
            return;
        }
        *g = Some(SessionInner {
            outbound: tx,
            host_online: host_online.clone(),
            host_busy: host_busy.clone(),
            connected: connected.clone(),
            client_id: client_id.clone(),
        });
    }

    tauri::async_runtime::spawn(async move {
        let mut backoff = 2u64;
        loop {
            match session_loop(
                &url,
                &mut rx,
                &app,
                &host_online,
                &host_busy,
                &connected,
                &client_id,
            )
            .await
            {
                Ok(()) => backoff = 2,
                Err(e) => {
                    eprintln!("[run-relay-desktop] {e}");
                    connected.store(false, Ordering::SeqCst);
                    host_online.store(false, Ordering::SeqCst);
                    emit_status(&app);
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                }
            }
        }
    });
}

async fn session_loop(
    url: &str,
    rx: &mut mpsc::UnboundedReceiver<String>,
    app: &AppHandle,
    host_online: &Arc<AtomicBool>,
    host_busy: &Arc<AtomicBool>,
    connected: &Arc<AtomicBool>,
    client_id: &Arc<Mutex<String>>,
) -> Result<(), String> {
    let (ws, _) = tokio::time::timeout(Duration::from_secs(12), connect_async(url))
        .await
        .map_err(|_| format!("连接中继超时 ({url})"))?
        .map_err(|e| format!("连接失败 ({url}): {e}"))?;
    let (mut write, mut read) = ws.split();

    // auth / register
    let s = crate::settings::load_settings().unwrap_or_default();
    if s.run_relay_client_id.trim().is_empty() || s.run_relay_client_secret.trim().is_empty() {
        write
            .send(Message::Text(
                json!({ "type": "register", "displayName": "webclaw-desktop" }).to_string(),
            ))
            .await
            .map_err(|e| e.to_string())?;
    } else {
        write
            .send(Message::Text(
                json!({
                    "type": "auth",
                    "clientId": s.run_relay_client_id,
                    "clientSecret": s.run_relay_client_secret,
                })
                .to_string(),
            ))
            .await
            .map_err(|e| e.to_string())?;
    }

    let mut ping = tokio::time::interval(Duration::from_secs(25));
    loop {
        tokio::select! {
            _ = ping.tick() => {
                let _ = write.send(Message::Text(json!({ "type": "ping" }).to_string())).await;
            }
            out = rx.recv() => {
                match out {
                    Some(text) => {
                        if write.send(Message::Text(text)).await.is_err() {
                            return Err("写中继失败".into());
                        }
                    }
                    None => return Err("会话已关闭".into()),
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        handle_inbound(app, &t, host_online, host_busy, connected, client_id);
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = write.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => return Err("中继断开".into()),
                    Some(Err(e)) => return Err(format!("读失败: {e}")),
                    _ => {}
                }
            }
        }
    }
}

fn handle_inbound(
    app: &AppHandle,
    raw: &str,
    host_online: &Arc<AtomicBool>,
    host_busy: &Arc<AtomicBool>,
    connected: &Arc<AtomicBool>,
    client_id: &Arc<Mutex<String>>,
) {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match ty {
        "register_ok" => {
            let cid = v
                .get("clientId")
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            let sec = v
                .get("clientSecret")
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            let tok = v
                .get("hostToken")
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            if let Ok(mut s) = crate::settings::load_settings() {
                s.run_relay_client_id = cid.to_string();
                s.run_relay_client_secret = sec.to_string();
                if !tok.is_empty() {
                    s.run_relay_host_token = tok.to_string();
                }
                let _ = crate::settings::save_settings(&s);
            }
            *client_id.lock() = cid.to_string();
        }
        "auth_ok" => {
            connected.store(true, Ordering::SeqCst);
            if let Some(cid) = v.get("clientId").and_then(|x| x.as_str()) {
                *client_id.lock() = cid.to_string();
            }
            if let Some(tok) = v.get("hostToken").and_then(|x| x.as_str()) {
                if let Ok(mut s) = crate::settings::load_settings() {
                    if s.run_relay_host_token != tok {
                        s.run_relay_host_token = tok.to_string();
                        let _ = crate::settings::save_settings(&s);
                    }
                }
            }
            let online = v
                .get("hostOnline")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            host_online.store(online, Ordering::SeqCst);
            host_busy.store(
                v.get("hostBusy").and_then(|x| x.as_bool()).unwrap_or(false),
                Ordering::SeqCst,
            );
            emit_status(app);
            if online && PENDING_REMOTE.lock().is_some() {
                let app2 = app.clone();
                tauri::async_runtime::spawn(async move {
                    match finish_pending_remote_execute(app2.clone()).await {
                        Ok(Some(res)) => {
                            let _ = app2.emit("run-relay-pending-started", &res);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("[run-relay-desktop] pending resume: {e}");
                            let _ = app2.emit(
                                "run-relay-pending-failed",
                                &json!({ "message": e }),
                            );
                        }
                    }
                });
            }
        }
        "host_online" => {
            host_online.store(true, Ordering::SeqCst);
            host_busy.store(false, Ordering::SeqCst);
            emit_status(app);
            let has_pending = PENDING_REMOTE.lock().is_some();
            if has_pending {
                let app2 = app.clone();
                tauri::async_runtime::spawn(async move {
                    match finish_pending_remote_execute(app2.clone()).await {
                        Ok(Some(res)) => {
                            let _ = app2.emit("run-relay-pending-started", &res);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("[run-relay-desktop] pending resume: {e}");
                            let _ = app2.emit(
                                "run-relay-pending-failed",
                                &json!({ "message": e }),
                            );
                        }
                    }
                });
            }
        }
        "host_offline" => {
            host_online.store(false, Ordering::SeqCst);
            host_busy.store(false, Ordering::SeqCst);
            emit_status(app);
            if should_offer_continue_on_offline() {
                spawn_maybe_continue_on_offline(app.clone());
            } else if !waiting_for_continue_code() {
                let _ = app.emit(
                    "run-relay-host-offline",
                    &json!({ "reason": v.get("reason").cloned().unwrap_or(json!("socket_closed")) }),
                );
            }
        }
        "start_job_ok" => {
            host_busy.store(true, Ordering::SeqCst);
            mark_slice_started();
            emit_status(app);
        }
        "worker_online" => {
            mark_slice_started();
            let _ = app.emit("run-relay-event", &v);
        }
        "remote_run_event" => {
            let slice_continue = apply_remote_event_progress(&v);
            let mut out = v.clone();
            if slice_continue {
                out["sliceContinue"] = json!(true);
            }
            let _ = app.emit("run-relay-event", &out);
            if let Some(ev) = v.get("event") {
                if ev.get("type").and_then(|x| x.as_str()) == Some("done") {
                    host_busy.store(false, Ordering::SeqCst);
                    host_online.store(false, Ordering::SeqCst);
                    emit_status(app);
                    if slice_continue {
                        spawn_maybe_continue(app.clone());
                    }
                }
                if ev.get("type").and_then(|x| x.as_str()) == Some("need_human") {
                    let _ = app.emit("run-relay-need-human", &v);
                }
            }
        }
        "host_idle" => {
            host_busy.store(false, Ordering::SeqCst);
            emit_status(app);
        }
        "worker_offline" | "human_reply_result" | "expect_ok" | "error"
        | "pong" | "cancel_job_ok" => {
            let _ = app.emit("run-relay-event", &v);
            if ty == "error" {
                eprintln!(
                    "[run-relay-desktop] error: {}",
                    v.get("message").and_then(|x| x.as_str()).unwrap_or("?")
                );
            }
        }
        _ => {
            let _ = app.emit("run-relay-event", &v);
        }
    }
}

fn send_raw(text: String) -> Result<(), String> {
    let g = SESSION.lock();
    let s = g.as_ref().ok_or("运行中继未连接")?;
    s.outbound
        .send(text)
        .map_err(|_| "运行中继发送失败".to_string())
}

fn mark_slice_started() {
    if let Some(a) = ACTIVE_REMOTE.lock().as_mut() {
        a.slice_started = true;
    }
}

fn waiting_for_continue_code() -> bool {
    ACTIVE_REMOTE
        .lock()
        .as_ref()
        .is_some_and(|a| a.continue_prepared && !a.finished)
}

fn remaining_count(a: &ActiveRemoteRun) -> usize {
    remote_job::remaining_steps_after(&a.steps, a.completed_through).len()
}

fn should_offer_continue_on_offline() -> bool {
    let g = ACTIVE_REMOTE.lock();
    let Some(a) = g.as_ref() else {
        return false;
    };
    if a.finished || a.continue_prepared || !a.slice_started {
        return false;
    }
    remaining_count(a) > 0
}

fn event_job_id(v: &Value) -> String {
    v.get("jobId")
        .or_else(|| v.get("job_id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn mark_slice_progress(a: &mut ActiveRemoteRun, slice_idx: usize) {
    if slice_idx == 0 {
        return;
    }
    let orig = a.progress_slice_origin.saturating_sub(1) + slice_idx;
    if orig > a.completed_through {
        a.completed_through = orig.min(a.steps.len());
    }
}

fn apply_remote_event_progress(v: &Value) -> bool {
    let job_id = event_job_id(v);
    let Some(ev) = v.get("event") else {
        return false;
    };
    let ty = ev.get("type").and_then(|x| x.as_str()).unwrap_or("");
    let mut g = ACTIVE_REMOTE.lock();
    let Some(a) = g.as_mut() else {
        return false;
    };
    if a.finished {
        return false;
    }
    if !job_id.is_empty() && job_id != a.progress_job_id && job_id != a.job_id {
        return false;
    }
    match ty {
        "step_start" | "step_ok" | "step_fail" => {
            a.slice_started = true;
            if ty == "step_ok" {
                let idx = ev.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                mark_slice_progress(a, idx);
            }
            false
        }
        "log" => {
            let msg = ev.get("message").and_then(|x| x.as_str()).unwrap_or("");
            if let Some((idx, _total, ok)) = remote_job::parse_step_progress_line(msg) {
                a.slice_started = true;
                if ok == Some(true) {
                    mark_slice_progress(a, idx);
                }
            }
            false
        }
        "done" => {
            let err = ev.get("error").and_then(|x| x.as_str()).unwrap_or("");
            if err.contains("已取消") {
                a.finished = true;
                return false;
            }
            let fail = ev.get("fail").and_then(|x| x.as_u64()).unwrap_or(0);
            let ok = ev.get("ok").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            if ok > 0 {
                mark_slice_progress(a, ok);
            } else if fail == 0 && a.slice_len > 0 {
                mark_slice_progress(a, a.slice_len);
            }
            remaining_count(a) > 0
        }
        _ => false,
    }
}

fn try_mark_continue_preparing(require_started: bool) -> bool {
    let mut g = ACTIVE_REMOTE.lock();
    let Some(a) = g.as_mut() else {
        return false;
    };
    if a.finished || a.continue_prepared {
        return false;
    }
    if require_started && !a.slice_started {
        return false;
    }
    if remaining_count(a) == 0 {
        a.finished = true;
        return false;
    }
    a.continue_prepared = true;
    true
}

fn spawn_maybe_continue(app: AppHandle) {
    spawn_maybe_continue_ex(app, false);
}

fn spawn_maybe_continue_on_offline(app: AppHandle) {
    spawn_maybe_continue_ex(app, true);
}

fn spawn_maybe_continue_ex(app: AppHandle, require_started: bool) {
    if !try_mark_continue_preparing(require_started) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        match maybe_prepare_continue(app.clone()).await {
            Ok(Some(res)) => {
                let _ = app.emit("run-relay-need-continue", &res);
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("[run-relay-desktop] continue: {e}");
                if let Some(a) = ACTIVE_REMOTE.lock().as_mut() {
                    a.continue_prepared = false;
                }
                let _ = app.emit("run-relay-pending-failed", &json!({ "message": e }));
            }
        }
    });
}

pub fn abandon_remote_run() {
    *PENDING_REMOTE.lock() = None;
    *ACTIVE_REMOTE.lock() = None;
}

fn slice_title(base: &str, origin: usize, slice_len: usize, total: usize) -> String {
    if total <= slice_len && origin <= 1 {
        return base.to_string();
    }
    let end = (origin + slice_len.saturating_sub(1)).min(total);
    format!("{base}（第{origin}–{end}/{total}步）")
}

fn continue_message(completed: usize, total: usize, next: usize, slice_end: usize) -> String {
    if completed == 0 {
        format!("伙伴端已提前退出。把继续任务码发给豆包工作伙伴，将从第 {next} 步接着跑（本轮第 {next}–{slice_end} 步，共 {total} 步），不会重跑整包。")
    } else {
        format!("伙伴端已提前退出（已完成 {completed}/{total} 步）。把继续任务码发给豆包工作伙伴，将从第 {next} 步接着跑（本轮第 {next}–{slice_end} 步），不会重跑已完成步骤。")
    }
}

async fn pend_slice_job(
    base_title: &str,
    params: HashMap<String, String>,
    job_accounts: Vec<JobAccount>,
    steps: &[Step],
    completed_through: usize,
    is_continue: bool,
    run_task_id: &str,
) -> Result<(RemoteExecuteResult, PendingRemoteJob, usize, usize, String, String), String> {
    let total = steps.len();
    let (slice, origin) = remote_job::take_remote_slice(
        steps,
        completed_through,
        remote_job::REMOTE_SLICE_MAX_STEPS,
    );
    if slice.is_empty() {
        return Err("没有剩余步骤可继续".into());
    }
    let slice_len = slice.len();
    let recipe = remote_job::join_steps_recipe(&slice);
    let next = origin;
    let slice_end = (origin + slice_len.saturating_sub(1)).min(total);
    let title = slice_title(base_title, origin, slice_len, total);

    let mut job = RemoteJobPlain {
        job_id: uuid::Uuid::new_v4().to_string(),
        title: title.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        recipe,
        params,
        accounts: job_accounts,
        relay: None,
        resume_browser: is_continue,
        run_task_id: run_task_id.to_string(),
    };
    let run_token = generate_run_token()?;
    let ws_url = run_relay::resolve_ws_url().ok_or("未配置运行中继")?;
    let hinted = run_relay::desktop_expect_run(&ws_url, &job.job_id, &run_token, &job.title).await?;
    job.relay = Some(JobRelayMeta {
        ws_url: hinted.clone(),
        run_token: run_token.clone(),
    });
    let bytes = remote_job::pack_job_bytes(&job)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let accounts: Vec<String> = job.accounts.iter().map(|a| a.name.clone()).collect();
    let pair = create_pair_code().await?;
    let pending = PendingRemoteJob {
        job_id: job.job_id.clone(),
        run_token: run_token.clone(),
        title: job.title.clone(),
        job_b64: b64,
        accounts: accounts.clone(),
        ws_url: hinted,
    };
    let completed = completed_through as u32;
    let msg = if is_continue {
        Some(continue_message(
            completed_through,
            total,
            next,
            slice_end,
        ))
    } else if total > slice_len {
        Some(format!(
            "任务共 {total} 步，本轮先跑第 {next}–{slice_end} 步。伙伴若提前退出，会再给你继续任务码。"
        ))
    } else {
        None
    };
    let res = RemoteExecuteResult {
        job_id: job.job_id,
        run_token,
        title: job.title,
        accounts,
        need_pairing: true,
        pair_code: Some(pair.code),
        pair_expires_in_sec: Some(pair.expires_in_sec),
        is_continue,
        completed: Some(completed),
        total_steps: Some(total as u32),
        next_index: Some(next as u32),
        slice_end: Some(slice_end as u32),
        message: msg,
    };
    let job_id = pending.job_id.clone();
    let token = pending.run_token.clone();
    Ok((res, pending, origin, slice_len, job_id, token))
}

async fn maybe_prepare_continue(app: AppHandle) -> Result<Option<RemoteExecuteResult>, String> {
    let snapshot = {
        let mut g = ACTIVE_REMOTE.lock();
        let Some(a) = g.as_mut() else {
            return Ok(None);
        };
        if a.finished {
            return Ok(None);
        }
        if remaining_count(a) == 0 {
            a.finished = true;
            return Ok(None);
        }
        (
            a.base_title.clone(),
            a.params.clone(),
            a.job_accounts.clone(),
            a.steps.clone(),
            a.completed_through,
            a.run_task_id.clone(),
        )
    };
    ensure_connected(app);
    let (res, pending, origin, slice_len, job_id, run_token) = match pend_slice_job(
        &snapshot.0,
        snapshot.1,
        snapshot.2,
        &snapshot.3,
        snapshot.4,
        true,
        &snapshot.5,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            if let Some(a) = ACTIVE_REMOTE.lock().as_mut() {
                a.continue_prepared = false;
            }
            return Err(e);
        }
    };
    *PENDING_REMOTE.lock() = Some(pending);
    if let Some(a) = ACTIVE_REMOTE.lock().as_mut() {
        a.job_id = job_id;
        a.run_token = run_token;
        a.slice_origin = origin;
        a.slice_len = slice_len;
        a.slice_started = false;
        // progress_job_id / progress_slice_origin 保持旧轮，直到新 host 真正开跑
    }
    Ok(Some(res))
}

fn relay_http_base_from_ws(ws: &str) -> String {
    run_relay::relay_http_base_from_ws(ws)
}

/// 向中继申请 6 位任务码（HTTP）。
pub async fn create_pair_code() -> Result<PairCodeResult, String> {
    let ws_url = run_relay::resolve_ws_url().ok_or("未配置运行中继")?;
    let http = relay_http_base_from_ws(&ws_url);
    let s = crate::settings::load_settings()?;
    if s.run_relay_client_id.trim().is_empty() || s.run_relay_client_secret.trim().is_empty() {
        return Err("运行中继尚未注册。请先打开设置并等待中继连接。".into());
    }
    let key = remote_job::ensure_local_secret()?;
    let job_secret = base64::engine::general_purpose::STANDARD.encode(key);
    let url = format!("{http}/api/pair/create");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(&url)
        .json(&json!({
            "clientId": s.run_relay_client_id,
            "clientSecret": s.run_relay_client_secret,
            "jobSecret": job_secret,
        }))
        .send()
        .await
        .map_err(|e| format!("创建任务码失败: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
    if !status.is_success() || v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("创建任务码失败");
        return Err(err.to_string());
    }
    let code = v
        .get("code")
        .and_then(|x| x.as_str())
        .ok_or("中继未返回任务码")?
        .to_string();
    let expires = v
        .get("expiresInSec")
        .or_else(|| v.get("expires_in_sec"))
        .and_then(|x| x.as_u64())
        .unwrap_or(600) as u32;
    Ok(PairCodeResult {
        code,
        expires_in_sec: expires,
    })
}

async fn dispatch_start_job(
    app: &AppHandle,
    ws_url: &str,
    job_id: &str,
    run_token: &str,
    title: &str,
    job_b64: &str,
) -> Result<(), String> {
    ensure_connected(app.clone());
    for _ in 0..25 {
        if status_snapshot().connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut started = false;
    if status_snapshot().connected {
        send_raw(
            json!({
                "type": "start_job",
                "jobId": job_id,
                "runToken": run_token,
                "title": title,
                "jobBase64": job_b64,
            })
            .to_string(),
        )?;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if status_snapshot().host_busy {
                started = true;
                break;
            }
        }
    }
    if !started {
        start_job_short(ws_url, job_id, run_token, title, job_b64).await?;
    }
    Ok(())
}

/// 宿主机上线后，下发等待中的远程任务（若有）。
pub async fn finish_pending_remote_execute(app: AppHandle) -> Result<Option<RemoteExecuteResult>, String> {
    let pending = { PENDING_REMOTE.lock().take() };
    let Some(p) = pending else {
        return Ok(None);
    };
    for _ in 0..50 {
        if status_snapshot().host_online {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !status_snapshot().host_online {
        *PENDING_REMOTE.lock() = Some(p);
        return Err("宿主机仍未在线".into());
    }
    if status_snapshot().host_busy {
        *PENDING_REMOTE.lock() = Some(p);
        return Err("宿主机忙碌".into());
    }
    dispatch_start_job(
        &app,
        &p.ws_url,
        &p.job_id,
        &p.run_token,
        &p.title,
        &p.job_b64,
    )
    .await?;
    if let Some(a) = ACTIVE_REMOTE.lock().as_mut() {
        if a.job_id == p.job_id {
            a.slice_started = true;
            a.continue_prepared = false;
            a.progress_job_id = p.job_id.clone();
            a.progress_slice_origin = a.slice_origin;
        }
    }
    let (completed, total_steps, next_index, slice_end) = {
        let g = ACTIVE_REMOTE.lock();
        g.as_ref()
            .map(|a| {
                (
                    Some(a.completed_through as u32),
                    Some(a.steps.len() as u32),
                    Some(a.slice_origin as u32),
                    Some((a.slice_origin + a.slice_len.saturating_sub(1)).min(a.steps.len()) as u32),
                )
            })
            .unwrap_or((None, None, None, None))
    };
    Ok(Some(RemoteExecuteResult {
        job_id: p.job_id,
        run_token: p.run_token,
        title: p.title,
        accounts: p.accounts,
        need_pairing: false,
        pair_code: None,
        pair_expires_in_sec: None,
        is_continue: false,
        completed,
        total_steps,
        next_index,
        slice_end,
        message: None,
    }))
}

/// 远程执行：桌面保留整份步骤，只把当前一小段打包下发；伙伴提前退出时用继续任务码接着跑。
pub async fn remote_execute(
    app: AppHandle,
    title: String,
    recipe: String,
    params: HashMap<String, String>,
) -> Result<RemoteExecuteResult, String> {
    ensure_connected(app.clone());
    for _ in 0..30 {
        if status_snapshot().connected || !status_snapshot().client_id.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    if status_snapshot().host_busy {
        return Err("宿主机正在执行其他任务。可点「终止任务」后再试。".into());
    }

    let full = remote_job::build_job_for_remote_execute(&title, &recipe, params.clone())?;
    let (steps, merged_params) = remote_job::executable_remote_steps(&recipe, &full.params)?;
    abandon_remote_run();

    let run_task_id = uuid::Uuid::new_v4().to_string();
    let (res, pending, origin, slice_len, job_id, run_token) = pend_slice_job(
        &full.title,
        merged_params.clone(),
        full.accounts.clone(),
        &steps,
        0,
        false,
        &run_task_id,
    )
    .await?;
    *PENDING_REMOTE.lock() = Some(pending);
    *ACTIVE_REMOTE.lock() = Some(ActiveRemoteRun {
        base_title: full.title,
        params: merged_params,
        job_accounts: full.accounts,
        steps,
        completed_through: 0,
        slice_origin: origin,
        slice_len,
        job_id: job_id.clone(),
        run_token,
        progress_job_id: job_id,
        progress_slice_origin: origin,
        slice_started: false,
        continue_prepared: false,
        finished: false,
        run_task_id,
    });
    Ok(res)
}

async fn start_job_short(
    ws_url: &str,
    job_id: &str,
    run_token: &str,
    title: &str,
    job_b64: &str,
) -> Result<(), String> {
    let url = normalize_ws_url(ws_url);
    let (ws, _) = connect_async(&url)
        .await
        .map_err(|e| format!("短连失败: {e}"))?;
    let (mut write, mut read) = ws.split();
    let s = crate::settings::load_settings()?;
    write
        .send(Message::Text(
            json!({
                "type": "auth",
                "clientId": s.run_relay_client_id,
                "clientSecret": s.run_relay_client_secret,
            })
            .to_string(),
        ))
        .await
        .map_err(|e| e.to_string())?;
    // drain until auth_ok
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err("短连 auth 超时".into());
        }
        let item = tokio::time::timeout(Duration::from_secs(2), read.next())
            .await
            .map_err(|_| "短连超时".to_string())?
            .ok_or_else(|| "短连关闭".to_string())?
            .map_err(|e| e.to_string())?;
        if let Message::Text(t) = item {
            let v: Value = serde_json::from_str(&t).unwrap_or(json!({}));
            match v.get("type").and_then(|x| x.as_str()) {
                Some("auth_ok") => break,
                Some("error") => {
                    return Err(v
                        .get("message")
                        .and_then(|x| x.as_str())
                        .unwrap_or("auth failed")
                        .to_string());
                }
                _ => continue,
            }
        }
    }
    write
        .send(Message::Text(
            json!({
                "type": "start_job",
                "jobId": job_id,
                "runToken": run_token,
                "title": title,
                "jobBase64": job_b64,
            })
            .to_string(),
        ))
        .await
        .map_err(|e| e.to_string())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err("start_job 超时".into());
        }
        let item = tokio::time::timeout(Duration::from_secs(3), read.next())
            .await
            .map_err(|_| "start_job 超时".to_string())?
            .ok_or_else(|| "短连关闭".to_string())?
            .map_err(|e| e.to_string())?;
        if let Message::Text(t) = item {
            let v: Value = serde_json::from_str(&t).unwrap_or(json!({}));
            match v.get("type").and_then(|x| x.as_str()) {
                Some("start_job_ok") => return Ok(()),
                Some("error") => {
                    return Err(v
                        .get("message")
                        .and_then(|x| x.as_str())
                        .unwrap_or("start_job failed")
                        .to_string());
                }
                _ => continue,
            }
        }
    }
}

pub fn human_reply(
    job_id: String,
    run_token: String,
    text: Option<String>,
    ok: Option<bool>,
    cancel: Option<bool>,
) -> Result<(), String> {
    send_raw(
        json!({
            "type": "human_reply",
            "jobId": job_id,
            "runToken": run_token,
            "text": text,
            "ok": ok,
            "cancel": cancel,
        })
        .to_string(),
    )
}

/// 终止宿主机当前任务。jobId / runToken 可空（中继用当前忙碌任务）。
pub async fn cancel_host_job(
    app: AppHandle,
    job_id: Option<String>,
    run_token: Option<String>,
) -> Result<(), String> {
    ensure_connected(app.clone());
    for _ in 0..20 {
        if status_snapshot().connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !status_snapshot().connected {
        return Err("运行中继未连接".into());
    }
    abandon_remote_run();
    send_raw(
        json!({
            "type": "cancel_job",
            "jobId": job_id.unwrap_or_default(),
            "runToken": run_token.unwrap_or_default(),
        })
        .to_string(),
    )?;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if !status_snapshot().host_busy {
            return Ok(());
        }
    }
    Ok(())
}
