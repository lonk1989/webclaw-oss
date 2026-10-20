use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex as SyncMutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

static MSG_ID: AtomicU64 = AtomicU64::new(1);
static CAPTURE_GEN: AtomicU64 = AtomicU64::new(1);

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    Message,
>;

struct BrowserWsInner {
    sink: Mutex<WsSink>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    alive: AtomicBool,
}

/// deny 后客户端回放：method + headers + body（POST /dev-api/…/export 等）
#[derive(Clone, Debug, Default)]
pub struct DownloadReplayRequest {
    pub method: String,
    pub headers: HashMap<String, String>,
    pub post_data: Option<String>,
}

#[derive(Default)]
struct CaptureState {
    download_urls: Vec<String>,
    /// downloadWillBegin 等给出的建议文件名（url → name）
    suggested_filenames: HashMap<String, String>,
    /// 下载类请求的原始 method/headers/body（url → 回放），供 deny 后客户端鉴权
    download_replays: HashMap<String, DownloadReplayRequest>,
    recent_urls: Vec<String>,
    attached_sessions: HashSet<String>,
    /// 自动化当前关注的父标签（用于识别 window.open 子页）
    watch_opener: Option<String>,
    /// targetCreated 且 opener 匹配 watch_opener 时登记，避免被空白弹窗清理误关
    tracked_children: Vec<String>,
    /// 是否对新建 target 先暂停（短命下载页签竞态）
    pause_new_targets: bool,
    /// 推迟关闭 pause 的世代号（避免旧任务误关新 pause）
    pause_release_gen: u64,
    /// 仅自动化下载模式为 true：才对附着 session 开 Network/Debugger（否则复杂页刷新极慢）
    instrument_sessions: bool,
}

/// Browser 级 Network 监听（含 OOPIF / 子 frame target），捕获导出请求 URL。
pub struct BrowserNetworkCapture {
    port: u16,
    generation: u64,
    inner: Arc<BrowserWsInner>,
    state: Arc<SyncMutex<CaptureState>>,
    read_task: tokio::task::JoinHandle<()>,
}

static PORT_CAPTURES: std::sync::OnceLock<SyncMutex<HashMap<u16, Arc<BrowserNetworkCapture>>>> =
    std::sync::OnceLock::new();

/// 每个调试端口期望的下载模式（Browser WS 重连后立刻恢复，避免 deny/instrument 丢失）。
static DESIRED_AUTOMATION_DOWNLOAD: std::sync::OnceLock<SyncMutex<HashMap<u16, bool>>> =
    std::sync::OnceLock::new();

fn desired_automation_download(port: u16) -> bool {
    DESIRED_AUTOMATION_DOWNLOAD
        .get_or_init(|| SyncMutex::new(HashMap::new()))
        .lock()
        .get(&port)
        .copied()
        .unwrap_or(false)
}

fn set_desired_automation_download(port: u16, automation: bool) {
    DESIRED_AUTOMATION_DOWNLOAD
        .get_or_init(|| SyncMutex::new(HashMap::new()))
        .lock()
        .insert(port, automation);
}

fn is_closed_ws_error(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("closed connection")
        || e.contains("connection closed")
        || e.contains("already closed")
        || e.contains("websocket is closed")
        || e.contains("browser cdp 连接已断开")
        || e.contains("broken pipe")
        || e.contains("os error 10054")
        || e.contains("os error 32")
}

fn format_browser_cdp_error(method: &str, err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    if lower.contains("downloadpath") {
        return format!("{method} 失败：缺少绝对 downloadPath");
    }
    if lower.contains("wasn't found")
        || lower.contains("not found")
        || lower.contains("method not found")
        || lower.contains("not available")
        || err.contains("不可用")
    {
        return format!("{method} 不可用");
    }
    format!("{method} 失败: {err}")
}

impl BrowserNetworkCapture {
    fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst) && !self.read_task.is_finished()
    }

    /// 每个调试端口复用一条 Browser WS；连接断开后自动重建。
    pub async fn shared_for_port(port: u16) -> Result<Arc<Self>, String> {
        let map = PORT_CAPTURES.get_or_init(|| SyncMutex::new(HashMap::new()));
        if let Some(existing) = map.lock().get(&port).cloned() {
            if existing.is_alive() {
                return Ok(existing);
            }
            drop_port_capture(port);
        }
        let cap = Self::start(port).await?;
        map.lock().insert(port, cap.clone());
        Ok(cap)
    }

    pub async fn start(port: u16) -> Result<Arc<Self>, String> {
        let ws_url = crate::chrome::browser_ws_url_on(port).await?;
        let (ws, _) = connect_async(&ws_url)
            .await
            .map_err(|e| format!("Browser CDP 连接失败: {e}"))?;
        let (sink, mut stream) = ws.split();
        let generation = CAPTURE_GEN.fetch_add(1, Ordering::SeqCst);
        let inner = Arc::new(BrowserWsInner {
            sink: Mutex::new(sink),
            pending: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
        });
        let state = Arc::new(SyncMutex::new(CaptureState::default()));
        let reader_inner = inner.clone();
        let reader_state = state.clone();
        let read_task = tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let Ok(Message::Text(text)) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };

                if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                    let params = v.get("params").cloned().unwrap_or(Value::Null);
                    let session_id = v.get("sessionId").and_then(|s| s.as_str());
                    handle_browser_event(
                        port,
                        &reader_inner,
                        &reader_state,
                        method,
                        &params,
                        session_id,
                    )
                    .await;
                    continue;
                }

                let Some(id) = v.get("id").and_then(|x| x.as_u64()) else { continue };
                let mut pending = reader_inner.pending.lock().await;
                if let Some(tx) = pending.remove(&id) {
                    let result = if let Some(err) = v.get("error") {
                        Err(err.to_string())
                    } else {
                        Ok(v)
                    };
                    let _ = tx.send(result);
                }
            }
            reader_inner.alive.store(false, Ordering::SeqCst);
            {
                let mut pending = reader_inner.pending.lock().await;
                for (_, tx) in pending.drain() {
                    let _ = tx.send(Err("Browser CDP 连接已断开".into()));
                }
            }
            drop_port_capture_if_gen(port, generation);
        });

        let capture = Arc::new(Self {
            port,
            generation,
            inner,
            state,
            read_task,
        });

        capture
            .call("Target.setDiscoverTargets", json!({ "discover": true }), None)
            .await?;
        // 空闲默认不 autoAttach：否则登录结束后用户点菜单 window.open 会被 CDP 挂上/误关弹窗
        let auto = desired_automation_download(port);
        capture
            .call(
                "Target.setAutoAttach",
                json!({
                    "autoAttach": auto,
                    "waitForDebuggerOnStart": false,
                    "flatten": true
                }),
                None,
            )
            .await?;
        // 不在此处改下载策略：由 enter/exit/ensure 显式切换，避免监听器重建时把运行中的 deny 冲掉
        // 但若期望仍是自动化模式（任务中途 WS 重建），立刻补回 deny + instrument
        if auto {
            let _ = capture.set_download_behavior(true).await;
        }
        Ok(capture)
    }

    async fn apply_auto_attach(&self, auto_attach: bool, wait_for_debugger: bool) -> Result<(), String> {
        self.call(
            "Target.setAutoAttach",
            json!({
                "autoAttach": auto_attach,
                "waitForDebuggerOnStart": wait_for_debugger,
                "flatten": true
            }),
            None,
        )
        .await?;
        Ok(())
    }

    /// `automation=true`：deny + 事件，客户端拉取；`false`：default，恢复 Chrome 本地下载 UI
    /// （不可用 allow 且省略 downloadPath：新版 Chrome 会直接拒绝该方法）
    pub async fn set_download_behavior(&self, automation: bool) -> Result<(), String> {
        set_desired_automation_download(self.port, automation);
        let (was_instrument, pause) = {
            let mut st = self.state.lock();
            let was = st.instrument_sessions;
            st.instrument_sessions = automation;
            if !automation {
                st.pause_new_targets = false;
                st.watch_opener = None;
                st.tracked_children.clear();
                st.pause_release_gen = st.pause_release_gen.wrapping_add(1);
            }
            (was, st.pause_new_targets)
        };
        if automation {
            // 仅「首次进入」自动化时改 setAutoAttach。
            // 已在自动化时不要重发：否则会把 arm_ephemeral 的 waitForDebuggerOnStart 冲掉；
            // pause 只由 set_pause_new_targets 管理。
            if !was_instrument {
                let _ = self.apply_auto_attach(true, pause).await;
            }
            self.call(
                "Browser.setDownloadBehavior",
                json!({
                    "behavior": "deny",
                    "eventsEnabled": true
                }),
                None,
            )
            .await?;
            // 已附着 session 补开 Network（新建 target 走 attachedToTarget）
            let sessions: Vec<String> = self.state.lock().attached_sessions.iter().cloned().collect();
            for sid in sessions {
                enable_network_on_session(&self.inner, &sid).await;
                // pause 期间勿对全部 session 抢先 runIfWaitingForDebugger，
                // 以免与 attachedToTarget 里「先注入再 resume」竞态
                if !pause {
                    apply_automation_debugger_policy_on_session(&self.inner, &sid).await;
                }
            }
        } else {
            // 空闲：先摘掉 autoAttach，再恢复本地下载，避免手动点菜单被 CDP 干涉
            let _ = self.apply_auto_attach(false, false).await;
            // default：不要求 downloadPath，等同用户手动使用 Chrome 下载栏
            if self
                .call(
                    "Browser.setDownloadBehavior",
                    json!({
                        "behavior": "default",
                        "eventsEnabled": false
                    }),
                    None,
                )
                .await
                .is_err()
            {
                // 极老版本若不认 default，再试 allow + Downloads 绝对路径
                let fallback_path = std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(|h| {
                        std::path::PathBuf::from(h)
                            .join("Downloads")
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_else(|| ".".into());
                let _ = self
                    .call(
                        "Browser.setDownloadBehavior",
                        json!({
                            "behavior": "allow",
                            "downloadPath": fallback_path,
                            "eventsEnabled": false
                        }),
                        None,
                    )
                    .await;
            }
            let sessions: Vec<String> = self.state.lock().attached_sessions.iter().cloned().collect();
            for sid in sessions {
                disable_network_on_session(&self.inner, &sid).await;
                // Debugger.disable 减轻常驻附着对刷新的拖累
                disable_debugger_on_session(&self.inner, &sid).await;
            }
            // 空闲不再保留 flatten session 列表语义；清空以免误以为还在监听
            self.state.lock().attached_sessions.clear();
        }
        Ok(())
    }

    pub fn drain_download_urls(&self) -> Vec<String> {
        let mut st = self.state.lock();
        std::mem::take(&mut st.download_urls)
    }

    /// 取出并清除某 URL 的建议文件名（来自 CDP downloadWillBegin / Content-Disposition）
    pub fn take_suggested_filename(&self, url: &str) -> Option<String> {
        let mut st = self.state.lock();
        if let Some(n) = st.suggested_filenames.remove(url) {
            return Some(n);
        }
        let key = st
            .suggested_filenames
            .keys()
            .filter(|k| url.starts_with(k.as_str()) || k.starts_with(url))
            .max_by_key(|k| k.len())
            .cloned();
        key.and_then(|k| st.suggested_filenames.remove(&k))
    }

    /// 不取出：仅窥探建议文件名（拉取后若 CD 被 CORS 藏住可再查一次）
    pub fn peek_suggested_filename(&self, url: &str) -> Option<String> {
        let st = self.state.lock();
        if let Some(n) = st.suggested_filenames.get(url) {
            return Some(n.clone());
        }
        st.suggested_filenames
            .iter()
            .filter(|(k, _)| url.starts_with(k.as_str()) || k.starts_with(url))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, v)| v.clone())
    }

    /// 取出下载请求回放信息（method/headers/body；不含 Cookie）
    pub fn take_download_replay(&self, url: &str) -> Option<DownloadReplayRequest> {
        let mut st = self.state.lock();
        if let Some(r) = st.download_replays.remove(url) {
            return Some(r);
        }
        // URL 可能带瞬时 query 差异：按前缀最长匹配一次
        let key = st
            .download_replays
            .keys()
            .filter(|k| url.starts_with(k.as_str()) || k.starts_with(url))
            .max_by_key(|k| k.len())
            .cloned();
        key.and_then(|k| st.download_replays.remove(&k))
    }

    /// 兼容旧调用：只取 headers
    pub fn take_download_request_headers(&self, url: &str) -> Option<HashMap<String, String>> {
        self.take_download_replay(url).map(|r| r.headers)
    }

    pub fn diagnostic_summary(&self) -> String {
        let st = self.state.lock();
        let recent: Vec<_> = st.recent_urls.iter().rev().take(8).cloned().collect();
        format!(
            "browser会话={} | 下载队列={} | 近期请求: {}",
            st.attached_sessions.len(),
            st.download_urls.len(),
            if recent.is_empty() {
                "无".into()
            } else {
                recent.join(" | ")
            }
        )
    }

    pub async fn shutdown(self: Arc<Self>) {
        self.inner.alive.store(false, Ordering::SeqCst);
        self.read_task.abort();
        let _ = self.inner.sink.lock().await.close().await;
        drop_port_capture_if_gen(self.port, self.generation);
    }

    /// Browser 级激活标签（Chrome 前台切换）
    pub async fn activate_target(&self, target_id: &str) -> Result<(), String> {
        self.call(
            "Target.activateTarget",
            json!({ "targetId": target_id }),
            None,
        )
        .await?;
        Ok(())
    }

    /// 通过 Target.getTargets 获取带 openerId 的 page 列表
    pub async fn get_page_targets(&self) -> Result<Vec<crate::chrome::PageTargetWithOpener>, String> {
        let resp = self.call("Target.getTargets", json!({}), None).await?;
        let infos = resp
            .get("result")
            .and_then(|r| r.get("targetInfos"))
            .and_then(|t| t.as_array())
            .ok_or_else(|| "Target.getTargets 响应无效".to_string())?;
        let mut out = Vec::new();
        for info in infos {
            let kind = info.get("type").and_then(|x| x.as_str()).unwrap_or("");
            if kind != "page" {
                continue;
            }
            let id = info
                .get("targetId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let url = info
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let opener_id = info
                .get("openerId")
                .and_then(|x| x.as_str())
                .map(String::from);
            out.push(crate::chrome::PageTargetWithOpener {
                id,
                url,
                opener_id,
            });
        }
        Ok(out)
    }

    /// Target.getTargets 中的 iframe（OOPIF）；parentId 仅部分 Chrome 经 HTTP 暴露，CDP 侧常为空
    pub async fn get_iframe_targets(&self) -> Result<Vec<crate::chrome::TargetInfo>, String> {
        let resp = self.call("Target.getTargets", json!({}), None).await?;
        let infos = resp
            .get("result")
            .and_then(|r| r.get("targetInfos"))
            .and_then(|t| t.as_array())
            .ok_or_else(|| "Target.getTargets 响应无效".to_string())?;
        let mut out = Vec::new();
        for info in infos {
            let kind = info.get("type").and_then(|x| x.as_str()).unwrap_or("");
            if kind != "iframe" {
                continue;
            }
            let id = info
                .get("targetId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let title = info
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let url = info
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let parent_id = info
                .get("parentId")
                .or_else(|| info.get("openerId"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            out.push(crate::chrome::TargetInfo {
                id,
                kind: "iframe".into(),
                title,
                url,
                parent_id,
            });
        }
        Ok(out)
    }

    pub fn set_watch_opener(&self, opener_id: &str) {
        let mut st = self.state.lock();
        if st.watch_opener.as_deref() != Some(opener_id) {
            st.tracked_children.clear();
        }
        st.watch_opener = Some(opener_id.to_string());
    }

    /// 短命下载页签：新建 target 先暂停，等挂上 Network 再 resume
    pub async fn set_pause_new_targets(&self, pause: bool) -> Result<(), String> {
        let automation = desired_automation_download(self.port);
        let need_call = {
            let mut st = self.state.lock();
            if st.pause_new_targets == pause {
                // 空闲且要求不 pause：确保 autoAttach 已关
                if !pause && !automation {
                    true
                } else {
                    false
                }
            } else {
                st.pause_new_targets = pause;
                if pause {
                    st.pause_release_gen = st.pause_release_gen.wrapping_add(1);
                }
                true
            }
        };
        if !need_call {
            return Ok(());
        }
        // 自动化或显式 pause 才 autoAttach；空闲完全放手
        let auto_attach = automation || pause;
        self.apply_auto_attach(auto_attach, pause && auto_attach)
            .await?;
        Ok(())
    }

    /// 点击结束后延迟关闭 pause，给「导出全部」弹层生成下载链一点时间
    pub fn schedule_release_pause(&self, delay_ms: u64) {
        let gen = {
            let mut st = self.state.lock();
            st.pause_release_gen = st.pause_release_gen.wrapping_add(1);
            st.pause_release_gen
        };
        let port = self.port;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let Some(cap) = capture_for_port(port) else {
                return;
            };
            let should = {
                let st = cap.state.lock();
                st.pause_new_targets && st.watch_opener.is_none() && st.pause_release_gen == gen
            };
            if should {
                let _ = cap.set_pause_new_targets(false).await;
            }
        });
    }

    pub fn latest_tracked_child(&self, opener_id: &str) -> Option<String> {
        let st = self.state.lock();
        if st.watch_opener.as_deref() != Some(opener_id) {
            return None;
        }
        st.tracked_children.last().cloned()
    }

    pub fn remove_tracked_child(&self, opener_id: &str, target_id: &str) {
        let mut st = self.state.lock();
        if st.watch_opener.as_deref() == Some(opener_id) {
            st.tracked_children.retain(|id| id != target_id);
        }
    }

    pub fn take_tracked_child(&self, opener_id: &str) -> Option<String> {
        let mut st = self.state.lock();
        if st.watch_opener.as_deref() != Some(opener_id) {
            return None;
        }
        st.tracked_children.pop()
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        if !self.is_alive() {
            drop_port_capture_if_gen(self.port, self.generation);
            return Err("Browser CDP 连接已断开".into());
        }
        let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        let mut req = json!({ "id": id, "method": method, "params": params });
        if let Some(sid) = session_id {
            req["sessionId"] = json!(sid);
        }
        if let Err(e) = self
            .inner
            .sink
            .lock()
            .await
            .send(Message::Text(req.to_string()))
            .await
        {
            let msg = e.to_string();
            self.inner.alive.store(false, Ordering::SeqCst);
            drop_port_capture_if_gen(self.port, self.generation);
            if is_closed_ws_error(&msg) {
                return Err("Browser CDP 连接已断开".into());
            }
            return Err(msg);
        }
        let resp = tokio::time::timeout(std::time::Duration::from_secs(15), rx)
            .await
            .map_err(|_| format!("Browser CDP 超时: {method}"))?
            .map_err(|_| format!("Browser CDP 通道关闭: {method}"))?
            .map_err(|e| format_browser_cdp_error(method, &e))?;
        Ok(resp)
    }
}

async fn handle_browser_event(
    port: u16,
    inner: &Arc<BrowserWsInner>,
    state: &Arc<SyncMutex<CaptureState>>,
    method: &str,
    params: &Value,
    session_id: Option<&str>,
) {
    match method {
        "Target.targetCreated" | "Target.targetInfoChanged" => {
            if let Some(info) = params.get("targetInfo") {
                let target_id = info.get("targetId").and_then(|x| x.as_str());
                let url = info.get("url").and_then(|x| x.as_str()).unwrap_or("");
                let opener = info.get("openerId").and_then(|x| x.as_str());
                let kind = info.get("type").and_then(|x| x.as_str()).unwrap_or("");
                // 短命下载页签：URL 一出现在 targetInfo 就记下（页签可能立刻关闭）
                if !url.is_empty() && crate::client_download::looks_like_download_url(url) {
                    note_url(state, url, Some("download.bin"));
                }
                if method == "Target.targetCreated" && kind == "page" {
                    if let Some(tid) = target_id {
                        let is_tracked = {
                            let mut st = state.lock();
                            let watch = st.watch_opener.clone();
                            let track = match (watch.as_deref(), opener) {
                                (Some(watch_id), Some(opener_id)) => opener_id == watch_id,
                                // window.open 未带 openerId 时，在 watch 期间新建 page 视为子标签
                                (Some(_), None) => true,
                                _ => false,
                            };
                            if track {
                                st.tracked_children.push(tid.to_string());
                                true
                            } else {
                                false
                            }
                        };
                        if !is_tracked {
                            let (watch_active, instrument) = {
                                let st = state.lock();
                                (st.watch_opener.is_some(), st.instrument_sessions)
                            };
                            // 仅自动化期间清理卡死的空白弹窗；空闲误关用户菜单 window.open 会搞垮业务页
                            if instrument
                                && !watch_active
                                && opener.is_some()
                                && is_stuck_blank_popup_url(url)
                            {
                                let inner = inner.clone();
                                let tid = tid.to_string();
                                tokio::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                    let _ = close_target_if_still_blank(port, &inner, &tid).await;
                                });
                            }
                        }
                    }
                }
            }
        }
        "Target.attachedToTarget" => {
            if let Some(sid) = params.get("sessionId").and_then(|s| s.as_str()) {
                state.lock().attached_sessions.insert(sid.to_string());
                if let Some(info) = params.get("targetInfo") {
                    let url = info.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if !url.is_empty() && crate::client_download::looks_like_download_url(url) {
                        note_url(state, url, Some("download.bin"));
                    }
                }
                let waiting = params
                    .get("waitingForDebugger")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let kind = params
                    .get("targetInfo")
                    .and_then(|i| i.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                let (instrument, pause) = {
                    let st = state.lock();
                    (st.instrument_sessions, st.pause_new_targets)
                };
                if instrument {
                    enable_network_on_session(inner, sid).await;
                    // 必须在 resume 前注入：否则 isprint 页 onload 调 window.print 会弹原生预览。
                    // 禁止在 Browser WS 读循环里 await session_call_wait——会等不到回包而死锁，
                    // 表现为浏览器假死 + 后续 Browser.setDownloadBehavior 不可用。
                    if pause && waiting && kind == "page" {
                        let inner2 = Arc::clone(inner);
                        let sid2 = sid.to_string();
                        tokio::spawn(async move {
                            install_print_capture_before_resume(&inner2, &sid2).await;
                            apply_automation_debugger_policy_on_session(&inner2, &sid2).await;
                        });
                    } else {
                        apply_automation_debugger_policy_on_session(inner, sid).await;
                    }
                } else if waiting {
                    resume_if_waiting_for_debugger(inner, sid).await;
                }
            }
        }
        "Network.requestWillBeSent" => {
            let req = params.get("request");
            let method = req
                .and_then(|r| r.get("method"))
                .and_then(|m| m.as_str())
                .unwrap_or("GET");
            let url = req
                .and_then(|r| r.get("url"))
                .and_then(|u| u.as_str())
                .unwrap_or("");
            let looks_dl = crate::client_download::looks_like_download_url(url);
            // GET/HEAD：一律记近期 URL；像下载的再记回放头并入队
            // POST/PUT：只记回放，等 responseReceived / downloadWillBegin 再入队
            // （否则早于 Content-Disposition 入队，文件名会退化成路径末段 export）
            if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD") {
                note_url(state, url, None);
                if looks_dl {
                    note_download_replay(state, url, method, req);
                }
            } else if looks_dl {
                note_recent_only(state, url);
                note_download_replay(state, url, method, req);
            } else if !url.is_empty() {
                note_recent_only(state, url);
            }
        }
        "Network.responseReceived" => {
            if let Some(resp) = params.get("response") {
                let url = resp.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let mime = resp.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");
                let headers = resp.get("headers");
                let suggested = suggested_name_from_headers(headers);
                if crate::client_download::looks_like_download_url(url)
                    || looks_like_attachment_response(mime, headers)
                {
                    note_url(state, url, suggested.as_deref());
                } else {
                    note_recent_only(state, url);
                }
            }
        }
        "Browser.downloadWillBegin" | "Page.downloadWillBegin" => {
            let url = params.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let name = params
                .get("suggestedFilename")
                .and_then(|n| n.as_str())
                .filter(|s| !s.is_empty());
            if url.is_empty() {
                // skip
            } else if crate::client_download::looks_like_download_url(url)
                || name.is_some_and(filename_looks_like_export)
            {
                // 下载已被 behavior:deny，记下 URL 供客户端拉取
                note_url(state, url, name);
            } else {
                // WPS/CDN 等也会触发 downloadWillBegin，不入下载队列
                note_recent_only(state, url);
            }
        }
        _ => {
            let _ = session_id;
        }
    }
}

pub(crate) fn is_stuck_blank_popup_url(url: &str) -> bool {
    let u = url.trim();
    u.is_empty() || u == "about:blank" || u.starts_with("chrome://newtab")
}

fn opener_matches_watch(watch: &Option<String>, opener: &str) -> bool {
    watch.as_deref() == Some(opener)
}

pub(crate) fn capture_for_port(port: u16) -> Option<Arc<BrowserNetworkCapture>> {
    PORT_CAPTURES
        .get()
        .and_then(|map| map.lock().get(&port).cloned())
}

fn drop_port_capture_if_gen(port: u16, generation: u64) {
    if let Some(map) = PORT_CAPTURES.get() {
        let mut g = map.lock();
        if g.get(&port).map(|c| c.generation) == Some(generation) {
            g.remove(&port);
        }
    }
}

/// Chrome 重启或调试端口重置时丢弃该端口的 Browser WS 缓存。
pub fn drop_port_capture(port: u16) {
    if let Some(map) = PORT_CAPTURES.get() {
        if let Some(old) = map.lock().remove(&port) {
            old.inner.alive.store(false, Ordering::SeqCst);
            old.read_task.abort();
        }
    }
}

/// 用户点停止：失败该端口 Browser CDP 上挂起的调用（不关 Chrome、不拆 WS）。
pub fn abort_pending_on_port(port: u16, reason: &str) {
    let Some(cap) = capture_for_port(port) else {
        return;
    };
    let reason = reason.to_string();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let mut pending = cap.inner.pending.lock().await;
                for (_, tx) in pending.drain() {
                    let _ = tx.send(Err(reason.clone()));
                }
            });
        }
        Err(_) => {
            let mut pending = cap.inner.pending.blocking_lock();
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err(reason.clone()));
            }
        }
    }
}

pub fn abort_all_pending(reason: &str) {
    let caps: Vec<Arc<BrowserNetworkCapture>> = PORT_CAPTURES
        .get()
        .map(|m| m.lock().values().cloned().collect())
        .unwrap_or_default();
    let reason = reason.to_string();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                for cap in caps {
                    let mut pending = cap.inner.pending.lock().await;
                    for (_, tx) in pending.drain() {
                        let _ = tx.send(Err(reason.clone()));
                    }
                }
            });
        }
        Err(_) => {
            for cap in caps {
                let mut pending = cap.inner.pending.blocking_lock();
                for (_, tx) in pending.drain() {
                    let _ = tx.send(Err(reason.clone()));
                }
            }
        }
    }
}

async fn with_browser_capture<T, F, Fut>(port: u16, f: F) -> Result<T, String>
where
    F: Fn(Arc<BrowserNetworkCapture>) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last_err = None;
    for attempt in 0..2u8 {
        let cap = BrowserNetworkCapture::shared_for_port(port).await?;
        match f(cap).await {
            Ok(v) => return Ok(v),
            Err(e) if attempt == 0 && is_closed_ws_error(&e) => {
                drop_port_capture(port);
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| "Browser CDP 连接已断开".into()))
}

/// Browser 级激活指定标签（复用已有 Browser WS；断线自动重连一次）
pub async fn activate_target_on(port: u16, target_id: &str) -> Result<(), String> {
    let tid = target_id.to_string();
    with_browser_capture(port, |cap| {
        let tid = tid.clone();
        async move { cap.activate_target(&tid).await }
    })
    .await
}

/// 切换浏览器下载策略：自动化 deny / 空闲 default
pub async fn set_download_behavior_on(port: u16, automation: bool) -> Result<(), String> {
    with_browser_capture(port, move |cap| async move {
        cap.set_download_behavior(automation).await
    })
    .await
}

/// 在可能触发 window.open 的步骤前登记父标签，并暂停新建页签直到挂上 Network
pub async fn set_watch_opener_on(port: u16, opener_id: &str) {
    if let Ok(cap) = BrowserNetworkCapture::shared_for_port(port).await {
        // 任务进行中若 Browser WS 刚重建，补回 deny + instrument，否则新页签 pause 后无策略可恢复
        if desired_automation_download(port) && !cap.state.lock().instrument_sessions {
            let _ = cap.set_download_behavior(true).await;
        }
        cap.set_watch_opener(opener_id);
        let _ = cap.set_pause_new_targets(true).await;
    }
}

/// 点击步骤结束后清除 watch；延迟关闭 pause，覆盖短命下载页签
pub fn clear_watch_opener_on(port: u16) {
    if let Some(cap) = capture_for_port(port) {
        {
            let mut st = cap.state.lock();
            st.watch_opener = None;
            st.tracked_children.clear();
        }
        // 导出全部 → 新页签生成链 → 关闭：常见发生在点击后数百 ms～数秒
        cap.schedule_release_pause(20_000);
    }
}

/// 等待下载期间持续暂停新建页签并捕获
pub async fn arm_ephemeral_download_capture(port: u16) -> Result<(), String> {
    let cap = BrowserNetworkCapture::shared_for_port(port).await?;
    // 作废点击结束后的延迟释放，避免等待下载中途被关掉 pause
    {
        let mut st = cap.state.lock();
        st.pause_release_gen = st.pause_release_gen.wrapping_add(1);
    }
    cap.set_pause_new_targets(true).await
}

/// 是否处于「导出点击 / 等待下载」捕获窗口（与具体网站无关）
pub fn is_ephemeral_download_capture_armed(port: u16) -> bool {
    capture_for_port(port)
        .map(|cap| {
            let st = cap.state.lock();
            st.pause_new_targets || st.watch_opener.is_some()
        })
        .unwrap_or(false)
}

pub async fn disarm_ephemeral_download_capture(port: u16) {
    if let Some(cap) = capture_for_port(port) {
        let watching = cap.state.lock().watch_opener.is_some();
        if !watching {
            let _ = cap.set_pause_new_targets(false).await;
        }
    }
}

/// 人机介入 / 空闲手动开新标签：立刻关掉 waitForDebugger，并在非自动化时摘掉 autoAttach
pub async fn force_release_new_target_pause(port: u16) {
    if let Some(cap) = capture_for_port(port) {
        {
            let mut st = cap.state.lock();
            st.pause_release_gen = st.pause_release_gen.wrapping_add(1);
            st.watch_opener = None;
            st.tracked_children.clear();
            st.pause_new_targets = false;
        }
        if desired_automation_download(port) {
            let _ = cap.apply_auto_attach(true, false).await;
        } else {
            let _ = cap.apply_auto_attach(false, false).await;
        }
    }
}

/// 主动「打开标签 / 登录换页」期间临时关掉 pause，避免新建 target 卡在 waitingForDebugger
pub async fn with_manual_new_tab_unpaused<F, Fut, T>(port: u16, f: F) -> Result<T, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let had_pause = capture_for_port(port)
        .map(|cap| cap.state.lock().pause_new_targets)
        .unwrap_or(false);
    if had_pause {
        if let Some(cap) = capture_for_port(port) {
            let _ = cap.set_pause_new_targets(false).await;
        }
    }
    let result = f().await;
    if had_pause {
        // 仅在自动化下载仍武装时恢复 pause；空闲不再挂起新页签
        if desired_automation_download(port) {
            if let Some(cap) = capture_for_port(port) {
                let _ = cap.set_pause_new_targets(true).await;
            }
        }
    }
    result
}

/// 手动把疑似下载 URL 写入捕获队列（短命页签跟随时）
pub fn note_download_url_on(port: u16, url: &str) {
    if let Some(cap) = capture_for_port(port) {
        note_url(&cap.state, url, Some("download.bin"));
    }
}

/// 把近期请求里「像下载且尚未成功/在途」的 URL 重新入队（等待步骤补捞）。
pub fn requeue_recent_download_urls(port: u16, session: &crate::run_artifacts::RunSession) {
    let Some(cap) = capture_for_port(port) else {
        return;
    };
    let recent: Vec<String> = {
        let st = cap.state.lock();
        st.recent_urls.iter().rev().take(24).cloned().collect()
    };
    for url in recent {
        if session.is_download_url_handled(&url) {
            continue;
        }
        if crate::client_download::looks_like_download_url(&url) {
            note_url(&cap.state, &url, Some("download.bin"));
        }
    }
}

/// 取出 automation 跟踪到的最新子标签
pub fn take_tracked_child_on(port: u16, opener_id: &str) -> Option<String> {
    capture_for_port(port)
        .and_then(|cap| cap.take_tracked_child(opener_id))
}

/// 查看最新跟踪子标签（不消费）
pub fn latest_tracked_child_on(port: u16, opener_id: &str) -> Option<String> {
    capture_for_port(port)
        .and_then(|cap| cap.latest_tracked_child(opener_id))
}

pub fn remove_tracked_child_on(port: u16, opener_id: &str, target_id: &str) {
    if let Some(cap) = capture_for_port(port) {
        cap.remove_tracked_child(opener_id, target_id);
    }
}

/// 获取带 openerId 的 page 目标列表
pub async fn get_page_targets_on(port: u16) -> Result<Vec<crate::chrome::PageTargetWithOpener>, String> {
    let cap = BrowserNetworkCapture::shared_for_port(port).await?;
    cap.get_page_targets().await
}

/// 获取 OOPIF iframe 目标（补充 /json/list）
pub async fn get_iframe_targets_on(port: u16) -> Result<Vec<crate::chrome::TargetInfo>, String> {
    let cap = BrowserNetworkCapture::shared_for_port(port).await?;
    cap.get_iframe_targets().await
}

/// 快照当前所有 page 标签 ID（用于检测 window.open 新标签）
pub async fn snapshot_page_ids_on(port: u16) -> Result<std::collections::HashSet<String>, String> {
    crate::chrome::snapshot_all_page_ids_on(port).await
}

async fn close_target_if_still_blank(
    port: u16,
    inner: &Arc<BrowserWsInner>,
    target_id: &str,
) -> Result<(), String> {
    if let Ok(pages) = crate::chrome::list_page_targets_on(port).await {
        if let Some(p) = pages.iter().find(|p| p.id == target_id) {
            if !is_stuck_blank_popup_url(&p.url) {
                return Ok(());
            }
        }
    }
    let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    inner.pending.lock().await.insert(id, tx);
    let req = json!({
        "id": id,
        "method": "Target.closeTarget",
        "params": { "targetId": target_id }
    });
    inner
        .sink
        .lock()
        .await
        .send(Message::Text(req.to_string()))
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx).await;
    Ok(())
}

async fn enable_network_on_session(inner: &Arc<BrowserWsInner>, session_id: &str) {
    let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
    // maxPostDataSize：把 POST /export 的 JSON body 带进 requestWillBeSent，供 deny 后回放
    let req = json!({
        "id": id,
        "method": "Network.enable",
        "params": { "maxPostDataSize": 1048576 },
        "sessionId": session_id
    });
    let _ = inner
        .sink
        .lock()
        .await
        .send(Message::Text(req.to_string()))
        .await;
}

async fn disable_network_on_session(inner: &Arc<BrowserWsInner>, session_id: &str) {
    let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
    let req = json!({
        "id": id,
        "method": "Network.disable",
        "params": {},
        "sessionId": session_id
    });
    let _ = inner
        .sink
        .lock()
        .await
        .send(Message::Text(req.to_string()))
        .await;
}

async fn disable_debugger_on_session(inner: &Arc<BrowserWsInner>, session_id: &str) {
    let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
    let req = json!({
        "id": id,
        "method": "Debugger.disable",
        "params": {},
        "sessionId": session_id
    });
    let _ = inner
        .sink
        .lock()
        .await
        .send(Message::Text(req.to_string()))
        .await;
}

async fn resume_if_waiting_for_debugger(inner: &Arc<BrowserWsInner>, session_id: &str) {
    let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
    let req = json!({
        "id": id,
        "method": "Runtime.runIfWaitingForDebugger",
        "params": {},
        "sessionId": session_id
    });
    let _ = inner
        .sink
        .lock()
        .await
        .send(Message::Text(req.to_string()))
        .await;
}

/// 在 waitForDebugger 暂停期间注入打印捕获（须在 runIfWaitingForDebugger 之前完成）。
async fn install_print_capture_before_resume(inner: &Arc<BrowserWsInner>, session_id: &str) {
    let boot = crate::client_download::CAPTURE_BOOTSTRAP_SCRIPT;
    let print_hook = crate::print_pdf::PRINT_HOOK_SCRIPT;
    let combined = format!("{boot}\n{print_hook}");
    // Page/Runtime 按序等待回包，避免未注入完就 resume
    let _ = session_call_wait(inner, session_id, "Page.enable", json!({})).await;
    let _ = session_call_wait(inner, session_id, "Runtime.enable", json!({})).await;
    let _ = session_call_wait(
        inner,
        session_id,
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": boot }),
    )
    .await;
    let _ = session_call_wait(
        inner,
        session_id,
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": print_hook }),
    )
    .await;
    let _ = session_call_wait(
        inner,
        session_id,
        "Runtime.evaluate",
        json!({
            "expression": combined,
            "returnByValue": true
        }),
    )
    .await;
}

async fn session_call_wait(
    inner: &Arc<BrowserWsInner>,
    session_id: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    if !inner.alive.load(Ordering::SeqCst) {
        return Err("Browser CDP 连接已断开".into());
    }
    let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    inner.pending.lock().await.insert(id, tx);
    let req = json!({
        "id": id,
        "method": method,
        "params": params,
        "sessionId": session_id
    });
    if let Err(e) = inner
        .sink
        .lock()
        .await
        .send(Message::Text(req.to_string()))
        .await
    {
        return Err(e.to_string());
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .map_err(|_| format!("Browser CDP 超时: {method}"))?
        .map_err(|_| format!("Browser CDP 通道关闭: {method}"))?
        .map_err(|e| format!("{method}: {e}"))
}

async fn apply_automation_debugger_policy_on_session(
    inner: &Arc<BrowserWsInner>,
    session_id: &str,
) {
    for (method, params) in crate::cdp::automation_debugger_policy_steps() {
        let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
        let req = json!({
            "id": id,
            "method": method,
            "params": params,
            "sessionId": session_id
        });
        let _ = inner
            .sink
            .lock()
            .await
            .send(Message::Text(req.to_string()))
            .await;
    }
}

fn note_recent_only(state: &Arc<SyncMutex<CaptureState>>, url: &str) {
    if url.is_empty() || url.starts_with("data:") || url.starts_with("blob:") {
        return;
    }
    let mut st = state.lock();
    if st.recent_urls.last() != Some(&url.to_string()) {
        st.recent_urls.push(url.to_string());
        if st.recent_urls.len() > 40 {
            let drain = st.recent_urls.len() - 40;
            st.recent_urls.drain(0..drain);
        }
    }
}

fn note_download_replay(
    state: &Arc<SyncMutex<CaptureState>>,
    url: &str,
    method: &str,
    req: Option<&Value>,
) {
    let mut map = HashMap::new();
    if let Some(obj) = req
        .and_then(|r| r.get("headers"))
        .and_then(|h| h.as_object())
    {
        for (k, v) in obj {
            let key = k.to_ascii_lowercase();
            // Cookie 由页内 credentials / getAllCookies 处理；勿把过期快照写死进回放
            if key == "cookie" || key == "content-length" || key == "host" {
                continue;
            }
            let val = match v {
                Value::String(s) => s.clone(),
                other => other.as_str().unwrap_or(&other.to_string()).to_string(),
            };
            if !val.is_empty() {
                map.insert(key, val);
            }
        }
    }
    let post_data = extract_request_post_data(req);
    // 无头且无 body 的纯 GET 不必占槽；有 Authorization/body 的 POST 必须记
    if map.is_empty() && post_data.is_none() && method.eq_ignore_ascii_case("GET") {
        return;
    }
    let replay = DownloadReplayRequest {
        method: method.to_ascii_uppercase(),
        headers: map,
        post_data,
    };
    let mut st = state.lock();
    st.download_replays.insert(url.to_string(), replay);
    // 防止无限涨：只留最近 32 条
    if st.download_replays.len() > 32 {
        let overflow = st.download_replays.len() - 32;
        let drop_keys: Vec<String> = st
            .download_replays
            .keys()
            .take(overflow)
            .cloned()
            .collect();
        for k in drop_keys {
            st.download_replays.remove(&k);
        }
    }
}

/// CDP Network.Request：优先 postData 文本；大 body 可能在 postDataEntries[].bytes（base64）
fn extract_request_post_data(req: Option<&Value>) -> Option<String> {
    let req = req?;
    if let Some(s) = req.get("postData").and_then(|x| x.as_str()) {
        if !s.is_empty() && s.len() <= 2 * 1024 * 1024 {
            return Some(s.to_string());
        }
    }
    let entries = req.get("postDataEntries")?.as_array()?;
    let mut raw = Vec::new();
    for ent in entries {
        let b64 = ent.get("bytes").and_then(|x| x.as_str())?;
        use base64::Engine;
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .ok()?;
        raw.extend(chunk);
        if raw.len() > 2 * 1024 * 1024 {
            return None;
        }
    }
    if raw.is_empty() {
        return None;
    }
    String::from_utf8(raw).ok().filter(|s| !s.is_empty())
}

fn note_url(state: &Arc<SyncMutex<CaptureState>>, url: &str, suggested: Option<&str>) {
    if url.is_empty() || url.starts_with("data:") || url.starts_with("blob:") {
        return;
    }
    let mut st = state.lock();
    if st.recent_urls.last() != Some(&url.to_string()) {
        st.recent_urls.push(url.to_string());
        if st.recent_urls.len() > 40 {
            let drain = st.recent_urls.len() - 40;
            st.recent_urls.drain(0..drain);
        }
    }
    // 仅「像导出的文件名」才强制入队；占位名 download.bin 不能强制（否则上传杂讯全进队列）
    let force = suggested.is_some_and(filename_looks_like_export);
    let looks = crate::client_download::looks_like_download_url(url);
    if (force || looks) && !st.download_urls.iter().any(|u| u == url) {
        st.download_urls.push(url.to_string());
        crate::client_download::trace_download_queue(url, suggested, force, looks);
    }
    if let Some(name) = suggested {
        if name != "download.bin" {
            st.suggested_filenames
                .insert(url.to_string(), name.to_string());
        }
    }
}

/// 丢弃尚未拉取的下载 URL 队列（上传步骤前清理，避免误抓）
pub fn clear_download_urls_on(port: u16) -> usize {
    let Some(cap) = capture_for_port(port) else {
        return 0;
    };
    let mut st = cap.state.lock();
    let n = st.download_urls.len();
    st.download_urls.clear();
    n
}

fn header_value_ci(headers: Option<&Value>, name: &str) -> String {
    let Some(headers) = headers else {
        return String::new();
    };
    let want = name.to_ascii_lowercase();
    if let Some(obj) = headers.as_object() {
        for (k, v) in obj {
            if k.to_ascii_lowercase() == want {
                return v.as_str().unwrap_or(&v.to_string()).to_string();
            }
        }
    }
    String::new()
}

fn suggested_name_from_headers(headers: Option<&Value>) -> Option<String> {
    let cd = header_value_ci(headers, "content-disposition");
    if cd.is_empty() {
        return None;
    }
    crate::client_download::filename_from_content_disposition_str_pub(&cd)
}

fn looks_like_attachment_response(mime: &str, headers: Option<&Value>) -> bool {
    let m = mime.to_ascii_lowercase();
    let cd = header_value_ci(headers, "content-disposition").to_ascii_lowercase();
    let cd_has_attach = cd.contains("attachment");
    let cd_name = content_disposition_filename_hint(&cd);

    // 明确的办公/压缩文档类型
    if m.contains("spreadsheet")
        || m.contains("excel")
        || m.contains("ms-excel")
        || m.contains("csv")
        || m.contains("zip")
        || m.contains("pdf")
        || m.contains("officedocument")
        || m.contains("msword")
    {
        return true;
    }

    // 裸 octet-stream 太常见（WPS 插件、埋点、字体包），不能单独当成下载；
    // 必须带 attachment，或文件名像导出文档。
    if m == "application/octet-stream" || m == "application/force-download" {
        if cd_has_attach && cd_name.as_deref().is_some_and(filename_looks_like_export) {
            return true;
        }
        if cd_name.as_deref().is_some_and(filename_looks_like_export) {
            return true;
        }
        return false;
    }

    // 其它 mime：仅当 Content-Disposition 声明附件且文件名像导出时
    cd_has_attach && cd_name.as_deref().is_some_and(filename_looks_like_export)
}

fn content_disposition_filename_hint(cd: &str) -> Option<String> {
    crate::client_download::filename_from_content_disposition_str_pub(cd)
}

fn filename_looks_like_export(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".xlsx")
        || n.ends_with(".xls")
        || n.ends_with(".csv")
        || n.ends_with(".pdf")
        || n.ends_with(".zip")
        || n.ends_with(".docx")
        || n.ends_with(".doc")
        || n.ends_with(".xlsm")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opener_matches_watch_detects_automation_parent() {
        assert!(opener_matches_watch(&Some("parent-1".into()), "parent-1"));
        assert!(!opener_matches_watch(&Some("parent-1".into()), "other"));
        assert!(!opener_matches_watch(&None, "parent-1"));
    }

    #[test]
    fn is_stuck_blank_popup_url_recognizes_blank_targets() {
        assert!(is_stuck_blank_popup_url("about:blank"));
        assert!(is_stuck_blank_popup_url("chrome://newtab/"));
        assert!(!is_stuck_blank_popup_url("https://example.com"));
    }
}
