use crate::cdp::CdpClient;
use crate::chrome;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;
use crate::ui_host::{AppHandle, Emitter, Runtime};

#[derive(Clone, Serialize)]
pub struct ActiveBrowserSession {
    pub port: u16,
    pub target_id: String,
    pub target_title: String,
    pub target_url: String,
    pub task_id: String,
}

#[derive(Clone, Serialize)]
pub struct BrowserSessionUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<ActiveBrowserSession>,
}

pub type ActiveBrowserSlot = Arc<Mutex<Option<ActiveBrowserSession>>>;

pub fn session_from_client(task_id: &str, client: &CdpClient) -> ActiveBrowserSession {
    ActiveBrowserSession {
        port: client.port,
        target_id: client.target_id.clone(),
        target_title: client.target_title.clone(),
        target_url: client.target_url.clone(),
        task_id: task_id.to_string(),
    }
}

pub fn set_active_browser<R: Runtime>(
    app: &AppHandle<R>,
    slot: &ActiveBrowserSlot,
    task_id: &str,
    client: &CdpClient,
) {
    let session = session_from_client(task_id, client);
    *slot.lock() = Some(session.clone());
    let _ = app.emit(
        "browser-session-update",
        BrowserSessionUpdate {
            session: Some(session),
        },
    );
}

pub fn clear_active_browser<R: Runtime>(app: &AppHandle<R>, slot: &ActiveBrowserSlot) {
    *slot.lock() = None;
    let _ = app.emit(
        "browser-session-update",
        BrowserSessionUpdate { session: None },
    );
}

/// 仅刷新 UI 状态栏（不占用执行中的 active_browser 槽）
pub fn emit_browser_session<R: Runtime>(
    app: &AppHandle<R>,
    session: Option<ActiveBrowserSession>,
) {
    let _ = app.emit(
        "browser-session-update",
        BrowserSessionUpdate { session },
    );
}

/// 任务结束后释放执行会话，若 Debug Chrome 仍运行则保持 idle 连接供页面探针使用
pub async fn release_active_browser<R: Runtime>(
    app: &AppHandle<R>,
    slot: &ActiveBrowserSlot,
    preferred_target_id: Option<&str>,
) {
    *slot.lock() = None;
    let session = idle_browser_session(preferred_target_id).await;
    emit_browser_session(app, session);
}

/// 空闲态会话：只用 /json/list 轻量读取，不附着 CDP。
/// （CDP attach/evaluate 在 macOS 上常会把 Debug Chrome 拉到前台，导致无法操作客户端。）
async fn idle_browser_session(preferred_target_id: Option<&str>) -> Option<ActiveBrowserSession> {
    if !chrome::is_debug_port_ready_on(chrome::DEBUG_PORT).await {
        return None;
    }
    let pages = chrome::list_page_targets_on(chrome::DEBUG_PORT).await.ok()?;
    let page = preferred_target_id
        .filter(|s| !s.is_empty())
        .and_then(|tid| pages.iter().find(|p| p.id == tid))
        .or_else(|| chrome::pick_page_target(&pages))?;
    Some(ActiveBrowserSession {
        port: chrome::DEBUG_PORT,
        target_id: page.id.clone(),
        target_title: page.title.clone(),
        target_url: page.url.clone(),
        task_id: "idle".into(),
    })
}

pub fn get_active_browser(slot: &ActiveBrowserSlot) -> Option<ActiveBrowserSession> {
    slot.lock().clone()
}

/// 任务执行中的会话；若无则回退到 Debug Chrome 当前标签（供页面探针等空闲操作）
pub async fn resolve_browser_session(
    slot: &ActiveBrowserSlot,
    preferred_target_id: Option<&str>,
) -> Option<ActiveBrowserSession> {
    if let Some(session) = get_active_browser(slot) {
        return Some(session);
    }
    idle_browser_session(preferred_target_id).await
}
