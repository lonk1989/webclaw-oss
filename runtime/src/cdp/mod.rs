use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex as SyncMutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Weak;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

static MSG_ID: AtomicU64 = AtomicU64::new(1);
static LIVE_INNERS: std::sync::OnceLock<SyncMutex<Vec<Weak<CdpInner>>>> = std::sync::OnceLock::new();
/// 每个 target 只挂一次 `Page.addScriptToEvaluateOnNewDocument`（该 API 会跨会话累加，刷新时重复执行拖死复杂页）
static NEW_DOC_SCRIPT_IDS: std::sync::OnceLock<SyncMutex<HashMap<String, String>>> =
    std::sync::OnceLock::new();

#[derive(Clone, Copy)]
enum AttachMode {
    /// 自动化主路径：auto-guard + 等待 + createIsolatedWorld 补齐子 frame
    Full,
    /// 探针 clear / 短连 OOPIF：只开 Page+Runtime，避免反复造隔离世界撑原生内存
    Light,
}

/// 标签切换/隐藏时释放焦点，避免输入状态残留
const AUTOMATION_GUARD_SCRIPT: &str = r#"(function () {
  if (window.__webclawAutoGuard) return;
  window.__webclawAutoGuard = true;
  const blurActive = () => {
    const el = document.activeElement;
    if (!el || el === document.body || el === document.documentElement) return;
    try { el.blur(); } catch (_) {}
  };
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) blurActive();
  });
  window.addEventListener('pagehide', blurActive);
})();"#;

fn new_doc_script_ids() -> &'static SyncMutex<HashMap<String, String>> {
    NEW_DOC_SCRIPT_IDS.get_or_init(|| SyncMutex::new(HashMap::new()))
}

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    Message,
>;

#[derive(Debug, Clone)]
pub(crate) struct FileChooserOpened {
    backend_node_id: Option<u64>,
    mode: String,
}

struct CdpInner {
    port: u16,
    sink: Mutex<WsSink>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    /// 用户点停止：拒绝新调用并打断挂起请求（不杀浏览器）
    user_cancelled: AtomicBool,
    /// CDP Network 事件捕获到的下载 URL
    captured_urls: Mutex<Vec<String>>,
    /// frameId → 默认 executionContextId（同源 iframe 定位依赖此表；须用同步锁，事件回调不能 await）
    frame_contexts: SyncMutex<HashMap<String, u64>>,
    /// 拦截系统文件框后等待 `Page.fileChooserOpened`
    file_chooser_wait: Mutex<Option<oneshot::Sender<FileChooserOpened>>>,
    /// 点击「上传文件」等触发的 chooser（尚无「上传」步骤在等时先缓存）
    last_file_chooser: Mutex<Option<FileChooserOpened>>,
}

fn live_inners() -> &'static SyncMutex<Vec<Weak<CdpInner>>> {
    LIVE_INNERS.get_or_init(|| SyncMutex::new(Vec::new()))
}

fn register_live_inner(inner: &Arc<CdpInner>) {
    let mut list = live_inners().lock();
    list.retain(|w| w.strong_count() > 0);
    list.push(Arc::downgrade(inner));
}

impl CdpInner {
    /// 打断页内正在跑的长 JS（awaitPromise 的 waitUntil 等），否则点停止仍会空等满 elementWait。
    async fn terminate_active_execution(&self) {
        let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
        let req = json!({
            "id": id,
            "method": "Runtime.terminateExecution",
            "params": {}
        });
        let _ = self
            .sink
            .lock()
            .await
            .send(Message::Text(req.to_string()))
            .await;
        // 同步置位，供后续轮询脚本（若未及时 terminate）尽快退出
        let flag_id = MSG_ID.fetch_add(1, Ordering::SeqCst);
        let flag_req = json!({
            "id": flag_id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": "try{window.__webclawCancelRequested=true}catch(e){}",
                "returnByValue": true,
                "awaitPromise": false
            }
        });
        let _ = self
            .sink
            .lock()
            .await
            .send(Message::Text(flag_req.to_string()))
            .await;
    }

    fn terminate_active_execution_blocking(&self) {
        // 同步命令线程：尽力发送，拿不到 sink 锁则依赖 pending fail + cancel 轮询
        let Ok(mut sink) = self.sink.try_lock() else {
            return;
        };
        let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
        let req = json!({
            "id": id,
            "method": "Runtime.terminateExecution",
            "params": {}
        });
        let _ = futures_util::FutureExt::now_or_never(sink.send(Message::Text(req.to_string())));
        let flag_id = MSG_ID.fetch_add(1, Ordering::SeqCst);
        let flag_req = json!({
            "id": flag_id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": "try{window.__webclawCancelRequested=true}catch(e){}",
                "returnByValue": true,
                "awaitPromise": false
            }
        });
        let _ = futures_util::FutureExt::now_or_never(
            sink.send(Message::Text(flag_req.to_string())),
        );
    }

    async fn fail_all_pending(&self, reason: &str) {
        self.user_cancelled.store(true, Ordering::SeqCst);
        self.terminate_active_execution().await;
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    /// 无 Tokio 句柄时（托盘/同步命令线程）用阻塞锁打断挂起调用，避免 `tokio::spawn` panic。
    fn fail_all_pending_blocking(&self, reason: &str) {
        self.user_cancelled.store(true, Ordering::SeqCst);
        self.terminate_active_execution_blocking();
        let mut pending = self.pending.blocking_lock();
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(reason.to_string()));
        }
    }
}

/// 新一轮执行前清除 CDP 侧取消标记（与 clear_execution_cancel_cmd 配合）
pub fn clear_user_cancelled_on_live() {
    let mut list = live_inners().lock();
    list.retain(|w| w.strong_count() > 0);
    for w in list.iter() {
        if let Some(inner) = w.upgrade() {
            inner.user_cancelled.store(false, Ordering::SeqCst);
        }
    }
}

/// 用户点停止：立刻失败所有挂起的 Page CDP 调用（不杀浏览器）。
pub fn abort_all_pending(reason: &str) {
    abort_pending_where(|_| true, reason);
}

/// 仅打断指定调试端口上的挂起 CDP（单任务停止）。
pub fn abort_pending_on_port(port: u16, reason: &str) {
    abort_pending_where(|p| p == port, reason);
}

fn abort_pending_where(pred: impl Fn(u16) -> bool, reason: &str) {
    let reason = reason.to_string();
    let (arcs, ports): (Vec<Arc<CdpInner>>, Vec<u16>) = {
        let mut list = live_inners().lock();
        list.retain(|w| w.strong_count() > 0);
        let mut arcs = Vec::new();
        let mut ports = Vec::new();
        for w in list.iter() {
            let Some(inner) = w.upgrade() else { continue };
            if !pred(inner.port) {
                continue;
            }
            if !ports.contains(&inner.port) {
                ports.push(inner.port);
            }
            arcs.push(inner);
        }
        (arcs, ports)
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let reason2 = reason.clone();
            handle.spawn(async move {
                for inner in arcs {
                    inner.fail_all_pending(&reason2).await;
                }
            });
        }
        Err(_) => {
            for inner in arcs {
                inner.fail_all_pending_blocking(&reason);
            }
        }
    }
    for port in ports {
        crate::browser_network::abort_pending_on_port(port, &reason);
    }
}

fn capture_url_if_download(inner: &CdpInner, url: &str) {
    if !crate::client_download::looks_like_download_url(url) {
        return;
    }
    if let Ok(mut urls) = inner.captured_urls.try_lock() {
        if !urls.iter().any(|u| u == url) {
            urls.push(url.to_string());
        }
    }
}

fn handle_cdp_event(inner: &CdpInner, v: &Value) {
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else {
        return;
    };
    let params = v.get("params").unwrap_or(&Value::Null);
    match method {
        "Runtime.executionContextCreated" => {
            let ctx = params.get("context").unwrap_or(&Value::Null);
            let aux = ctx.get("auxData").unwrap_or(&Value::Null);
            let is_default = aux.get("isDefault").and_then(|x| x.as_bool()).unwrap_or(false);
            if !is_default {
                return;
            }
            let Some(frame_id) = aux.get("frameId").and_then(|x| x.as_str()) else {
                return;
            };
            let Some(ctx_id) = ctx.get("id").and_then(|x| x.as_u64()) else {
                return;
            };
            // 必须阻塞写入：旧版 try_lock 在 ordered_frame_context_ids 持锁时会丢事件，
            // 同源 iframe 上下文从此失踪 → 探针/点击只能扫到主文档。
            inner.frame_contexts.lock().insert(frame_id.to_string(), ctx_id);
        }
        "Runtime.executionContextDestroyed" => {
            if let Some(destroyed) = params.get("executionContextId").and_then(|x| x.as_u64()) {
                inner.frame_contexts.lock().retain(|_, id| *id != destroyed);
            }
        }
        "Network.requestWillBeSent" => {
            let req = params.get("request");
            let method = req
                .and_then(|r| r.get("method"))
                .and_then(|m| m.as_str())
                .unwrap_or("GET");
            if !(method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD")) {
                return;
            }
            if let Some(url) = req.and_then(|r| r.get("url")).and_then(|u| u.as_str()) {
                capture_url_if_download(inner, url);
            }
        }
        "Network.responseReceived" => {
            if let Some(url) = params
                .get("response")
                .and_then(|r| r.get("url"))
                .and_then(|u| u.as_str())
            {
                capture_url_if_download(inner, url);
            }
        }
        "Page.fileChooserOpened" => {
            let info = FileChooserOpened {
                backend_node_id: params
                    .get("backendNodeId")
                    .and_then(|x| x.as_u64())
                    .or_else(|| {
                        params
                            .get("backendNodeId")
                            .and_then(|x| x.as_i64())
                            .map(|n| n as u64)
                    }),
                mode: params
                    .get("mode")
                    .and_then(|x| x.as_str())
                    .unwrap_or("selectSingle")
                    .to_string(),
            };
            if let Ok(mut slot) = inner.file_chooser_wait.try_lock() {
                if let Some(tx) = slot.take() {
                    let _ = tx.send(info);
                    return;
                }
            }
            if let Ok(mut last) = inner.last_file_chooser.try_lock() {
                *last = Some(info);
            }
        }
        _ => {}
    }
}

pub struct CdpClient {
    inner: Arc<CdpInner>,
    pub port: u16,
    pub target_id: String,
    pub target_title: String,
    pub target_url: String,
    /// 上一步点击脚本判断该元素可能 window.open / target=_blank
    pub click_may_open_tab: bool,
    /// 登录步骤已跳过填表（已处于登录态），下一步不弹验证码窗
    pub skip_next_captcha: bool,
}

impl CdpClient {
    /// 连接共用 Debug Chrome（9222）
    pub async fn connect() -> Result<Self, String> {
        Self::connect_on(crate::chrome::DEBUG_PORT).await
    }

    /// 连接指定调试端口的 Chrome（后台任务并行实例）
    pub async fn connect_on(port: u16) -> Result<Self, String> {
        crate::chrome::wait_for_debug_port_on(port, 5).await?;

        let mut last_err = String::new();
        for attempt in 1..=6 {
            match Self::try_connect_once_on(port).await {
                Ok(client) => return Ok(client),
                Err(e) => {
                    last_err = e.clone();
                    if attempt < 6 {
                        tokio::time::sleep(std::time::Duration::from_millis(400 * attempt)).await;
                    }
                }
            }
        }
        Err(last_err)
    }

    /// 附着到指定调试端口上的某个标签页
    pub async fn connect_to_target_on(port: u16, target_id: &str) -> Result<Self, String> {
        Self::connect_to_any_target_on(port, target_id).await
    }

    /// 附着 page / iframe 等任意可调试目标（OOPIF 跨域 iframe 用）
    pub async fn connect_to_any_target_on(port: u16, target_id: &str) -> Result<Self, String> {
        let targets = crate::chrome::list_targets_on(port).await?;
        let target = targets
            .into_iter()
            .find(|t| t.id == target_id && (t.kind == "page" || t.kind == "iframe"))
            .ok_or_else(|| format!("目标 {target_id} 不存在（端口 {port}）"))?;
        Self::attach_page(port, &target, AttachMode::Full).await
    }

    /// 探针 clear / 短评测：只开 Page+Runtime，不做 auto-guard / sleep / createIsolatedWorld。
    /// Full 附着会 backfill 隔离世界；探针频繁连 OOPIF 时易把原生内存顶上去。
    pub async fn connect_to_any_target_light_on(port: u16, target_id: &str) -> Result<Self, String> {
        let targets = crate::chrome::list_targets_on(port).await?;
        let target = targets
            .into_iter()
            .find(|t| t.id == target_id && (t.kind == "page" || t.kind == "iframe"))
            .ok_or_else(|| format!("目标 {target_id} 不存在（端口 {port}）"))?;
        Self::attach_page(port, &target, AttachMode::Light).await
    }

    async fn try_connect_once_on(port: u16) -> Result<Self, String> {
        let _ = crate::chrome::close_devtools_page_targets_on(port).await;
        let page = crate::chrome::ensure_page_target_on(port).await?;
        Self::attach_page(port, &page, AttachMode::Full).await
    }

    async fn attach_page(
        port: u16,
        page: &crate::chrome::TargetInfo,
        mode: AttachMode,
    ) -> Result<Self, String> {
        let ws_url = format!("ws://127.0.0.1:{port}/devtools/page/{}", page.id);

        let (ws, _) = connect_async(&ws_url)
            .await
            .map_err(|e| format!("WebSocket 连接失败 ({ws_url}): {e}"))?;

        let (sink, mut stream) = ws.split();
        let inner = Arc::new(CdpInner {
            port,
            sink: Mutex::new(sink),
            pending: Mutex::new(HashMap::new()),
            user_cancelled: AtomicBool::new(false),
            captured_urls: Mutex::new(Vec::new()),
            frame_contexts: SyncMutex::new(HashMap::new()),
            file_chooser_wait: Mutex::new(None),
            last_file_chooser: Mutex::new(None),
        });
        register_live_inner(&inner);

        let reader_inner = inner.clone();
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let Ok(Message::Text(text)) = msg else {
                    // 非文本或读失败：继续；真正断开由循环结束处理
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if v.get("id").is_none() {
                    handle_cdp_event(&reader_inner, &v);
                    continue;
                }
                let Some(id) = v.get("id").and_then(|x| x.as_u64()) else { continue };
                let mut pending = reader_inner.pending.lock().await;
                if let Some(tx) = pending.remove(&id) {
                    if let Some(err) = v.get("error") {
                        let _ = tx.send(Err(err.to_string()));
                    } else {
                        let _ = tx.send(Ok(v));
                    }
                }
            }
            // Chrome OOM/被杀后 WS 关闭：立刻失败所有挂起调用，避免等满 30s 超时
            let mut pending = reader_inner.pending.lock().await;
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err("CDP 通道关闭（浏览器已退出或无响应）".into()));
            }
        });

        let client = Self {
            inner,
            port,
            target_id: page.id.clone(),
            target_title: page.title.clone(),
            target_url: page.url.clone(),
            click_may_open_tab: false,
            skip_next_captcha: false,
        };
        client.call("Page.enable", json!({})).await?;
        // 勿默认 DOM.enable：会镜像整棵 DOM，复杂页加载/刷新显著变慢；按需 enable
        client.call("Runtime.enable", json!({})).await?;
        if matches!(mode, AttachMode::Full) {
            // 禁止每次附着都 addScript：会永久累加，复杂页刷新从数秒涨到十余秒
            let _ = client
                .ensure_new_document_script("auto-guard", AUTOMATION_GUARD_SCRIPT)
                .await;
            let _ = client.evaluate(AUTOMATION_GUARD_SCRIPT).await;
            let _ = client.unblock_debugger().await;
            // 附着后立即补齐 FrameTree 子 frame 上下文，避免首步点击只看到主文档
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            client.backfill_missing_frame_contexts().await;
        }
        // 勿默认 Network.enable / 拉起 Browser 捕获：探针点一次就会让整端口所有页常驻 Network+Debugger，刷新多秒
        Ok(client)
    }

    /// 每个 target×kind 只注册一次；重复调用不会再累加脚本。
    pub async fn ensure_new_document_script(
        &self,
        kind: &str,
        source: &str,
    ) -> Result<(), String> {
        let key = format!("{}:{}:{kind}", self.port, self.target_id);
        if new_doc_script_ids().lock().contains_key(&key) {
            return Ok(());
        }
        let v = self
            .call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": source }),
            )
            .await?;
        let id = v
            .pointer("/result/identifier")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let remove_dup = {
            let mut map = new_doc_script_ids().lock();
            if map.contains_key(&key) {
                true
            } else {
                map.insert(key, id.clone());
                false
            }
        };
        if remove_dup && !id.is_empty() {
            let _ = self
                .call(
                    "Page.removeScriptToEvaluateOnNewDocument",
                    json!({ "identifier": id }),
                )
                .await;
        }
        Ok(())
    }

    /// 任务结束时卸掉「新文档自动开捕获」引导，避免空闲导航仍拦截下载/打印。
    pub async fn clear_new_document_script(&self, kind: &str) -> Result<(), String> {
        let key = format!("{}:{}:{kind}", self.port, self.target_id);
        let id = new_doc_script_ids().lock().remove(&key);
        let Some(id) = id else {
            return Ok(());
        };
        if id.is_empty() {
            return Ok(());
        }
        let _ = self
            .call(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({ "identifier": id }),
            )
            .await;
        Ok(())
    }

    /// 仅文件选择 / getFrameOwner 等 DOM 域 API 需要
    pub async fn ensure_dom_enabled(&self) -> Result<(), String> {
        self.call("DOM.enable", json!({})).await?;
        Ok(())
    }

    /// 探针/步骤结束后尽量关掉 DOM/Network 镜像，减轻后续刷新负担
    pub async fn release_heavy_domains(&self) {
        let _ = self.call("DOM.disable", json!({})).await;
        let _ = self.call("Network.disable", json!({})).await;
    }

    /// 页级 Network 监听（仅下载 URL 兜底需要时）
    pub async fn ensure_network_enabled(&self) -> Result<(), String> {
        self.call("Network.enable", json!({})).await?;
        Ok(())
    }

    /// 将当前附着的标签页窗口置于前台（用户显式「显示浏览器」时使用）
    pub async fn bring_to_front(&self) -> Result<(), String> {
        self.call("Page.bringToFront", json!({})).await?;
        Ok(())
    }

    /// Browser 级激活 + Page 置前，确保窗口在系统前台（会抢走其它应用焦点）
    pub async fn activate_tab(&self) -> Result<(), String> {
        crate::browser_network::activate_target_on(self.port, &self.target_id).await?;
        self.bring_to_front().await
    }

    /// 自动化步骤前准备标签：Windows 仍置前；macOS 不抢系统焦点，以免打断 webclaw。
    /// CDP 已附着目标页，后台亦可下发输入事件。
    pub async fn prepare_tab_for_automation(&self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            let _ = self;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.activate_tab().await
        }
    }

    /// 跟随浏览器当前活动页签（用户手动切换后，下一步在该标签执行）。
    /// 返回 `Ok(true)` 表示已切换到其它标签。
    ///
    /// 规则与页面探针一致：优先 `document.hasFocus()`，其次可见性。
    /// 短命下载弹窗关闭后，Chrome 常把前台切回其它标签；若当前自动化页仍存活，
    /// 且正处于下载捕获窗口，则不跟走，避免离开业务页。
    pub async fn sync_to_active_tab(&mut self) -> Result<bool, String> {
        let mut current_visible = false;
        // 快路径：当前附着页已聚焦，无需扫描其它标签
        if let Ok(v) = self
            .evaluate(
                r#"(() => {
  try {
    return {
      focused: !!document.hasFocus(),
      visible: document.visibilityState === 'visible' && !document.hidden
    };
  } catch (_) {
    return { focused: false, visible: false };
  }
})()"#,
            )
            .await
        {
            if v.get("focused").and_then(|x| x.as_bool()) == Some(true) {
                return Ok(false);
            }
            current_visible = v.get("visible").and_then(|x| x.as_bool()) == Some(true);
        }

        // 当前仍可见但未聚焦：可能是短暂弹窗抢焦，或其它窗口标签已聚焦。
        // 优先跟 hasFocus 页签；若探测不到，再用探针同源打分（find_foreground），
        // 避免用户已切到新标签却仍卡在旧可见页上（临时步骤点坐标常见）。
        if current_visible {
            if let Some(page) = crate::chrome::find_focused_page_on(self.port).await {
                if page.id != self.target_id {
                    return self.attach_to_page_target(&page).await;
                }
                return Ok(false);
            }
            return self.attach_foreground_tab(false).await;
        }

        self.attach_foreground_tab(false).await
    }

    /// 与页面探针一致：按前台聚焦/可见性重新附着标签。
    /// `allow_mac_unocclude`：窗口遮挡导致 visibility 全灭时，是否短暂置前再探测。
    pub async fn attach_foreground_tab(&mut self, allow_mac_unocclude: bool) -> Result<bool, String> {
        let page =
            match crate::chrome::find_foreground_page_on_ex(self.port, allow_mac_unocclude).await {
                Ok(p) => p,
                Err(_) => return Ok(false),
            };
        if page.id == self.target_id {
            return Ok(false);
        }
        self.attach_to_page_target(&page).await
    }

    async fn attach_to_page_target(
        &mut self,
        page: &crate::chrome::TargetInfo,
    ) -> Result<bool, String> {
        // 当前页仍存活时：勿跟到空白/下载弹窗；下载捕获窗口内也不跟走
        if crate::chrome::page_target_exists_on(self.port, &self.target_id)
            .await
            .unwrap_or(false)
            && should_keep_automation_tab(self.port, &page.url)
        {
            return Ok(false);
        }

        let may_open = self.click_may_open_tab;
        let skip_captcha = self.skip_next_captcha;
        *self = Self::connect_to_target_on(self.port, &page.id).await?;
        self.click_may_open_tab = may_open;
        self.skip_next_captcha = skip_captcha;
        let _ = self.sync_page_meta().await;
        // macOS：勿 Page.bringToFront，避免把 Chrome 拉到系统前台
        #[cfg(not(target_os = "macos"))]
        {
            let _ = self.bring_to_front().await;
        }
        Ok(true)
    }

    /// 重新附着到指定标签并激活
    pub async fn switch_to_target(&mut self, target_id: &str) -> Result<(), String> {
        let may_open = self.click_may_open_tab;
        let skip_captcha = self.skip_next_captcha;
        *self = Self::connect_to_target_on(self.port, target_id).await?;
        self.click_may_open_tab = may_open;
        self.skip_next_captcha = skip_captcha;
        self.prepare_tab_for_automation().await
    }

    /// 若父标签通过 window.open 打开了子 page，切换 CDP 到该子标签
    pub async fn try_follow_opener_child_tab(
        &mut self,
        parent_target_id: &str,
        known_page_ids: Option<&std::collections::HashSet<String>>,
        may_open_tab: bool,
    ) -> Result<bool, String> {
        self.try_follow_opener_child_tab_cancel(
            parent_target_id,
            known_page_ids,
            may_open_tab,
            None,
        )
        .await
    }

    pub async fn try_follow_opener_child_tab_cancel(
        &mut self,
        parent_target_id: &str,
        known_page_ids: Option<&std::collections::HashSet<String>>,
        may_open_tab: bool,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<bool, String> {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};
        // 普通点击极少会 window.open；有 pages_before 时也不应空等数秒。
        // 仅脚本标明 opensNewTab / may_open_tab 时才拉长轮询。
        let min_poll = if may_open_tab {
            Duration::from_secs(8)
        } else if known_page_ids.is_some() {
            Duration::from_millis(350)
        } else {
            Duration::from_millis(200)
        };
        let max_poll = if may_open_tab {
            Duration::from_secs(12)
        } else if known_page_ids.is_some() {
            Duration::from_millis(1200)
        } else {
            Duration::from_millis(600)
        };
        let started = Instant::now();

        while started.elapsed() < max_poll {
            if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
                return Ok(false);
            }
            // 打印预览常在点「打印」后立刻弹出并抢焦点，导致跟随子标签像卡住；轮询里持续关掉。
            let _ = crate::print_pdf::dismiss_native_print_ui(self.port).await;

            if let Some(child_id) =
                resolve_opener_child_id(self.port, parent_target_id, known_page_ids).await?
            {
                if child_id != self.target_id {
                    // 尽早轻量附着：完整 ensure（OOPIF/下载钩子）曾耗数秒，丢掉 window.print。
                    let _ = crate::print_pdf::dismiss_native_print_ui(self.port).await;
                    match CdpClient::connect_to_any_target_light_on(self.port, &child_id).await {
                        Ok(early) => {
                            if crate::client_download::ensure_print_capture_fast(&early)
                                .await
                                .is_ok()
                            {
                                let _ = crate::print_pdf::dismiss_native_print_ui_with_escape(
                                    &early,
                                )
                                .await;
                            }
                        }
                        Err(_) => {}
                    }

                    let blank_deadline = Instant::now() + Duration::from_secs(10);
                    while Instant::now() < blank_deadline {
                        if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
                            return Err("已取消".into());
                        }
                        if page_target_url_ready(self.port, &child_id).await? {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }

                    // 短命下载页签：只记下 URL，不把 CDP 切过去（页签常立刻关闭）
                    if let Ok(pages) = crate::chrome::list_page_targets_on(self.port).await {
                        if let Some(p) = pages.iter().find(|p| p.id == child_id) {
                            let u = p.url.trim();
                            if crate::client_download::looks_like_download_url(u) {
                                crate::browser_network::note_download_url_on(self.port, u);
                                crate::browser_network::remove_tracked_child_on(
                                    self.port,
                                    parent_target_id,
                                    &child_id,
                                );
                                return Ok(false);
                            }
                        }
                    }

                    self.switch_to_target(&child_id).await?;
                    let _ = crate::print_pdf::dismiss_native_print_ui_with_escape(self).await;
                    // switch 后仍用轻量路径抢 print；完整下载钩子留给「等待下载」
                    let _ = crate::client_download::ensure_print_capture_fast(self).await;
                    self.wait_after_navigation_cancel(15, cancel).await?;
                    let _ = self.sync_page_meta().await;
                    if self.target_url.contains("isprint=1") {
                        let _ = crate::client_download::ensure_print_capture_fast(self).await;
                        let _ = crate::print_pdf::dismiss_native_print_ui_with_escape(self).await;
                    }
                    crate::browser_network::remove_tracked_child_on(
                        self.port,
                        parent_target_id,
                        &child_id,
                    );
                    return Ok(true);
                }
                crate::browser_network::remove_tracked_child_on(
                    self.port,
                    parent_target_id,
                    &child_id,
                );
            }

            if started.elapsed() >= min_poll {
                let has_signal =
                    has_popup_follow_signal(self.port, parent_target_id, known_page_ids).await?;
                let has_new = if let Some(known) = known_page_ids {
                    has_new_page_ids(self.port, known).await?
                } else {
                    false
                };
                if !has_signal && !has_new {
                    return Ok(false);
                }
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Ok(false)
    }

    /// 是否存在待跟随的 window.open 子标签（用于避免同页点击空等）
    pub async fn popup_follow_pending(
        &self,
        parent_target_id: &str,
        known_page_ids: Option<&std::collections::HashSet<String>>,
    ) -> Result<bool, String> {
        if has_popup_follow_signal(self.port, parent_target_id, known_page_ids).await? {
            return Ok(true);
        }
        if let Some(known) = known_page_ids {
            if has_new_page_ids(self.port, known).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 从页面读取最新标题与 URL，写回客户端元数据
    pub async fn sync_page_meta(&mut self) -> Result<(), String> {
        let title = self
            .evaluate("document.title")
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        let url = self
            .evaluate("location.href")
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        if !title.is_empty() {
            self.target_title = title;
        }
        if !url.is_empty() {
            self.target_url = url;
        }
        Ok(())
    }

    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call_with_timeout(method, params, std::time::Duration::from_secs(30))
            .await
    }

    pub(crate) async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, String> {
        if self.inner.user_cancelled.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        let id = MSG_ID.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);

        let req = json!({ "id": id, "method": method, "params": params });
        if let Err(e) = self
            .inner
            .sink
            .lock()
            .await
            .send(Message::Text(req.to_string()))
            .await
        {
            self.inner.pending.lock().await.remove(&id);
            return Err(e.to_string());
        }

        if self.inner.user_cancelled.load(Ordering::SeqCst) {
            self.inner.pending.lock().await.remove(&id);
            return Err("已取消".into());
        }

        let resp = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| format!("CDP 超时: {method}"))?
            .map_err(|_| format!("CDP 通道关闭: {method}"))??;
        Ok(resp)
    }

    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        let x = x.round();
        let y = y.round();
        self.call(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": x,
                "y": y,
                "button": "left",
                "buttons": 0,
            }),
        )
        .await?;
        self.call(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mousePressed",
                "x": x,
                "y": y,
                "button": "left",
                "buttons": 1,
                "clickCount": 1
            }),
        )
        .await?;
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        self.call(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseReleased",
                "x": x,
                "y": y,
                "button": "left",
                "buttons": 0,
                "clickCount": 1
            }),
        )
        .await?;
        Ok(())
    }

    /// 拦截系统文件选择框：启用后点击文件入口不会弹出 Windows 对话框，
    /// 改为推送 `Page.fileChooserOpened`，由程序注入路径。
    pub async fn set_intercept_file_chooser(&self, enabled: bool) -> Result<(), String> {
        self.call(
            "Page.setInterceptFileChooserDialog",
            json!({ "enabled": enabled }),
        )
        .await?;
        Ok(())
    }

    pub async fn arm_file_chooser_wait(&self) -> oneshot::Receiver<FileChooserOpened> {
        let (tx, rx) = oneshot::channel();
        *self.inner.file_chooser_wait.lock().await = Some(tx);
        rx
    }

    pub async fn clear_file_chooser_wait(&self) {
        *self.inner.file_chooser_wait.lock().await = None;
    }

    pub async fn take_last_file_chooser(&self) -> Option<FileChooserOpened> {
        self.inner.last_file_chooser.lock().await.take()
    }

    /// 用户已按直觉点开上传入口（如「上传」→「上传文件」）后：把本地文件写入被拦截的 chooser / input。
    /// 不开关系统框拦截（由菜谱执行器全程控制，类似下载捕获）。
    pub async fn upload_local_files_after_open(
        &self,
        paths: &[std::path::PathBuf],
    ) -> Result<String, String> {
        if paths.is_empty() {
            return Err("未指定要上传的文件".into());
        }
        for p in paths {
            if !p.is_file() {
                return Err(format!("文件不存在: {}", p.display()));
            }
        }
        let names: Vec<String> = paths
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();

        if let Some(opened) = self.take_last_file_chooser().await {
            if let Some(backend) = opened.backend_node_id {
                self.set_file_input_files(backend, paths).await?;
                return Ok(format!(
                    "已写入（文件框/{}）: {}",
                    opened.mode,
                    names.join("、")
                ));
            }
        }

        if self
            .resolve_best_file_input_object_id(
                paths
                    .first()
                    .and_then(|p| p.extension())
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .as_deref(),
            )
            .await?
            .is_some()
            || self.find_file_input_backend_node().await?.is_some()
        {
            return self.upload_local_files_direct(paths).await;
        }

        let mut rx = self.arm_file_chooser_wait().await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(opened) = self.take_last_file_chooser().await {
                self.clear_file_chooser_wait().await;
                if let Some(backend) = opened.backend_node_id {
                    self.set_file_input_files(backend, paths).await?;
                    return Ok(format!(
                        "已写入（文件框/{}）: {}",
                        opened.mode,
                        names.join("、")
                    ));
                }
                self.wait_and_inject_file_input(paths, 3).await?;
                return Ok(format!("已写入（文件框→input）: {}", names.join("、")));
            }
            match tokio::time::timeout(std::time::Duration::from_millis(200), &mut rx).await {
                Ok(Ok(opened)) => {
                    if let Some(backend) = opened.backend_node_id {
                        self.set_file_input_files(backend, paths).await?;
                        return Ok(format!(
                            "已写入（文件框/{}）: {}",
                            opened.mode,
                            names.join("、")
                        ));
                    }
                    self.wait_and_inject_file_input(paths, 3).await?;
                    return Ok(format!("已写入（文件框→input）: {}", names.join("、")));
                }
                Ok(Err(_)) => {
                    return Err("文件选择等待被取消".into());
                }
                Err(_) => {}
            }
            if self
                .resolve_best_file_input_object_id(None)
                .await?
                .is_some()
                || self.find_file_input_backend_node().await?.is_some()
            {
                self.clear_file_chooser_wait().await;
                return self.upload_local_files_direct(paths).await;
            }
            if std::time::Instant::now() >= deadline {
                self.clear_file_chooser_wait().await;
                return Err(
                    "未找到可写入的文件选择器。请先：点击上传，点击上传文件，再执行「上传」"
                        .into(),
                );
            }
        }
    }

    async fn wait_and_inject_file_input(
        &self,
        paths: &[std::path::PathBuf],
        timeout_secs: u64,
    ) -> Result<(), String> {
        let prefer_ext = paths
            .first()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            if let Some((object_id, _)) = self
                .resolve_best_file_input_object_id(prefer_ext.as_deref())
                .await?
            {
                return self.set_file_input_files_on_object(&object_id, paths).await;
            }
            if let Some(backend) = self.find_file_input_backend_node().await? {
                return self.set_file_input_files(backend, paths).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        Err("点击后仍未出现 input[type=file]，无法写入文件".into())
    }

    /// 向 `<input type=file>` 注入本地绝对路径（绕过系统文件框）。
    pub async fn set_file_input_files(
        &self,
        backend_node_id: u64,
        paths: &[std::path::PathBuf],
    ) -> Result<(), String> {
        if paths.is_empty() {
            return Err("未指定要上传的文件".into());
        }
        for p in paths {
            if !p.is_file() {
                return Err(format!("文件不存在: {}", p.display()));
            }
        }
        let files: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.ensure_dom_enabled().await?;
        self.call(
            "DOM.setFileInputFiles",
            json!({
                "files": files,
                "backendNodeId": backend_node_id,
            }),
        )
        .await?;
        Ok(())
    }

    /// 经 objectId 注入文件（只写入 input，不额外派发事件或点击）。
    pub async fn set_file_input_files_on_object(
        &self,
        object_id: &str,
        paths: &[std::path::PathBuf],
    ) -> Result<(), String> {
        if paths.is_empty() {
            return Err("未指定要上传的文件".into());
        }
        for p in paths {
            if !p.is_file() {
                return Err(format!("文件不存在: {}", p.display()));
            }
        }
        let files: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.ensure_dom_enabled().await?;
        self.call(
            "DOM.setFileInputFiles",
            json!({
                "files": files,
                "objectId": object_id,
            }),
        )
        .await?;
        Ok(())
    }

    /// 挑选最合适的 `input[type=file]`（含隐藏），返回 Runtime objectId。
    /// `prefer_ext` 如 `xlsx`，会优先 accept 匹配的控件。
    pub async fn resolve_best_file_input_object_id(
        &self,
        prefer_ext: Option<&str>,
    ) -> Result<Option<(String, String)>, String> {
        let prefer = prefer_ext.unwrap_or("").to_ascii_lowercase();
        let prefer_json = serde_json::to_string(&prefer).unwrap_or_else(|_| "\"\"".into());
        let expr = format!(
            r#"(() => {{
  const prefer = {prefer_json};
  const inputs = Array.from(document.querySelectorAll('input[type=file]'));
  if (!inputs.length) return null;
  const score = (el, i) => {{
    let s = i * 0.01;
    if (!el.disabled) s += 10;
    if (!el.readOnly) s += 2;
    const acc = String(el.accept || '').toLowerCase();
    if (!acc) s += 6;
    if (prefer) {{
      if (acc.includes(prefer) || acc.includes('.' + prefer)) s += 30;
      if (prefer === 'xlsx' || prefer === 'xls' || prefer === 'xlsm' || prefer === 'csv') {{
        if (acc.includes('sheet') || acc.includes('excel') || acc.includes('.xls') || acc.includes('spreadsheet')) s += 20;
      }}
      if (acc.includes('image') && ['png','jpg','jpeg','gif','webp'].includes(prefer)) s += 20;
    }}
    const st = window.getComputedStyle(el);
    if (st && (st.display === 'none' || st.visibility === 'hidden' || el.hidden)) s += 1;
    return s;
  }};
  let best = 0, bestScore = -1e9;
  inputs.forEach((el, i) => {{
    const sc = score(el, i);
    if (sc >= bestScore) {{ bestScore = sc; best = i; }}
  }});
  const el = inputs[best];
  el.setAttribute('data-webclaw-file-target', '1');
  return {{
    index: best,
    count: inputs.length,
    accept: el.accept || '',
    name: el.name || '',
    multiple: !!el.multiple,
  }};
}})()"#
        );
        let info = self.evaluate(&expr).await?;
        if info.is_null() {
            return Ok(None);
        }
        let count = info.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        if count == 0 {
            return Ok(None);
        }
        let eval = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "document.querySelector('input[type=file][data-webclaw-file-target=\"1\"]')",
                    "returnByValue": false,
                }),
            )
            .await?;
        let object_id = eval["result"]["result"]["objectId"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "未能取得文件输入框 objectId".to_string())?;
        // 清掉标记，避免污染 DOM
        let _ = self
            .evaluate(
                r#"(() => {
  document.querySelectorAll('[data-webclaw-file-target]').forEach(el => el.removeAttribute('data-webclaw-file-target'));
})()"#,
            )
            .await;
        let summary = format!(
            "input#{}/{} accept={}",
            info.get("index").and_then(|v| v.as_u64()).unwrap_or(0) + 1,
            count,
            info.get("accept").and_then(|v| v.as_str()).unwrap_or("")
        );
        Ok(Some((object_id, summary)))
    }

    /// 查询页面上第一个 `input[type=file]` 的 backendNodeId（含隐藏 input）。
    pub async fn find_file_input_backend_node(&self) -> Result<Option<u64>, String> {
        self.ensure_dom_enabled().await?;
        let doc = self.call("DOM.getDocument", json!({ "depth": 0 })).await?;
        let root_id = doc["result"]["root"]["nodeId"]
            .as_u64()
            .ok_or("DOM.getDocument 未返回 nodeId")?;
        let q = self
            .call(
                "DOM.querySelector",
                json!({
                    "nodeId": root_id,
                    "selector": "input[type=file]",
                }),
            )
            .await?;
        let node_id = match q["result"]["nodeId"].as_u64() {
            Some(id) if id > 0 => id,
            _ => return Ok(None),
        };
        let desc = self
            .call("DOM.describeNode", json!({ "nodeId": node_id }))
            .await?;
        let backend = desc["result"]["node"]["backendNodeId"]
            .as_u64()
            .or_else(|| {
                desc["result"]["node"]["backendNodeId"]
                    .as_i64()
                    .map(|n| n as u64)
            });
        Ok(backend)
    }

    /// 不点击：只把本地文件写入页面 `input[type=file]`（含隐藏）。后续页面反应交给其它菜谱步骤。
    pub async fn upload_local_files_direct(
        &self,
        paths: &[std::path::PathBuf],
    ) -> Result<String, String> {
        if paths.is_empty() {
            return Err("未指定要上传的文件".into());
        }
        for p in paths {
            if !p.is_file() {
                return Err(format!("文件不存在: {}", p.display()));
            }
        }
        let prefer_ext = paths
            .first()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());

        if let Some((object_id, summary)) = self
            .resolve_best_file_input_object_id(prefer_ext.as_deref())
            .await?
        {
            self.set_file_input_files_on_object(&object_id, paths)
                .await?;
            let names: Vec<String> = paths
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            return Ok(format!(
                "已写入 {}（{}）: {}",
                summary,
                paths.len(),
                names.join("、")
            ));
        }

        let backend = self.find_file_input_backend_node().await?.ok_or_else(|| {
            "页面上未找到 input[type=file]".to_string()
        })?;
        self.set_file_input_files(backend, paths).await?;
        let names: Vec<String> = paths
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        Ok(format!(
            "已写入 input（node:{backend}）: {}",
            names.join("、")
        ))
    }

    pub async fn navigate(&mut self, url: &str) -> Result<(), String> {
        let resp = self.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = resp["result"]["errorText"].as_str() {
            if !err.is_empty() {
                return Err(format!("导航失败: {err}"));
            }
        }
        // 等待加载
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let ready = self
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            if ready == "complete" || ready == "interactive" {
                break;
            }
        }
        let _ = self.sync_page_meta().await;
        Ok(())
    }

    /// 刷新当前标签页（F5），并等待 DOM 就绪
    pub async fn reload(&mut self, ignore_cache: bool) -> Result<(), String> {
        self.call(
            "Page.reload",
            json!({ "ignoreCache": ignore_cache }),
        )
        .await?;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let ready = self
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            if ready == "complete" || ready == "interactive" {
                break;
            }
        }
        let _ = self.sync_page_meta().await;
        Ok(())
    }

    /// 解除调试器暂停（避免「Debugger paused in another tab」导致页面假死）
    pub async fn unblock_debugger(&self) -> Result<(), String> {
        for (method, params) in automation_debugger_policy_steps() {
            let _ = self.call(method, params).await;
        }
        Ok(())
    }

    /// 向当前焦点元素插入文本（调用前应由页面脚本完成聚焦与全选）
    pub async fn insert_text(&self, text: &str) -> Result<(), String> {
        self.call("Input.insertText", json!({ "text": text })).await?;
        let text_json = serde_json::to_string(text).map_err(|e| e.to_string())?;
        let _ = self
            .evaluate(&format!(
                r#"(() => {{
  const el = document.activeElement;
  if (!el) return;
  const data = {text_json};
  try {{
    if (typeof el.setCustomValidity === 'function') el.setCustomValidity('');
  }} catch (_) {{}}
  try {{
    el.dispatchEvent(new InputEvent('input', {{ bubbles: true, data, inputType: 'insertText' }}));
  }} catch (_) {{
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  }}
  el.dispatchEvent(new Event('change', {{ bubbles: true }}));
}})()"#
            ))
            .await;
        Ok(())
    }

    /// CDP 级按键（比 JS KeyboardEvent 更接近真实键入，isTrusted=true）
    pub async fn dispatch_key_event(
        &self,
        key: &str,
        code: &str,
        key_code: u32,
        text: Option<&str>,
    ) -> Result<(), String> {
        for event_type in ["rawKeyDown", "keyDown", "keyUp"] {
            let mut params = json!({
                "type": event_type,
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": key_code,
                "nativeVirtualKeyCode": key_code,
            });
            if event_type == "keyDown" {
                if let Some(t) = text {
                    if let Some(obj) = params.as_object_mut() {
                        obj.insert("text".into(), json!(t));
                    }
                }
            }
            self.call("Input.dispatchKeyEvent", params).await?;
        }
        Ok(())
    }

    /// 截取当前视口 PNG
    pub async fn capture_screenshot_png(&self) -> Result<Vec<u8>, String> {
        self.capture_screenshot_viewport(None).await
    }

    /// 按 CSS 像素区域截图（x/y/w/h；w 或 h 为 0 时截全页）
    pub async fn capture_screenshot_clip(
        &self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        dpr: f64,
    ) -> Result<Vec<u8>, String> {
        let dpr = if dpr > 0.0 { dpr } else { 1.0 };
        if w <= 0.0 || h <= 0.0 {
            return self.capture_screenshot_viewport(None).await;
        }
        self.capture_screenshot_viewport(Some(json!({
            "x": x * dpr,
            "y": y * dpr,
            "width": w * dpr,
            "height": h * dpr,
            "scale": 1
        })))
        .await
    }

    async fn capture_screenshot_viewport(
        &self,
        clip: Option<Value>,
    ) -> Result<Vec<u8>, String> {
        let mut params = json!({ "format": "png", "fromSurface": true });
        if let Some(c) = clip {
            params["clip"] = c;
        }
        let v = self.call("Page.captureScreenshot", params).await?;
        let b64 = v["result"]["data"]
            .as_str()
            .ok_or_else(|| "截图数据为空".to_string())?;
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("截图解码失败: {e}"))
    }

    pub async fn call_result(&self, method: &str, params: Value) -> Result<Value, String> {
        let v = self.call(method, params).await?;
        if let Some(err) = v.get("error") {
            return Err(err.to_string());
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn evaluate(&self, expression: &str) -> Result<Value, String> {
        self.evaluate_in_context(expression, None).await
    }

    pub async fn evaluate_download(&self, expression: &str) -> Result<Value, String> {
        self.evaluate_in_context_with_timeout(
            expression,
            None,
            std::time::Duration::from_secs(120),
        )
        .await
    }

    pub async fn evaluate_in_context(
        &self,
        expression: &str,
        context_id: Option<u64>,
    ) -> Result<Value, String> {
        self.evaluate_in_context_with_timeout(
            expression,
            context_id,
            std::time::Duration::from_secs(30),
        )
        .await
    }

    pub async fn evaluate_in_context_with_timeout(
        &self,
        expression: &str,
        context_id: Option<u64>,
        timeout: std::time::Duration,
    ) -> Result<Value, String> {
        let mut params = json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true
        });
        if let Some(id) = context_id {
            params["contextId"] = json!(id);
        }
        let v = self.call_with_timeout("Runtime.evaluate", params, timeout).await?;
        if let Some(exc) = v["result"]["exceptionDetails"].as_object() {
            return Err(format!("JS 执行异常: {exc:?}"));
        }
        Ok(v["result"]["result"]["value"].clone())
    }

    pub async fn frame_context_ids(&self) -> Vec<u64> {
        let map = self.inner.frame_contexts.lock();
        let mut ids: Vec<u64> = map.values().copied().collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// 按 Page.getFrameTree 深度优先收集 frameId（主 frame 在前，子 iframe 按 DOM 树顺序）。
    fn collect_frame_tree_ids(node: &Value, out: &mut Vec<String>) {
        if let Some(id) = node
            .get("frame")
            .and_then(|f| f.get("id"))
            .and_then(|x| x.as_str())
        {
            out.push(id.to_string());
        }
        if let Some(children) = node.get("childFrames").and_then(|c| c.as_array()) {
            for child in children {
                Self::collect_frame_tree_ids(child, out);
            }
        }
    }

    /// FrameTree DFS 顺序的 frame URL（用于 OOPIF 与 DOM 顺序对齐）。
    pub async fn frame_tree_urls_dfs(&self) -> Vec<String> {
        let Ok(v) = self.call("Page.getFrameTree", json!({})).await else {
            return Vec::new();
        };
        let Some(root) = v.get("result").and_then(|r| r.get("frameTree")) else {
            return Vec::new();
        };
        let mut urls = Vec::new();
        fn walk(node: &Value, urls: &mut Vec<String>) {
            if let Some(url) = node
                .get("frame")
                .and_then(|f| f.get("url"))
                .and_then(|x| x.as_str())
            {
                if !url.is_empty() {
                    urls.push(url.to_string());
                }
            }
            if let Some(children) = node.get("childFrames").and_then(|c| c.as_array()) {
                for child in children {
                    walk(child, urls);
                }
            }
        }
        walk(root, &mut urls);
        urls
    }

    /// 主文档上下文在前，同源子 iframe 按 FrameTree DOM 顺序在后（稳定、可复现）。
    pub async fn ordered_frame_context_ids(&self) -> Vec<u64> {
        let tree = self.call("Page.getFrameTree", json!({})).await.ok();
        let map = self.inner.frame_contexts.lock();
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();

        if let Some(v) = tree.as_ref() {
            if let Some(root) = v.get("result").and_then(|r| r.get("frameTree")) {
                let mut frame_ids = Vec::new();
                Self::collect_frame_tree_ids(root, &mut frame_ids);
                for fid in frame_ids {
                    if let Some(&ctx) = map.get(&fid) {
                        if seen.insert(ctx) {
                            ordered.push(ctx);
                        }
                    }
                }
            }
        }

        let mut rest: Vec<u64> = map
            .values()
            .copied()
            .filter(|id| !seen.contains(id))
            .collect();
        rest.sort_unstable();
        ordered.extend(rest);
        ordered
    }

    /// 重新启用 Runtime 并短暂等待 iframe 执行上下文就绪；返回主→子稳定顺序。
    /// FrameTree 已声明但事件表缺失的子 frame，用 createIsolatedWorld 兜底拿 context（同源必经）。
    pub async fn refresh_frame_contexts(&self) -> Vec<u64> {
        let _ = self.call("Runtime.enable", json!({})).await;
        let _ = self.call("Page.enable", json!({})).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.backfill_missing_frame_contexts().await;
        self.ordered_frame_context_ids().await
    }

    /// 对 FrameTree 中尚无默认上下文的 frame，创建隔离世界以便求值（不依赖易丢的 Created 事件）。
    async fn backfill_missing_frame_contexts(&self) {
        let Ok(v) = self.call("Page.getFrameTree", json!({})).await else {
            return;
        };
        let Some(root) = v.get("result").and_then(|r| r.get("frameTree")) else {
            return;
        };
        let mut frame_ids = Vec::new();
        Self::collect_frame_tree_ids(root, &mut frame_ids);
        let missing: Vec<String> = {
            let map = self.inner.frame_contexts.lock();
            frame_ids
                .into_iter()
                .filter(|fid| !map.contains_key(fid))
                .collect()
        };
        for fid in missing {
            let Ok(resp) = self
                .call(
                    "Page.createIsolatedWorld",
                    json!({
                        "frameId": fid,
                        "worldName": "__webclaw_frame__",
                        // CDP 协议字段拼写如此（Univeral）
                        "grantUniveralAccess": true,
                    }),
                )
                .await
            else {
                continue;
            };
            let Some(ctx_id) = resp
                .get("result")
                .and_then(|r| r.get("executionContextId"))
                .and_then(|x| x.as_u64())
            else {
                continue;
            };
            self.inner.frame_contexts.lock().insert(fid, ctx_id);
        }
    }

    async fn main_frame_id(&self) -> Option<String> {
        let v = self.call("Page.getFrameTree", json!({})).await.ok()?;
        v.get("result")?
            .get("frameTree")?
            .get("frame")?
            .get("id")?
            .as_str()
            .map(String::from)
    }

    /// 供点击日志判断是否点在同源子 frame。
    pub async fn main_frame_id_public(&self) -> Option<String> {
        self.main_frame_id().await
    }

    /// contextId → frameId（默认执行上下文）
    pub async fn frame_id_for_context(&self, context_id: u64) -> Option<String> {
        let map = self.inner.frame_contexts.lock();
        map.iter()
            .find(|(_, &id)| id == context_id)
            .map(|(fid, _)| fid.clone())
    }

    /// FrameTree：子 frameId → 父 frameId
    async fn frame_parent_map(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let Ok(v) = self.call("Page.getFrameTree", json!({})).await else {
            return out;
        };
        let Some(root) = v.get("result").and_then(|r| r.get("frameTree")) else {
            return out;
        };
        fn walk(node: &Value, parent: Option<&str>, out: &mut HashMap<String, String>) {
            let Some(id) = node
                .get("frame")
                .and_then(|f| f.get("id"))
                .and_then(|x| x.as_str())
            else {
                return;
            };
            if let Some(p) = parent {
                out.insert(id.to_string(), p.to_string());
            }
            if let Some(children) = node.get("childFrames").and_then(|c| c.as_array()) {
                for child in children {
                    walk(child, Some(id), out);
                }
            }
        }
        walk(root, None, &mut out);
        out
    }

    /// iframe 元素内容区左上角，相对「容纳该 iframe 的文档」视口。
    /// 优先 resolveNode + getBoundingClientRect（含 clientLeft/Top）；失败再回退 getBoxModel。
    async fn frame_owner_content_offset(&self, frame_id: &str) -> Result<(f64, f64), String> {
        self.ensure_dom_enabled().await?;
        let owner = self
            .call("DOM.getFrameOwner", json!({ "frameId": frame_id }))
            .await?;
        let backend = owner
            .get("result")
            .and_then(|r| r.get("backendNodeId"))
            .and_then(|x| x.as_u64())
            .ok_or_else(|| format!("getFrameOwner 无 backendNodeId（frame={frame_id}）"))?;

        // 1) JS getBoundingClientRect：border box + clientLeft/Top → 内容区原点（与 iframe 内 (0,0) 对齐）
        if let Ok(resolved) = self
            .call("DOM.resolveNode", json!({ "backendNodeId": backend }))
            .await
        {
            if let Some(object_id) = resolved
                .pointer("/result/object/objectId")
                .and_then(|x| x.as_str())
            {
                let call = self
                    .call(
                        "Runtime.callFunctionOn",
                        json!({
                            "objectId": object_id,
                            "functionDeclaration": r#"function() {
  try { this.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'instant' }); } catch (_) {}
  const r = this.getBoundingClientRect();
  return {
    x: r.left + (this.clientLeft || 0),
    y: r.top + (this.clientTop || 0),
    w: r.width,
    h: r.height
  };
}"#,
                            "returnByValue": true,
                        }),
                    )
                    .await;
                // 释放远程对象，避免句柄堆积
                let _ = self
                    .call("Runtime.releaseObject", json!({ "objectId": object_id }))
                    .await;
                if let Ok(v) = call {
                    if let Some(val) = v.pointer("/result/result/value") {
                        let x = val.get("x").and_then(|x| x.as_f64());
                        let y = val.get("y").and_then(|x| x.as_f64());
                        if let (Some(x), Some(y)) = (x, y) {
                            return Ok((x, y));
                        }
                    }
                }
            }
        }

        // 2) 回退 getBoxModel content quad
        let box_v = self
            .call("DOM.getBoxModel", json!({ "backendNodeId": backend }))
            .await?;
        let content = box_v
            .get("result")
            .and_then(|r| r.get("model"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .ok_or_else(|| "getBoxModel 无 content quad".to_string())?;
        let x = content
            .first()
            .and_then(|v| v.as_f64())
            .ok_or_else(|| "content quad 缺 x".to_string())?;
        let y = content
            .get(1)
            .and_then(|v| v.as_f64())
            .ok_or_else(|| "content quad 缺 y".to_string())?;
        Ok((x, y))
    }

    /// 将「某 frame 内视口坐标」累加到主文档视口：沿 FrameTree 向上加各层 iframe 元素偏移。
    /// 同站点跨源 iframe（无独立 OOPIF target）点击时必须用此变换。
    pub async fn frame_viewport_offset_to_root(&self, frame_id: &str) -> Result<(f64, f64), String> {
        let main = self
            .main_frame_id()
            .await
            .ok_or_else(|| "无法取得主 frameId".to_string())?;
        if frame_id == main {
            return Ok((0.0, 0.0));
        }
        let parents = self.frame_parent_map().await;
        let mut ox = 0.0;
        let mut oy = 0.0;
        let mut cur = frame_id.to_string();
        for _ in 0..16 {
            if cur == main {
                return Ok((ox, oy));
            }
            let (dx, dy) = self.frame_owner_content_offset(&cur).await?;
            ox += dx;
            oy += dy;
            cur = parents
                .get(&cur)
                .cloned()
                .ok_or_else(|| format!("frame {cur} 在 FrameTree 中无父节点"))?;
        }
        Err("iframe 嵌套过深".into())
    }

    /// 子 frame 执行上下文内的 (x,y) → 主文档视口坐标所需的偏移
    pub async fn frame_context_viewport_offset(
        &self,
        context_id: u64,
    ) -> Result<(f64, f64), String> {
        let frame_id = self
            .frame_id_for_context(context_id)
            .await
            .ok_or_else(|| format!("找不到 context={context_id} 对应的 frameId"))?;
        self.frame_viewport_offset_to_root(&frame_id).await
    }

    /// FrameTree 是否声明了子 frame（跨域 iframe 可能尚未出现在 /json/list）。
    pub async fn frame_tree_has_child_frames(&self) -> bool {
        let Ok(v) = self.call("Page.getFrameTree", json!({})).await else {
            return false;
        };
        let Some(root) = v.get("result").and_then(|r| r.get("frameTree")) else {
            return false;
        };
        root.get("childFrames")
            .and_then(|c| c.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    }

    /// 等 SPA 真有可交互内容（勿仅凭 readyState=interactive 就动手）。
    /// 条件：可见正文够长，或能数到按钮 / 链接 / 输入框。
    pub async fn wait_spa_content_ready(&self, timeout: std::time::Duration) -> bool {
        const PROBE: &str = r#"( () => {
  try {
    const body = document.body;
    if (!body) return { ok: false, textLen: 0, controls: 0 };
    const text = String(body.innerText || body.textContent || '').replace(/\s+/g, ' ').trim();
    const controls = document.querySelectorAll(
      'button, [role="button"], a[href], input:not([type="hidden"]), textarea, select, [contenteditable="true"]'
    ).length;
    return {
      ok: text.length >= 8 || controls >= 1,
      textLen: text.length,
      controls,
    };
  } catch (e) {
    return { ok: false, textLen: 0, controls: 0 };
  }
})()"#;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(v) = self.evaluate(PROBE).await {
                if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
                    return true;
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    /// 同源子 frame 的默认执行上下文（不含主 frame），按 FrameTree DOM 顺序。
    ///
    /// 同源 iframe 不是独立 CDP target，主文档求值无法穿透其 DOM 边界，
    /// 因此点击/输入等指令必须显式在这些上下文里再跑一遍。
    /// 无法确定主 frame 时返回空，避免在主文档上下文重复执行带副作用的脚本。
    pub async fn child_frame_context_ids(&self) -> Vec<u64> {
        let _ = self.call("Runtime.enable", json!({})).await;
        let _ = self.call("Page.enable", json!({})).await;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        self.backfill_missing_frame_contexts().await;
        let Some(main_id) = self.main_frame_id().await else {
            return Vec::new();
        };
        let ordered = self.ordered_frame_context_ids().await;
        let map = self.inner.frame_contexts.lock();
        let main_ctx = map.get(&main_id).copied();
        ordered
            .into_iter()
            .filter(|id| main_ctx.map(|m| *id != m).unwrap_or(true))
            .collect()
    }

    pub async fn drain_network_urls(&self) -> Vec<String> {
        let mut urls = self.inner.captured_urls.lock().await;
        std::mem::take(&mut *urls)
    }

    /// CDP 在整页跳转/刷新后可能短暂失效，需重新附着到标签页
    pub fn is_session_lost_error(msg: &str) -> bool {
        let m = msg.to_lowercase();
        m.contains("not attached to an active page")
            || m.contains("inspected target navigated or closed")
            || m.contains("target closed")
            || m.contains("session closed")
            || m.contains("cannot find context")
            || m.contains("detached")
    }

    /// 浏览器进程已挂死/退出（OOM、被杀、调试端口不可达），仅刷新页面无效。
    pub fn is_browser_dead_error(msg: &str) -> bool {
        let e = msg.trim();
        let el = e.to_lowercase();
        e.contains("CDP 超时")
            || e.contains("CDP 通道关闭")
            || e.contains("WebSocket 连接失败")
            || e.contains("无法连接调试端口")
            || e.contains("仍无法连接调试端口")
            || e.contains("浏览器已退出")
            || e.contains("浏览器无响应")
            || e.contains("Browser CDP 连接已断开")
            || e.contains("调试端口不可达")
            || e.contains("浏览器崩溃")
            || e.contains("OOM")
            || el.contains("connection refused")
            || el.contains("os error 10061")
            || el.contains("failed to connect")
    }

    pub async fn ensure_attached(&mut self) -> Result<(), String> {
        if self.evaluate("1").await.is_ok() {
            return Ok(());
        }
        let port = self.port;
        let prev_id = self.target_id.clone();
        let hint_url = self.target_url.clone();

        if let Ok(client) = Self::connect_to_target_on(port, &prev_id).await {
            *self = client;
            if self.evaluate("1").await.is_ok() {
                let _ = self.sync_page_meta().await;
                return Ok(());
            }
        }

        let pages = crate::chrome::list_page_targets_on(port).await?;
        let page = pick_reconnect_target(&pages, &hint_url)
            .ok_or_else(|| "浏览器标签已关闭，无法重新附着".to_string())?;
        *self = Self::connect_to_target_on(port, &page.id).await?;
        let _ = self.sync_page_meta().await;
        Ok(())
    }

    /// 点击/导航后等待页面稳定（含 CDP 重连）
    pub async fn wait_after_navigation(&mut self, timeout_secs: u64) -> Result<(), String> {
        self.wait_after_navigation_cancel(timeout_secs, None).await
    }

    pub async fn wait_after_navigation_cancel(
        &mut self,
        timeout_secs: u64,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        use std::time::Instant;
        let deadline = Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let mut stable_ticks = 0u32;
        while Instant::now() < deadline {
            if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
                return Err("已取消".into());
            }
            if self.ensure_attached().await.is_err() {
                stable_ticks = 0;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }
            let ready = self
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            if ready == "complete" || ready == "interactive" {
                stable_ticks += 1;
                if stable_ticks >= 2 {
                    return Ok(());
                }
            } else {
                stable_ticks = 0;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        self.ensure_attached().await
    }

    pub async fn evaluate_resilient(&mut self, expression: &str) -> Result<Value, String> {
        match self.evaluate(expression).await {
            Ok(v) => Ok(v),
            Err(e) if Self::is_session_lost_error(&e) => {
                self.ensure_attached().await?;
                self.evaluate(expression).await
            }
            Err(e) => Err(e),
        }
    }
}

fn should_keep_automation_tab(port: u16, foreground_url: &str) -> bool {
    // 空白弹窗 / 下载直链：跟过去无意义且会丢业务页
    if crate::browser_network::is_stuck_blank_popup_url(foreground_url)
        || crate::client_download::looks_like_download_url(foreground_url)
    {
        return true;
    }
    // 导出点击～等待下载窗口内：短命页签关后前台常跳到其它标签，保持当前附着
    crate::browser_network::is_ephemeral_download_capture_armed(port)
}

fn pick_reconnect_target(
    pages: &[crate::chrome::TargetInfo],
    hint_url: &str,
) -> Option<crate::chrome::TargetInfo> {
    if pages.is_empty() {
        return None;
    }
    let hint_host = url::Url::parse(hint_url)
        .ok()
        .and_then(|u| u.host_str().map(String::from));

    let mut pool: Vec<&crate::chrome::TargetInfo> = pages
        .iter()
        .filter(|t| {
            !t.url.starts_with("chrome://devtools")
                && !t.url.starts_with("devtools://")
        })
        .collect();
    if pool.is_empty() {
        pool = pages.iter().collect();
    }

    if let Some(host) = &hint_host {
        if let Some(p) = pool
            .iter()
            .find(|t| t.url.contains(host.as_str()) && t.url.starts_with("http"))
        {
            return Some((*p).clone());
        }
    }
    if let Some(p) = pool.iter().find(|t| {
        t.url.contains("#/index")
            || t.url.contains("hcmcloud")
            || t.url.contains("/login")
    }) {
        return Some((*p).clone());
    }
    crate::chrome::pick_page_target(pages).cloned()
}

async fn resolve_opener_child_id(
    port: u16,
    parent_target_id: &str,
    known_page_ids: Option<&std::collections::HashSet<String>>,
) -> Result<Option<String>, String> {
    if let Some(tid) = crate::browser_network::latest_tracked_child_on(port, parent_target_id) {
        return Ok(Some(tid));
    }
    if let Ok(targets) = crate::browser_network::get_page_targets_on(port).await {
        if let Some(known) = known_page_ids {
            if let Some(t) =
                crate::chrome::pick_new_child_page_target(&targets, parent_target_id, known)
            {
                return Ok(Some(t.id.clone()));
            }
            if let Some(t) =
                crate::chrome::pick_newest_unknown_page(&targets, known, parent_target_id)
            {
                return Ok(Some(t.id.clone()));
            }
        } else if let Some(t) = crate::chrome::pick_child_page_target(&targets, parent_target_id) {
            return Ok(Some(t.id.clone()));
        }
    }
    if let Some(known) = known_page_ids {
        if let Ok(pages) = crate::chrome::list_page_targets_on(port).await {
            if let Some(p) =
                crate::chrome::pick_newest_unknown_json_page(&pages, known, parent_target_id)
            {
                return Ok(Some(p.id.clone()));
            }
        }
    }
    Ok(None)
}

async fn has_new_page_ids(
    port: u16,
    known: &std::collections::HashSet<String>,
) -> Result<bool, String> {
    let now = crate::chrome::snapshot_all_page_ids_on(port).await?;
    Ok(now.iter().any(|id| !known.contains(id)))
}

async fn page_target_url_ready(port: u16, target_id: &str) -> Result<bool, String> {
    if let Ok(pages) = crate::chrome::list_page_targets_on(port).await {
        if let Some(p) = pages.iter().find(|p| p.id == target_id) {
            return Ok(!crate::browser_network::is_stuck_blank_popup_url(&p.url));
        }
    }
    if let Ok(targets) = crate::browser_network::get_page_targets_on(port).await {
        if let Some(t) = targets.iter().find(|t| t.id == target_id) {
            return Ok(!crate::browser_network::is_stuck_blank_popup_url(&t.url));
        }
    }
    Ok(false)
}

async fn has_popup_follow_signal(
    port: u16,
    parent_target_id: &str,
    known_page_ids: Option<&std::collections::HashSet<String>>,
) -> Result<bool, String> {
    if crate::browser_network::latest_tracked_child_on(port, parent_target_id).is_some() {
        return Ok(true);
    }
    let Some(known) = known_page_ids else {
        return Ok(false);
    };
    if has_new_page_ids(port, known).await? {
        return Ok(true);
    }
    if let Ok(targets) = crate::browser_network::get_page_targets_on(port).await {
        if crate::chrome::pick_new_child_page_target(&targets, parent_target_id, known).is_some() {
            return Ok(true);
        }
        if crate::chrome::pick_newest_unknown_page(&targets, known, parent_target_id).is_some() {
            return Ok(true);
        }
    }
    if let Ok(pages) = crate::chrome::list_page_targets_on(port).await {
        if crate::chrome::pick_newest_unknown_json_page(&pages, known, parent_target_id).is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// CDP 调试策略：跳过断点/异常暂停，并恢复等待中的页面
pub fn automation_debugger_policy_steps() -> [(&'static str, Value); 5] {
    [
        ("Runtime.runIfWaitingForDebugger", json!({})),
        ("Debugger.enable", json!({})),
        ("Debugger.setSkipAllPauses", json!({ "skip": true })),
        ("Debugger.setBreakpointsActive", json!({ "active": false })),
        ("Debugger.resume", json!({})),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_guard_script_blurs_on_hidden() {
        assert!(AUTOMATION_GUARD_SCRIPT.contains("visibilitychange"));
        assert!(AUTOMATION_GUARD_SCRIPT.contains("blur"));
    }

    #[test]
    fn keep_tab_for_blank_or_download_foreground() {
        assert!(should_keep_automation_tab(0, "about:blank"));
        assert!(should_keep_automation_tab(
            0,
            "https://example.com/files/report.xls"
        ));
        // 端口无捕获会话时，普通页面不拦截跟随
        assert!(!should_keep_automation_tab(0, "https://example.com/app"));
    }
}
