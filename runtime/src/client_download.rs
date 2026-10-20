use crate::cdp::CdpClient;
use crate::run_artifacts::{RunArtifact, RunSession};
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use crate::ui_host::{AppHandle, Runtime};
use url::Url;

/// 同一 URL 拉取失败超过此次数后放弃，避免死循环。
const MAX_DOWNLOAD_FETCH_ATTEMPTS: u32 = 8;

/// 下载截获追踪：写入当前 RunSession 的 executions/download-capture.log（若已登记）。
static ACTIVE_DOWNLOAD_TRACE: std::sync::OnceLock<
    parking_lot::Mutex<Option<std::sync::Weak<RunSession>>>,
> = std::sync::OnceLock::new();

pub fn set_active_download_trace(session: &Arc<RunSession>) {
    let slot = ACTIVE_DOWNLOAD_TRACE.get_or_init(|| parking_lot::Mutex::new(None));
    *slot.lock() = Some(Arc::downgrade(session));
}

pub fn clear_active_download_trace() {
    if let Some(slot) = ACTIVE_DOWNLOAD_TRACE.get() {
        *slot.lock() = None;
    }
}

fn with_download_trace(f: impl FnOnce(&RunSession)) {
    let Some(slot) = ACTIVE_DOWNLOAD_TRACE.get() else {
        return;
    };
    let weak = slot.lock().clone();
    if let Some(sess) = weak.and_then(|w| w.upgrade()) {
        f(&sess);
    }
}

pub fn trace_download(line: impl AsRef<str>) {
    let line = line.as_ref();
    with_download_trace(|s| s.append_download_trace(line));
}

pub fn trace_download_queue(url: &str, suggested: Option<&str>, force: bool, looks: bool) {
    let name = suggested.unwrap_or("-");
    let short = if url.len() > 160 {
        format!("{}…", &url[..160])
    } else {
        url.to_string()
    };
    trace_download(format!(
        "入队 url={short} | suggested={name} | force={force} | looksUrl={looks}"
    ));
}

/// 「等待下载」期间临时武装自动落盘；drop 时若策略为 wait-only 则解除。
pub struct AutoFetchWaitGuard<'a> {
    session: &'a RunSession,
}

impl<'a> AutoFetchWaitGuard<'a> {
    pub fn enter(session: &'a RunSession) -> Self {
        Self { session }
    }
}

impl Drop for AutoFetchWaitGuard<'_> {
    fn drop(&mut self) {
        if self.session.is_auto_fetch_wait_only() {
            self.session.set_auto_fetch_armed(false);
            self.session
                .append_download_trace("「等待下载」结束：已解除自动落盘武装");
        }
    }
}

/// 通用下载捕获：仅在 `__webclawClientDownload===true`（自动化运行中）时拦截导航；
/// 空闲时放行浏览器本地下载，用户可手动点「保留」接受 HTTP 不安全下载提示。
pub const DOWNLOAD_HOOK_SCRIPT: &str = r#"
(function () {
  if (window.__webclawDlHook) return;
  window.__webclawDlHook = true;
  function captureEnabled() {
    try { return window.__webclawClientDownload === true; } catch (e) { return false; }
  }
  function queueRoot() {
    // OOPIF 无法写 top；队列挂在本窗口，由 CDP 逐 frame drain
    window.__webclawPendingDownloads = window.__webclawPendingDownloads || [];
    return window.__webclawPendingDownloads;
  }
  function looksLikeDownload(url) {
    if (!url || typeof url !== 'string') return false;
    var u = url.toLowerCase();
    // Chrome 原生打印预览：绝不能当业务下载（第二次点打印常误入此路径）
    if (/^(chrome|chrome-untrusted|chrome-extension|devtools|edge):/i.test(u)) return false;
    if (/\.(xlsx|xls|csv|pdf|zip|docx?)(\?|#|&|$)/i.test(u)) return true;
    if (/file_name=|filename=|[?&]file=/i.test(u)) return true;
    if (/\/document\/exp\//i.test(u) || /common_excel/i.test(u)) return true;
    if (/附件|导出|月报/i.test(url)) return true;
    // 旧 MES 等：/exportbypage?downloadAll=true，无扩展名
    // 注意：勿用裸 /export/ —— 飞书审批 queryByIds/queryList/batchExport 等是 JSON 接口
    if (/exportbypage|exportby[a-z0-9]/i.test(u)) return true;
    if (/\/export(?:\?|$)/i.test(u)) return true;
    if (/\/export\/(?:[^/?#]*download|download)/i.test(u)) return true;
    if (/[?&]downloadall=/i.test(u)) return true;
    if (/monthlyreport/i.test(u) && /(export|download)/i.test(u)) return true;
    // 勿用裸 file：泛微 weaver.file.FileDownload 会误伤首页图片
    if (/(export|download)/i.test(u) && /(excel|xls|csv|\/exp\/)/i.test(u)) return true;
    return false;
  }
  function enqueue(u) {
    if (!captureEnabled()) return;
    if (u == null || u === '') return;
    if (typeof u === 'object' && u && u.url) u = u.url;
    if (typeof u !== 'string') return;
    try {
      u = new URL(u, document.baseURI).href;
    } catch (e) {
      return;
    }
    if (u.startsWith('blob:') || u.startsWith('javascript:') || u.startsWith('data:')) return;
    if (!looksLikeDownload(u)) return;
    var q = queueRoot();
    if (q.some(function (x) { return x && x.url === u; })) return;
    q.push({ url: u, ts: Date.now() });
  }
  function makeFakeWindow(url) {
    var href = typeof url === 'string' ? url : 'about:blank';
    var loc = {
      _href: href,
      assign: function (u) { enqueue(u); this._href = u; },
      replace: function (u) { enqueue(u); this._href = u; },
      reload: function () {}
    };
    Object.defineProperty(loc, 'href', {
      get: function () { return loc._href; },
      set: function (u) { enqueue(u); loc._href = u; },
      configurable: true
    });
    return {
      closed: false,
      close: function () { this.closed = true; },
      focus: function () {},
      blur: function () {},
      postMessage: function () {},
      location: loc
    };
  }
  var origOpen = window.open.bind(window);
  window.open = function (url, target, features) {
    if (captureEnabled() && looksLikeDownload(typeof url === 'string' ? url : '')) {
      enqueue(url);
      return makeFakeWindow(url);
    }
    return origOpen(url, target, features);
  };
  document.addEventListener(
    'click',
    function (ev) {
      if (!captureEnabled()) return;
      const a = ev.target && ev.target.closest ? ev.target.closest('a[href]') : null;
      if (!a) return;
      const href = a.getAttribute('href');
      if (!href) return;
      if (a.hasAttribute('download') || looksLikeDownload(href)) {
        enqueue(href);
        try { ev.preventDefault(); ev.stopPropagation(); } catch (e) {}
      }
    },
    true
  );
  try {
    var desc = Object.getOwnPropertyDescriptor(Location.prototype, 'href')
      || Object.getOwnPropertyDescriptor(window.location.__proto__ || {}, 'href');
    if (desc && desc.set) {
      var origSet = desc.set;
      Object.defineProperty(window.location, 'href', {
        configurable: true,
        get: function () { return desc.get ? desc.get.call(window.location) : ''; },
        set: function (u) {
          if (captureEnabled() && looksLikeDownload(String(u || ''))) {
            enqueue(u);
            return;
          }
          return origSet.call(window.location, u);
        }
      });
    }
  } catch (e) {}
  if (window.fetch) {
    var origFetch = window.fetch;
    window.fetch = function (input, init) {
      var url = typeof input === 'string' ? input : (input && input.url) || '';
      var method = 'GET';
      try {
        if (init && init.method) method = String(init.method);
        else if (input && typeof input === 'object' && input.method) method = String(input.method);
      } catch (e) {}
      var m = method.toUpperCase();
      if (m === 'GET' || m === 'HEAD') enqueue(url);
      else if (looksLikeDownload(String(url || ''))) enqueue(url);
      return origFetch.apply(this, arguments).then(function (resp) {
        try {
          if (m === 'GET' || m === 'HEAD' || looksLikeDownload(String(url || ''))) {
            enqueue(resp.url || url);
          }
        } catch (e) {}
        return resp;
      });
    };
  }
  if (window.XMLHttpRequest) {
    var origOpenXhr = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
      this.__webclawUrl = url;
      this.__webclawMethod = method ? String(method).toUpperCase() : 'GET';
      if (this.__webclawMethod === 'GET' || this.__webclawMethod === 'HEAD'
          || looksLikeDownload(String(url || ''))) {
        enqueue(url);
      }
      return origOpenXhr.apply(this, arguments);
    };
    var origSend = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.send = function () {
      var xhr = this;
      xhr.addEventListener('load', function () {
        try {
          if (xhr.__webclawMethod === 'GET' || xhr.__webclawMethod === 'HEAD') {
            enqueue(xhr.responseURL || xhr.__webclawUrl);
          }
        } catch (e) {}
      });
      return origSend.apply(this, arguments);
    };
  }
  document.addEventListener(
    'submit',
    function (ev) {
      if (!captureEnabled()) return;
      var f = ev.target;
      if (!f || !f.action) return;
      if (looksLikeDownload(f.action)) {
        enqueue(f.action);
        try { ev.preventDefault(); } catch (e) {}
      } else {
        enqueue(f.action);
      }
    },
    true
  );
})();
"#;

const DRAIN_PENDING_SCRIPT: &str = r#"
(() => {
  const q = window.__webclawPendingDownloads || [];
  window.__webclawPendingDownloads = [];
  return q;
})()
"#;

const SCAN_PERFORMANCE_SCRIPT: &str = r#"
(() => {
  const out = [];
  try {
  const seen = new Set();
  const now = performance.now();
  const maxAgeMs = 45000;
  function add(u) {
    if (!u || seen.has(u)) return;
    seen.add(u);
    out.push(u);
  }
  (performance.getEntriesByType('resource') || []).forEach(function (e) {
    if (e.responseEnd && (now - e.responseEnd) > maxAgeMs) return;
    add(e.name);
  });
  } catch (e) {}
  return out;
})()
"#;

/// 仅用于判断捕获钩子是否仍有效（reinject / 超时摘要），不含调试字段。
const CAPTURE_ALIVE_SCRIPT: &str = r#"
(() => ({
  hooked: !!window.__webclawDlHook,
  printHook: !!window.__webclawPrintHook && window.__webclawPrintWrapped === window.print,
  capture: window.__webclawClientDownload === true
}))()
"#;

const SET_CAPTURE_MODE_SCRIPT: &str = r#"
(function (enabled) {
  window.__webclawClientDownload = !!enabled;
  try {
    document.querySelectorAll('iframe').forEach(function (el) {
      try {
        if (el.contentWindow) el.contentWindow.__webclawClientDownload = !!enabled;
      } catch (e) {}
    });
  } catch (e) {}
})
"#;

/// 新文档一创建就开捕获。子标签导航会清掉 evaluate 设的 flag，必须靠 addScriptToEvaluateOnNewDocument。
/// 任务结束时用 clear_new_document_script("capture-boot") 卸掉。
pub const CAPTURE_BOOTSTRAP_SCRIPT: &str = r#"
try { window.__webclawClientDownload = true; } catch (e) {}
"#;

const INJECT_SAME_ORIGIN_FRAMES: &str = r#"
(function (src) {
  try { eval(src); } catch (e) {}
  document.querySelectorAll('iframe').forEach(function (el) {
    try {
      var w = el.contentWindow;
      if (w && !w.__webclawDlHook) w.eval(src);
    } catch (e) {}
  });
})
"#;

pub async fn install_hooks(client: &CdpClient) -> Result<(), String> {
    // 先挂捕获引导：打印子页导航后 document 重建时仍保持 __webclawClientDownload=true
    client
        .ensure_new_document_script("capture-boot", CAPTURE_BOOTSTRAP_SCRIPT)
        .await?;
    // 每个 target 只挂一次：重复 addScriptToEvaluateOnNewDocument 会在每次刷新时叠跑大钩子
    client
        .ensure_new_document_script("download-hook", DOWNLOAD_HOOK_SCRIPT)
        .await?;
    client.evaluate(DOWNLOAD_HOOK_SCRIPT).await?;
    let wrapped = format!(
        "({INJECT_SAME_ORIGIN_FRAMES})({script})",
        script = serde_json::to_string(DOWNLOAD_HOOK_SCRIPT).map_err(|e| e.to_string())?
    );
    let _ = client.evaluate(&wrapped).await;
    for ctx_id in client.frame_context_ids().await {
        let _ = client
            .evaluate_in_context(DOWNLOAD_HOOK_SCRIPT, Some(ctx_id))
            .await;
    }
    // OOPIF：跨域 iframe 为独立 CDP target，需单独附着注入
    if let Ok(iframes) =
        crate::chrome::list_iframe_targets_for_parent_on(client.port, &client.target_id).await
    {
        for iframe in iframes {
            if let Ok(child) =
                CdpClient::connect_to_any_target_on(client.port, &iframe.id).await
            {
                let _ = child
                    .ensure_new_document_script("capture-boot", CAPTURE_BOOTSTRAP_SCRIPT)
                    .await;
                let _ = child
                    .ensure_new_document_script("download-hook", DOWNLOAD_HOOK_SCRIPT)
                    .await;
                let _ = child.evaluate(DOWNLOAD_HOOK_SCRIPT).await;
            }
        }
    }
    // 方案 A：拦截 window.print → 静默 Page.printToPDF → downloads/
    let _ = crate::print_pdf::install_print_hooks(client).await;
    Ok(())
}

/// 导航/登录后页面会丢失 `__webclawClientDownload`；导出前后必须调用。
/// 每次都会重新加上 Browser.deny —— 只开页面开关不够，CDP 策略被冲掉后客户端下载会静默失败。
pub async fn ensure_automation_capture(client: &CdpClient) -> Result<(), String> {
    let _ = crate::browser_network::set_download_behavior_on(client.port, true).await;
    install_hooks(client).await?;
    set_capture_mode(client, true).await?;
    Ok(())
}

/// 子标签 early_attach 专用：只挂 capture-boot + print 钩子并开捕获。
/// 跳过下载钩子 / OOPIF / iframe 遍历，避免丢掉 window.print 竞态窗口（完整 ensure 曾达数秒）。
pub async fn ensure_print_capture_fast(client: &CdpClient) -> Result<(), String> {
    client
        .ensure_new_document_script("capture-boot", CAPTURE_BOOTSTRAP_SCRIPT)
        .await?;
    let _ = client.evaluate(CAPTURE_BOOTSTRAP_SCRIPT).await;
    crate::print_pdf::install_print_hooks_fast(client).await?;
    // 仅当前文档，不扫 iframe target（list/connect 是慢点）
    let expr = format!("({SET_CAPTURE_MODE_SCRIPT})(true)");
    let _ = client.evaluate(&expr).await;
    Ok(())
}

/// 切换页面内是否拦截下载导航（自动化=true，空闲=false 放行浏览器下载 UI）
pub async fn set_capture_mode(client: &CdpClient, enabled: bool) -> Result<(), String> {
    let expr = format!(
        "({SET_CAPTURE_MODE_SCRIPT})({})",
        if enabled { "true" } else { "false" }
    );
    let _ = client.evaluate(&expr).await;
    if let Ok(iframes) =
        crate::chrome::list_iframe_targets_for_parent_on(client.port, &client.target_id).await
    {
        for iframe in iframes {
            if let Ok(child) =
                CdpClient::connect_to_any_target_on(client.port, &iframe.id).await
            {
                let _ = child.evaluate(&expr).await;
            }
        }
    }
    Ok(())
}

/// 共用调试浏览器会残留多标签：只关 preferred 一页会导致「第一次成功、第二次原生打印」。
async fn set_capture_mode_all_pages(port: u16, enabled: bool) {
    let Ok(pages) = crate::chrome::list_page_targets_on(port).await else {
        return;
    };
    for page in pages {
        if is_browser_internal_url(&page.url) {
            continue;
        }
        if let Ok(c) = CdpClient::connect_to_any_target_light_on(port, &page.id).await {
            if enabled {
                // 开捕获时连钩子一起装，避免残留页 print 已被覆盖却只刷了 flag
                let _ = ensure_automation_capture(&c).await;
            } else {
                let _ = c.clear_new_document_script("capture-boot").await;
                let _ = set_capture_mode(&c, false).await;
            }
        }
    }
}

/// 快捷指令/任务开始：CDP deny + 页面拦截，由客户端拉取
pub async fn enter_automation_download_mode(port: u16, preferred_target_id: Option<&str>) {
    let _ = crate::browser_network::set_download_behavior_on(port, true).await;
    // 清掉上次原生打印预览残留的假 PDF URL，避免武装后立刻重试死循环
    let _ = crate::browser_network::clear_download_urls_on(port);
    if let Ok(client) = connect_preferred_page(port, preferred_target_id).await {
        let _ = install_hooks(&client).await;
        let _ = set_capture_mode(&client, true).await;
    }
    // 所有业务页一并开捕获（含上次留下的 isprint=1 页与父流程页）
    set_capture_mode_all_pages(port, true).await;
}

/// 任务结束/暂停/空闲：恢复浏览器本地下载（CDP default + 关闭当前页拦截）
pub async fn exit_automation_download_mode(port: u16, preferred_target_id: Option<&str>) {
    crate::browser_network::force_release_new_target_pause(port).await;
    let _ = crate::browser_network::set_download_behavior_on(port, false).await;
    if let Ok(client) = connect_preferred_page(port, preferred_target_id).await {
        let _ = set_capture_mode(&client, false).await;
    }
    // 必须扫全部页签：否则父页仍 capture=true / 打印页仍开着，下次行为漂移
    set_capture_mode_all_pages(port, false).await;
    let _ = crate::browser_network::clear_download_urls_on(port);
}

async fn connect_preferred_page(
    port: u16,
    preferred_target_id: Option<&str>,
) -> Result<CdpClient, String> {
    if let Some(tid) = preferred_target_id.filter(|s| !s.is_empty()) {
        if let Ok(c) = CdpClient::connect_to_target_on(port, tid).await {
            return Ok(c);
        }
    }
    CdpClient::connect_on(port).await
}

/// 任务停止后的轻量收尾：新建 Page CDP（不受已取消会话影响）
pub async fn connect_page_for_cleanup(
    port: u16,
    preferred_target_id: Option<&str>,
) -> Result<CdpClient, String> {
    connect_preferred_page(port, preferred_target_id).await
}

/// Chrome / Edge 内部页（含原生打印预览 `chrome-untrusted://print/.../print.pdf`）。
pub fn is_browser_internal_url(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("chrome://")
        || u.starts_with("chrome-untrusted://")
        || u.starts_with("chrome-extension://")
        || u.starts_with("devtools://")
        || u.starts_with("edge://")
}

/// 与页面脚本 `looksLikeDownload` 对齐的 Rust 侧 URL 判定。
pub fn looks_like_download_url(url: &str) -> bool {
    let u = url.to_lowercase();
    if u.starts_with("blob:") || u.starts_with("javascript:") || u.starts_with("data:") {
        return false;
    }
    // 原生打印预览 PDF：第一次成功后再点打印常弹出，误抓会拖死「等待下载」
    if is_browser_internal_url(url) {
        return false;
    }
    // 泛微 Ecology：首页轮播/内刊缩略图也走 FileDownload？fileid=…，不能当业务下载
    if u.contains("weaver.file.filedownload") {
        return false;
    }
    if regex_like_ext(&u) {
        return true;
    }
    if u.contains("file_name=") || u.contains("filename=") || u.contains("file=") {
        return true;
    }
    if u.contains("/document/exp/") || u.contains("common_excel") {
        return true;
    }
    if url.contains("导出") || url.contains("月报") || url.contains("附件") {
        return true;
    }
    // 旧 MES 考勤月报：…/exportbypage?downloadAll=true（无扩展名，响应为 xls 二进制）
    // 勿用裸 contains("/export/")：飞书审批 /export/queryByIds、queryList、batchExport 是 JSON，不是文件
    if u.contains("exportbypage") || contains_exportby_action(&u) {
        return true;
    }
    if u.contains("/export?") || u.ends_with("/export") {
        return true;
    }
    if is_export_download_path(&u) {
        return true;
    }
    if u.contains("downloadall=") {
        return true;
    }
    if u.contains("monthlyreport") && (u.contains("export") || u.contains("download")) {
        return true;
    }
    // 勿用裸 contains("file")：会匹配 weaver.file / profile 等
    if u.contains("attachment")
        || ((u.contains("export") || u.contains("download"))
            && (u.contains("excel")
                || u.contains("xls")
                || u.contains("csv")
                || u.contains("/exp/")))
    {
        return true;
    }
    if u.contains("hcmcloud.cn") && (u.contains("/exp/") || u.contains("excel")) {
        return true;
    }
    false
}

/// 供 browser_network 解析 Content-Disposition。
pub fn filename_from_content_disposition_str_pub(raw: &str) -> Option<String> {
    filename_from_content_disposition_str(raw)
}

/// `exportby…` 动作名（exportbypage / exportbyid），避免误伤 `exported` 等词。
fn contains_exportby_action(u: &str) -> bool {
    let Some(i) = u.find("exportby") else {
        return false;
    };
    let rest = &u[i + "exportby".len()..];
    rest.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
}

/// 真正落文件的 export 路径：…/export/download、…/export/xxxDownload
fn is_export_download_path(u: &str) -> bool {
    let path = u.split(['?', '#']).next().unwrap_or(u);
    if !path.contains("/export/") {
        return false;
    }
    // …/export/download 或末段含 download（downloadFile 等）
    if path.contains("/export/download") {
        return true;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    last.contains("download")
}

fn regex_like_ext(u: &str) -> bool {
    // 不含 .bin：扩展名过泛，WPS/CDN 杂讯常被误抓
    const EXT: [&str; 7] = [".xlsx", ".xls", ".csv", ".pdf", ".zip", ".docx", ".doc"];
    for ext in EXT {
        if let Some(i) = u.find(ext) {
            let after = i + ext.len();
            if after >= u.len() {
                return true;
            }
            match u.as_bytes().get(after) {
                // query 里常见 file_name=a.xlsx&category=…
                Some(b'?' | b'#' | b'&') => return true,
                _ => {}
            }
        }
    }
    false
}

pub async fn drain_target_urls(
    port: u16,
    seen: &mut HashSet<String>,
) -> Result<Vec<String>, String> {
    let targets = crate::chrome::list_targets_on(port).await?;
    let mut out = Vec::new();
    for t in targets {
        if looks_like_download_url(&t.url) && seen.insert(t.url.clone()) {
            out.push(t.url);
        }
    }
    Ok(out)
}

pub async fn collect_pending_urls(
    client: &CdpClient,
    seen_targets: &mut HashSet<String>,
    session: &RunSession,
) -> Result<Vec<String>, String> {
    let mut urls = drain_pending_urls(client).await?;
    urls.extend(client.drain_network_urls().await);
    urls.extend(scan_performance_urls(client).await?);
    if let Ok(cap) = crate::browser_network::BrowserNetworkCapture::shared_for_port(client.port).await
    {
        urls.extend(cap.drain_download_urls());
    }
    urls.extend(drain_target_urls(client.port, seen_targets).await?);
    urls.retain(|u| !session.is_download_url_handled(u));
    urls.sort();
    urls.dedup();
    Ok(urls)
}

pub async fn scan_performance_urls(client: &CdpClient) -> Result<Vec<String>, String> {
    let v = client.evaluate(SCAN_PERFORMANCE_SCRIPT).await?;
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for item in arr {
            if let Some(url) = item.as_str() {
                if looks_like_download_url(url) {
                    out.push(url.to_string());
                }
            }
        }
    }
    Ok(out)
}

/// 捕获钩子是否仍有效（用于 reinject / 超时摘要）。
pub async fn capture_hooks_alive(client: &CdpClient) -> bool {
    match client.evaluate(CAPTURE_ALIVE_SCRIPT).await {
        Ok(v) => {
            v.get("hooked").and_then(|x| x.as_bool()).unwrap_or(false)
                && v.get("printHook").and_then(|x| x.as_bool()).unwrap_or(false)
                && v.get("capture").and_then(|x| x.as_bool()).unwrap_or(false)
        }
        Err(_) => false,
    }
}

pub async fn download_diagnostic(client: &CdpClient) -> String {
    let hook = match client.evaluate(CAPTURE_ALIVE_SCRIPT).await {
        Ok(v) => format!(
            "hook={} printHook={} capture={}",
            v.get("hooked").and_then(|x| x.as_bool()).unwrap_or(false),
            v.get("printHook").and_then(|x| x.as_bool()).unwrap_or(false),
            v.get("capture").and_then(|x| x.as_bool()).unwrap_or(false),
        ),
        Err(e) => format!("hook状态读取失败: {e}"),
    };
    let browser = match crate::browser_network::BrowserNetworkCapture::shared_for_port(client.port).await
    {
        Ok(cap) => cap.diagnostic_summary(),
        Err(e) => format!("browser监听: {e}"),
    };
    format!("{hook} | {browser}")
}

pub async fn drain_pending_urls(client: &CdpClient) -> Result<Vec<String>, String> {
    let mut out = drain_pending_on_client(client).await?;
    if let Ok(iframes) =
        crate::chrome::list_iframe_targets_for_parent_on(client.port, &client.target_id).await
    {
        for iframe in iframes {
            if let Ok(child) =
                CdpClient::connect_to_any_target_on(client.port, &iframe.id).await
            {
                if let Ok(urls) = drain_pending_on_client(&child).await {
                    out.extend(urls);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

async fn drain_pending_on_client(client: &CdpClient) -> Result<Vec<String>, String> {
    let v = client.evaluate(DRAIN_PENDING_SCRIPT).await?;
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for item in arr {
            if let Some(url) = item.get("url").and_then(|x| x.as_str()) {
                if !url.is_empty() {
                    out.push(url.to_string());
                }
            }
        }
    }
    Ok(out)
}

/// 处理队列中的 URL，逐个客户端下载并登记产物。
/// 仅在成功落盘后标记 URL 已处理；失败会重新入队并有限重试。
pub async fn process_pending<R: crate::ui_host::Runtime>(
    client: &CdpClient,
    session: &RunSession,
    step_index: Option<u32>,
    seen_targets: &mut HashSet<String>,
    progress: Option<&crate::task_progress::DownloadProgressSink<'_, R>>,
) -> Result<Vec<RunArtifact>, String> {
    let mut added = Vec::new();
    // 打印预览抢焦点时，先清掉再处理队列/兜底
    let _ = crate::print_pdf::dismiss_native_print_ui(client.port).await;
    // 先处理打印拦截队列（window.print → PDF），再处理普通下载 URL
    match crate::print_pdf::process_pending_prints(client, session, step_index).await {
        Ok(prints) if !prints.is_empty() => {
            let names: Vec<_> = prints.iter().map(|a| a.name.as_str()).collect();
            session.append_download_trace(format!(
                "打印拦截 | 本轮落盘 {} 个: {}",
                prints.len(),
                names.join(", ")
            ));
            if let Some(p) = progress {
                p.done(names[0]);
            }
            added.extend(prints);
        }
        Err(e) => {
            session.append_download_trace(format!("打印拦截 | 处理异常: {e}"));
        }
        Ok(_) => {}
    }

    let urls = collect_pending_urls(client, seen_targets, session).await?;
    if !urls.is_empty() {
        session.append_download_trace(format!("待处理 {} 个 URL", urls.len()));
    }
    let mut last_err = None;
    for url in urls {
        if !session.try_begin_download_url(&url) {
            session.append_download_trace(format!("跳过（已在途/已处理）: {url}"));
            continue;
        }
        let label = preview_filename_from_url(&url);
        session.append_download_trace(format!("开始拉取: {label} | {url}"));
        if let Some(p) = progress {
            p.fetching(&label);
        }
        match fetch_to_session(client, session, &url, step_index).await {
            Ok(art) => {
                session.mark_download_url_handled(&url);
                session.append_download_trace(format!(
                    "已落盘: {} ({} 字节)",
                    art.name, art.size_bytes
                ));
                if let Some(p) = progress {
                    p.done(&art.name);
                }
                added.push(art);
            }
            Err(e) => {
                let msg = format!("{url} → {e}");
                session.append_download_trace(format!("拉取失败: {e}"));
                // 已判定为非业务垃圾：直接标记完成，禁止重试刷 UI
                if e.contains("忽略非业务下载") {
                    session.mark_download_url_handled(&url);
                    continue;
                }
                match session.record_download_fetch_failure(&url, MAX_DOWNLOAD_FETCH_ATTEMPTS) {
                    Some(n) => {
                        session.append_download_trace(format!("将重试 ({n}/{MAX_DOWNLOAD_FETCH_ATTEMPTS})"));
                        crate::browser_network::note_download_url_on(client.port, &url);
                    }
                    None => {
                        last_err = Some(format!("{msg}（已重试 {MAX_DOWNLOAD_FETCH_ATTEMPTS} 次）"));
                        continue;
                    }
                }
                last_err = Some(msg);
            }
        }
    }
    if !added.is_empty() {
        return Ok(added);
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    Ok(added)
}

/// 任务期间自动把截获到的下载落到 `downloads/`；「等待下载」只负责同步屏障。
pub struct AutoDownloadPump {
    stop: Arc<AtomicBool>,
    join: tokio::task::JoinHandle<()>,
}

impl AutoDownloadPump {
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.join.abort();
    }
}

pub fn spawn_auto_download_pump<R: Runtime>(
    app: AppHandle<R>,
    task_id: String,
    port: u16,
    session: Arc<RunSession>,
    active_browser: crate::browser_session::ActiveBrowserSlot,
    recipe_cancel: Arc<AtomicBool>,
    recipe_pause: Arc<AtomicBool>,
    task_log: Option<crate::task_run::TaskControl>,
) -> AutoDownloadPump {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    set_active_download_trace(&session);
    let join = tokio::spawn(async move {
        let mut seen_targets = HashSet::new();
        let mut last_reinject = Instant::now() - Duration::from_secs(60);
        let mut last_disarmed_log = Instant::now() - Duration::from_secs(30);
        let mut client: Option<CdpClient> = None;
        let mut last_target_id = String::new();
        while !stop_flag.load(Ordering::SeqCst) && !recipe_cancel.load(Ordering::SeqCst) {
            if recipe_pause.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            if !session.is_auto_fetch_armed() {
                // 未武装时不要挂着 Page CDP，避免空转 attach 占内存
                client = None;
                if last_disarmed_log.elapsed() >= Duration::from_secs(15) {
                    let msg = "自动落盘未武装（仅在「等待下载」及其前一步拉取）";
                    session.append_download_trace(msg);
                    if let Some(c) = &task_log {
                        c.push_log(format!("下载截获 | {msg}"));
                    }
                    last_disarmed_log = Instant::now();
                }
                // 短睡以便「点击导出」刚武装时尽快醒来，避免瞬时下载窗口漏接
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            let preferred = {
                let g = active_browser.lock();
                g.as_ref()
                    .filter(|s| s.task_id == task_id && s.port == port)
                    .map(|s| s.target_id.clone())
            };
            let preferred_changed = preferred
                .as_ref()
                .is_some_and(|id| id != &last_target_id);
            if preferred_changed {
                client = None;
            }

            // 复用 Page CDP；仅在失效 / 切标签时重建（旧逻辑每 400ms 新建连接会导致 renderer 原生内存暴涨）
            let need_connect = match client.as_ref() {
                None => true,
                Some(c) => c.evaluate("1").await.is_err(),
            };
            if need_connect {
                match connect_preferred_page(port, preferred.as_deref()).await {
                    Ok(c) => {
                        last_target_id = c.target_id.clone();
                        client = Some(c);
                    }
                    Err(_) => {
                        client = None;
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                }
            }

            let Some(c) = client.as_ref() else {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };

            if last_reinject.elapsed() >= Duration::from_secs(30) || preferred_changed {
                let need_hooks = !capture_hooks_alive(c).await;
                if need_hooks || preferred_changed {
                    let _ = ensure_automation_capture(c).await;
                } else {
                    // 钩子仍在：只刷新 Browser.deny，避免反复 install_hooks / iframe 再附着
                    let _ = crate::browser_network::set_download_behavior_on(port, true).await;
                }
                last_reinject = Instant::now();
            }

            let progress = crate::task_progress::DownloadProgressSink::new(&app, &task_id);
            let idle = match process_pending(
                c,
                &session,
                None,
                &mut seen_targets,
                Some(&progress),
            )
            .await
            {
                Ok(added) if !added.is_empty() => {
                    let names: Vec<_> = added.iter().map(|a| a.name.as_str()).collect();
                    let line = format!("下载截获 | 已落盘: {}", names.join(", "));
                    if let Some(ctrl) = &task_log {
                        ctrl.push_log(&line);
                    }
                    crate::run_artifacts::emit_artifacts_update(&app, &session);
                    false
                }
                Err(e) => {
                    let line = format!("下载截获 | 拉取异常: {e}");
                    session.append_download_trace(&e);
                    if let Some(ctrl) = &task_log {
                        ctrl.push_log(&line);
                    }
                    if CdpClient::is_browser_dead_error(&e)
                        || CdpClient::is_session_lost_error(&e)
                        || e.contains("CDP 通道关闭")
                        || e.contains("CDP 超时")
                    {
                        client = None;
                    }
                    true
                }
                _ => true,
            };

            // 空闲降频：无 URL 时不要 400ms 狂扫 evaluate / list_targets
            let sleep_ms = if idle { 2500 } else { 400 };
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }
        clear_active_download_trace();
    });
    AutoDownloadPump { stop, join }
}

pub fn preview_filename_from_url(url: &str) -> String {
    filename_from_download_url(url).unwrap_or_else(|| "文件".into())
}

pub async fn fetch_to_session(
    client: &CdpClient,
    session: &RunSession,
    url: &str,
    step_index: Option<u32>,
) -> Result<RunArtifact, String> {
    let capture = crate::browser_network::BrowserNetworkCapture::shared_for_port(client.port)
        .await
        .ok();
    let mut suggested = capture
        .as_ref()
        .and_then(|cap| cap.take_suggested_filename(url));
    // POST /export 等：必须带回 Authorization + body，否则 deny 后重拉会 401
    let replay = capture
        .as_ref()
        .and_then(|cap| cap.take_download_replay(url));
    if let Some(ref r) = replay {
        let has_auth = r
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("authorization"));
        trace_download(format!(
            "下载回放 | {} {} | headers={} | body={} | auth={}",
            r.method,
            url,
            r.headers.len(),
            r.post_data.as_ref().map(|b| b.len()).unwrap_or(0),
            has_auth
        ));
    }

    let finish = |session: &RunSession,
                  suggested: &mut Option<String>,
                  url: &str,
                  port: u16,
                  bytes: Vec<u8>,
                  cd: Option<String>,
                  ct: Option<String>,
                  step_index: Option<u32>|
     -> Result<RunArtifact, String> {
        // 拉取期间 responseReceived 可能才写入建议名
        if suggested
            .as_deref()
            .is_none_or(|s| s.is_empty() || is_generic_download_basename(s))
        {
            if let Some(n) = crate::browser_network::capture_for_port(port)
                .and_then(|cap| cap.take_suggested_filename(url))
            {
                *suggested = Some(n);
            }
        }
        let filename = resolve_download_filename(
            cd.as_deref(),
            suggested.as_deref(),
            url,
            ct.as_deref(),
            &bytes,
            Some(port),
        );
        if is_junk_download_payload(&bytes, &filename) {
            trace_download(format!("忽略垃圾载荷: {filename} | {url}"));
            return Err(format!("忽略非业务下载: {filename}"));
        }
        trace_download(format!("落盘文件名 | {filename}"));
        session.save_download_bytes(&filename, &bytes, step_index)
    };

    // 飞书：优先用浏览器登录态（页内 fetch）或开放平台 token；裸 reqwest 常因缺用户会话返回 401
    if is_feishu_related_url(url) {
        match fetch_feishu_to_bytes(client, url, replay.as_ref()).await {
            Ok((bytes, filename_hint, cd, ct)) => {
                if suggested.is_none() {
                    suggested = filename_hint;
                }
                return finish(
                    session,
                    &mut suggested,
                    url,
                    client.port,
                    bytes,
                    Some(cd),
                    Some(ct),
                    step_index,
                );
            }
            Err(e) => {
                trace_download(format!("飞书专用拉取失败，回退通用路径: {e}"));
            }
        }
    }

    // 有回放（尤其 POST + Bearer）：先按原 method/headers/body 拉，避免 GET 无鉴权 401
    if let Some(ref r) = replay {
        if replay_needs_full_request(r) {
            match fetch_via_page_with_replay(client, url, r).await {
                Ok((bytes, cd, ct)) => {
                    trace_download("页内回放拉取成功");
                    return finish(
                        session,
                        &mut suggested,
                        url,
                        client.port,
                        bytes,
                        Some(cd),
                        Some(ct),
                        step_index,
                    );
                }
                Err(e) => {
                    trace_download(format!("页内回放失败: {e}"));
                }
            }
            match fetch_via_reqwest_with_replay(client, url, r).await {
                Ok((bytes, headers)) if response_looks_like_file(&headers, &bytes) => {
                    let cd = headers
                        .get(CONTENT_DISPOSITION)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    let ct = headers
                        .get(CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    // reqwest 头里的 CD 优先；非 UTF-8 时 to_str 失败则靠 CDP 建议名
                    if cd.is_none() {
                        if let Some(raw) = headers.get(CONTENT_DISPOSITION) {
                            if let Ok(name) = try_filename_from_header_value(raw) {
                                suggested = Some(name);
                            }
                        }
                    }
                    trace_download("HTTP 客户端回放拉取成功");
                    return finish(
                        session,
                        &mut suggested,
                        url,
                        client.port,
                        bytes,
                        cd,
                        ct,
                        step_index,
                    );
                }
                Ok(_) => {
                    trace_download("HTTP 客户端回放成功但响应不像文件，继续兜底");
                }
                Err(e) => {
                    trace_download(format!("HTTP 客户端回放失败: {e}"));
                }
            }
        }
    }

    // 优先在客户端进程拉取，避免 Chrome 渲染进程整文件 base64 导致 OOM。
    // 页面流式 fetch 仅作 Cookie/CORS 兜底。
    match fetch_via_reqwest(client, url).await {
        Ok((bytes, headers)) if response_looks_like_file(&headers, &bytes) => {
            let cd = headers
                .get(CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let ct = headers
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            if cd.is_none() {
                if let Some(raw) = headers.get(CONTENT_DISPOSITION) {
                    if let Ok(name) = try_filename_from_header_value(raw) {
                        suggested = Some(name);
                    }
                }
            }
            return finish(
                session,
                &mut suggested,
                url,
                client.port,
                bytes,
                cd,
                ct,
                step_index,
            );
        }
        Ok(_) => {
            // HTTP 200 但像登录页/HTML，改走页面流式
        }
        Err(_) => {}
    }

    let page_headers = replay
        .as_ref()
        .map(|r| r.headers.clone())
        .unwrap_or_default();
    match fetch_via_page_with_headers(client, url, &page_headers).await {
        Ok((bytes, cd, ct)) => finish(
            session,
            &mut suggested,
            url,
            client.port,
            bytes,
            Some(cd),
            Some(ct),
            step_index,
        ),
        Err(page_err) => match fetch_via_reqwest(client, url).await {
            Ok((bytes, headers)) => {
                let cd = headers
                    .get(CONTENT_DISPOSITION)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let ct = headers
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                if cd.is_none() {
                    if let Some(raw) = headers.get(CONTENT_DISPOSITION) {
                        if let Ok(name) = try_filename_from_header_value(raw) {
                            suggested = Some(name);
                        }
                    }
                }
                finish(
                    session,
                    &mut suggested,
                    url,
                    client.port,
                    bytes,
                    cd,
                    ct,
                    step_index,
                )
            }
            Err(req_err) => Err(format!("页面 fetch: {page_err}; HTTP 客户端: {req_err}")),
        },
    }
}

/// Content-Disposition 含非 UTF-8 字节时 HeaderValue::to_str 会失败；按 latin1 再解析一次
fn try_filename_from_header_value(val: &reqwest::header::HeaderValue) -> Result<String, ()> {
    if let Ok(s) = val.to_str() {
        return filename_from_content_disposition_str(s).ok_or(());
    }
    let raw = val
        .as_bytes()
        .iter()
        .map(|&b| b as char)
        .collect::<String>();
    filename_from_content_disposition_str(&raw).ok_or(())
}

fn replay_needs_full_request(r: &crate::browser_network::DownloadReplayRequest) -> bool {
    let m = r.method.to_ascii_uppercase();
    if m == "POST" || m == "PUT" || m == "PATCH" {
        return true;
    }
    if r.post_data.as_ref().is_some_and(|b| !b.is_empty()) {
        return true;
    }
    r.headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("authorization") || k.eq_ignore_ascii_case("token"))
}

fn is_feishu_related_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains("feishu.cn")
        || u.contains("larksuite.com")
        || u.contains("feishu.net")
        || u.contains("larksuitecdn.com")
        || u.contains("feishucdn.com")
}

/// 飞书下载：1) Open API + 应用 token  2) 页内 credentials + 原请求回放  3) 补全域 Cookie 的 reqwest
async fn fetch_feishu_to_bytes(
    client: &CdpClient,
    url: &str,
    replay: Option<&crate::browser_network::DownloadReplayRequest>,
) -> Result<(Vec<u8>, Option<String>, String, String), String> {
    if let Some(file_token) = crate::feishu::parse_open_api_file_token(url) {
        match crate::feishu::download_file_by_token(&file_token).await {
            Ok((bytes, name)) => {
                trace_download(format!(
                    "飞书 Open API 下载成功 | file_token={file_token} | {name}"
                ));
                return Ok((bytes, Some(name), String::new(), String::new()));
            }
            Err(e) => {
                trace_download(format!(
                    "飞书 Open API 下载失败（将改用浏览器会话）: {e}"
                ));
            }
        }
    }

    let owned;
    let replay_ref = if let Some(r) = replay {
        r
    } else {
        owned = crate::browser_network::BrowserNetworkCapture::shared_for_port(client.port)
            .await
            .ok()
            .and_then(|cap| cap.take_download_replay(url))
            .unwrap_or_default();
        &owned
    };
    let replay_headers = &replay_ref.headers;
    if !replay_headers.is_empty() || replay_ref.post_data.is_some() {
        trace_download(format!(
            "飞书回放 | {} | headers={} | body={}",
            replay_ref.method,
            replay_headers.len(),
            replay_ref.post_data.as_ref().map(|b| b.len()).unwrap_or(0)
        ));
    }

    if replay_needs_full_request(replay_ref) {
        match fetch_via_page_with_replay(client, url, replay_ref).await {
            Ok((bytes, cd, ct)) => {
                trace_download("飞书页内回放成功（含登录 Cookie）");
                return Ok((bytes, None, cd, ct));
            }
            Err(e) => {
                trace_download(format!("飞书页内回放失败: {e}"));
            }
        }
    }

    match fetch_via_page_with_headers(client, url, replay_headers).await {
        Ok((bytes, cd, ct)) => {
            trace_download("飞书页内 fetch 成功（含登录 Cookie）");
            return Ok((bytes, None, cd, ct));
        }
        Err(e) => {
            trace_download(format!("飞书页内 fetch 失败: {e}"));
        }
    }

    let (bytes, headers) = if replay_needs_full_request(replay_ref) {
        fetch_via_reqwest_with_replay(client, url, replay_ref).await?
    } else {
        fetch_via_reqwest_feishu(client, url, replay_headers).await?
    };
    if !response_looks_like_file(&headers, &bytes) {
        return Err("飞书 HTTP 响应不像文件（可能仍是登录页）".into());
    }
    let cd = headers
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let ct = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    Ok((bytes, None, cd, ct))
}

async fn fetch_via_reqwest_feishu(
    client: &CdpClient,
    url: &str,
    replay_headers: &std::collections::HashMap<String, String>,
) -> Result<(Vec<u8>, reqwest::header::HeaderMap), String> {
    let cookie_header = build_feishu_cookie_header(client, url).await?;
    let referer = if client.target_url.is_empty() {
        origin_of(url).unwrap_or_default()
    } else {
        client.target_url.clone()
    };

    let mut req = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(Duration::from_secs(120))
        .cookie_store(true)
        .build()
        .map_err(|e| e.to_string())?
        .get(url);

    if !cookie_header.is_empty() {
        req = req.header("Cookie", cookie_header);
    }
    // 先挂原请求头（鉴权/CSRF），再补 Referer/UA，避免被空值覆盖
    for (k, v) in replay_headers {
        if k.eq_ignore_ascii_case("cookie")
            || k.eq_ignore_ascii_case("host")
            || k.eq_ignore_ascii_case("content-length")
        {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            req = req.header(name, val);
        }
    }
    if !referer.is_empty() && !replay_headers.keys().any(|k| k.eq_ignore_ascii_case("referer")) {
        req = req.header("Referer", &referer);
        if let Some(origin) = origin_of(&referer) {
            if !replay_headers.keys().any(|k| k.eq_ignore_ascii_case("origin")) {
                req = req.header("Origin", origin);
            }
        }
    }
    if !replay_headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("user-agent"))
    {
        req = req.header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        );
    }
    if !replay_headers.keys().any(|k| k.eq_ignore_ascii_case("accept")) {
        req = req.header("Accept", "*/*");
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("飞书 HTTP 请求失败: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_hint = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect::<String>();
        return Err(format!(
            "飞书 HTTP {status}（缺浏览器用户会话时常为 401）: {body_hint}"
        ));
    }

    let headers = resp.headers().clone();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取飞书响应失败: {e}"))?;
    if bytes.is_empty() {
        return Err("飞书响应体为空".into());
    }
    Ok((bytes.to_vec(), headers))
}

/// 飞书多子域会话：用 getAllCookies 补齐 .feishu.cn / accounts 等，避免只带下载 URL 域导致 401
async fn build_feishu_cookie_header(client: &CdpClient, url: &str) -> Result<String, String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();

    let push_cookies = |arr: &[serde_json::Value], pairs: &mut Vec<(String, String)>, seen: &mut HashSet<String>| {
        for c in arr {
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let value = c.get("value").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let domain = c
                .get("domain")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let feishu_cookie = domain.is_empty()
                || domain.contains("feishu")
                || domain.contains("lark");
            if !feishu_cookie {
                continue;
            }
            let key = format!("{name}@{domain}");
            if seen.insert(key) {
                pairs.push((name.to_string(), value.to_string()));
            }
        }
    };

    if let Ok(v) = client
        .call_result("Network.getAllCookies", json!({}))
        .await
    {
        if let Some(arr) = v.get("cookies").and_then(|x| x.as_array()) {
            push_cookies(arr, &mut pairs, &mut seen);
        }
    }

    // 再按目标 URL 拉一轮，覆盖分区 Cookie
    let origin = origin_of(url).unwrap_or_else(|| url.to_string());
    let page = if client.target_url.is_empty() {
        origin.clone()
    } else {
        client.target_url.clone()
    };
    if let Ok(v) = client
        .call_result(
            "Network.getCookies",
            json!({ "urls": [url, origin, page] }),
        )
        .await
    {
        if let Some(arr) = v.get("cookies").and_then(|x| x.as_array()) {
            push_cookies(arr, &mut pairs, &mut seen);
        }
    }

    if pairs.is_empty() {
        return build_cookie_header(client, url).await;
    }
    Ok(pairs
        .into_iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// WPS 插件包、页面图片等：自动捕获阶段应丢弃，避免污染 downloads/。
fn is_junk_download_payload(bytes: &[u8], filename: &str) -> bool {
    if bytes.starts_with(b"WPSWFM") {
        return true;
    }
    let n = filename.to_ascii_lowercase();
    if n.ends_with(".png")
        || n.ends_with(".jpg")
        || n.ends_with(".jpeg")
        || n.ends_with(".gif")
        || n.ends_with(".webp")
        || n.ends_with(".svg")
        || n.ends_with(".ico")
        || n.ends_with(".woff")
        || n.ends_with(".woff2")
        || n.ends_with(".ttf")
    {
        return true;
    }
    // 无名 .bin 且不像 Office/ZIP 魔数
    if n.ends_with(".bin") || n.starts_with("download_") {
        let officeish = bytes.starts_with(b"PK") // zip/xlsx/docx
            || bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) // ole xls/doc
            || bytes.starts_with(b"%PDF");
        if !officeish {
            return true;
        }
    }
    false
}

fn response_looks_like_file(headers: &reqwest::header::HeaderMap, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let cd = headers
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if cd.contains("attachment") || cd.contains("filename") {
        return true;
    }
    let ct = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ct.contains("text/html") || ct.contains("application/xhtml") {
        return false;
    }
    let head = &bytes[..bytes.len().min(64)];
    let as_ascii = String::from_utf8_lossy(head).to_ascii_lowercase();
    if as_ascii.contains("<!doctype html") || as_ascii.contains("<html") {
        return false;
    }
    true
}

/// 在已登录页面内流式 fetch（不把整文件塞进一次 CDP 返回值，避免 OOM）。
/// 优先在 URL 同源 / 飞书同站的 OOPIF 内拉取，避免父页跨域 CORS。
async fn fetch_via_page(client: &CdpClient, url: &str) -> Result<(Vec<u8>, String, String), String> {
    fetch_via_page_with_headers(client, url, &std::collections::HashMap::new()).await
}

async fn fetch_via_page_with_headers(
    client: &CdpClient,
    url: &str,
    replay_headers: &std::collections::HashMap<String, String>,
) -> Result<(Vec<u8>, String, String), String> {
    let replay = crate::browser_network::DownloadReplayRequest {
        method: "GET".into(),
        headers: replay_headers.clone(),
        post_data: None,
    };
    fetch_via_page_with_replay(client, url, &replay).await
}

async fn fetch_via_page_with_replay(
    client: &CdpClient,
    url: &str,
    replay: &crate::browser_network::DownloadReplayRequest,
) -> Result<(Vec<u8>, String, String), String> {
    let mut last_err = String::new();
    let url_host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()));
    if let Ok(iframes) =
        crate::chrome::list_iframe_targets_for_parent_on(client.port, &client.target_id).await
    {
        for iframe in iframes {
            let iframe_host = Url::parse(&iframe.url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()));
            let same_host = matches!((&url_host, &iframe_host), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b));
            let same_feishu_site = matches!(
                (&url_host, &iframe_host),
                (Some(a), Some(b)) if feishu_same_site(a, b)
            );
            if !same_host && !same_feishu_site {
                continue;
            }
            let Ok(child) = CdpClient::connect_to_any_target_on(client.port, &iframe.id).await
            else {
                continue;
            };
            match fetch_via_page_stream_on(&child, url, replay).await {
                Ok(v) => return Ok(v),
                Err(e) => last_err = e,
            }
        }
    }
    match fetch_via_page_stream_on(client, url, replay).await {
        Ok(v) => Ok(v),
        Err(e) => Err(if last_err.is_empty() {
            e
        } else {
            format!("{last_err}; {e}")
        }),
    }
}

fn feishu_same_site(a: &str, b: &str) -> bool {
    fn root(host: &str) -> Option<&'static str> {
        let h = host.trim_start_matches('.').to_ascii_lowercase();
        if h == "feishu.cn" || h.ends_with(".feishu.cn") {
            Some("feishu.cn")
        } else if h == "larksuite.com" || h.ends_with(".larksuite.com") {
            Some("larksuite.com")
        } else if h == "feishu.net" || h.ends_with(".feishu.net") {
            Some("feishu.net")
        } else {
            None
        }
    }
    matches!((root(a), root(b)), (Some(x), Some(y)) if x == y)
}

fn btoa_chunk_js() -> &'static str {
    r#"function __webclawB64(u8) {
  var bin = '';
  var step = 0x8000;
  for (var i = 0; i < u8.length; i += step) {
    bin += String.fromCharCode.apply(null, u8.subarray(i, i + step));
  }
  return btoa(bin);
}"#
}

/// 流式：页面只保留 ReadableStream reader，按块 base64 回传；峰值约一块网络缓冲。
/// 支持回放原 method / headers / body（POST /dev-api/…/export + Bearer）。
async fn fetch_via_page_stream_on(
    client: &CdpClient,
    url: &str,
    replay: &crate::browser_network::DownloadReplayRequest,
) -> Result<(Vec<u8>, String, String), String> {
    let headers_json = serde_json::to_string(&replay.headers).unwrap_or_else(|_| "{}".into());
    let method = if replay.method.trim().is_empty() {
        "GET".to_string()
    } else {
        replay.method.to_ascii_uppercase()
    };
    let body_json = serde_json::to_string(&replay.post_data).unwrap_or_else(|_| "null".into());
    let start = format!(
        r#"(async () => {{
  {btoa_fn}
  const target = {url};
  const replay = {headers};
  const method = {method};
  const body = {body};
  window.__webclawDlJobs = window.__webclawDlJobs || {{}};
  function cookieVal(name) {{
    try {{
      const m = document.cookie.match(new RegExp('(?:^|; )' + name.replace(/[$()*+.?[\\\]^{{}}|]/g, '\\$&') + '=([^;]*)'));
      return m ? decodeURIComponent(m[1]) : '';
    }} catch (e) {{ return ''; }}
  }}
  const hdr = Object.assign({{}}, replay || {{}});
  delete hdr.cookie; delete hdr.Cookie; delete hdr.host; delete hdr.Host;
  // 飞书审批后台等：无 CSRF 头时常 401
  const csrf = cookieVal('_csrf_token') || cookieVal('csrf_token') || cookieVal('landing_csrf_token')
    || cookieVal('xt-csrf-token') || cookieVal('X-CSRF-Token');
  if (csrf) {{
    if (!hdr['x-csrf-token'] && !hdr['X-CSRF-Token'] && !hdr['x-csrftoken']) {{
      hdr['X-CSRF-Token'] = csrf;
      hdr['x-csrf-token'] = csrf;
    }}
  }}
  if (!hdr['X-Requested-With'] && !hdr['x-requested-with']) {{
    hdr['X-Requested-With'] = 'XMLHttpRequest';
  }}
  if (!hdr['Referer'] && !hdr['referer']) {{
    try {{ hdr['Referer'] = location.href; }} catch (e) {{}}
  }}
  if (!hdr['Origin'] && !hdr['origin']) {{
    try {{ hdr['Origin'] = location.origin; }} catch (e) {{}}
  }}
  if (!hdr['Accept'] && !hdr['accept']) hdr['Accept'] = '*/*';
  const init = {{ method: method || 'GET', credentials: 'include', redirect: 'follow', headers: hdr }};
  if (body != null && body !== '' && method && method.toUpperCase() !== 'GET' && method.toUpperCase() !== 'HEAD') {{
    init.body = body;
  }}
  const resp = await fetch(target, init);
  const cd = resp.headers.get('content-disposition') || '';
  const ct = resp.headers.get('content-type') || '';
  if (!resp.ok) {{
    return {{ ok: false, status: resp.status, cd, ct }};
  }}
  if (!resp.body || !resp.body.getReader) {{
    // 极旧环境兜底：仍避免整包 btoa，按块切 Uint8Array
    const buf = await resp.arrayBuffer();
    const bytes = new Uint8Array(buf);
    const id = 'buf_' + Date.now() + '_' + Math.random().toString(36).slice(2);
    window.__webclawDlJobs[id] = {{ kind: 'buf', bytes: bytes, offset: 0, cd: cd, ct: ct }};
    return {{ ok: true, jobId: id, cd: cd, ct: ct, size: bytes.length, mode: 'buf' }};
  }}
  const id = 'stream_' + Date.now() + '_' + Math.random().toString(36).slice(2);
  window.__webclawDlJobs[id] = {{ kind: 'stream', reader: resp.body.getReader(), cd: cd, ct: ct }};
  return {{ ok: true, jobId: id, cd: cd, ct: ct, size: -1, mode: 'stream' }};
}})()"#,
        btoa_fn = btoa_chunk_js(),
        url = serde_json::json!(url),
        headers = headers_json,
        method = serde_json::json!(method),
        body = body_json
    );
    let v = client.evaluate_download(&start).await?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        let status = v.get("status").and_then(|x| x.as_u64()).unwrap_or(0);
        return Err(format!("HTTP {status}"));
    }
    let job_id = v
        .get("jobId")
        .and_then(|x| x.as_str())
        .ok_or("页面流式下载未返回 jobId")?
        .to_string();
    let cd = v
        .get("cd")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let ct = v
        .get("ct")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("stream");

    let mut out: Vec<u8> = Vec::new();
    if let Some(sz) = v.get("size").and_then(|x| x.as_u64()) {
        if sz > 0 && sz < 512 * 1024 * 1024 {
            out.reserve(sz as usize);
        }
    }

    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    const MAX_BYTES: usize = 512 * 1024 * 1024; // 512MB 硬顶，防止异常撑爆客户端

    let result = async {
        loop {
            let step = if mode == "buf" {
                format!(
                    r#"(function () {{
  {btoa_fn}
  var jobs = window.__webclawDlJobs || {{}};
  var job = jobs[{id}];
  if (!job || job.kind !== 'buf') return {{ ok: false, error: 'job missing' }};
  var chunk = 262144;
  if (job.offset >= job.bytes.length) {{
    delete jobs[{id}];
    return {{ ok: true, done: true, b64: '' }};
  }}
  var end = Math.min(job.offset + chunk, job.bytes.length);
  var slice = job.bytes.subarray(job.offset, end);
  job.offset = end;
  var b64 = __webclawB64(slice);
  var done = job.offset >= job.bytes.length;
  if (done) delete jobs[{id}];
  return {{ ok: true, done: done, b64: b64 }};
}})()"#,
                    btoa_fn = btoa_chunk_js(),
                    id = serde_json::json!(job_id)
                )
            } else {
                format!(
                    r#"(async () => {{
  {btoa_fn}
  var jobs = window.__webclawDlJobs || {{}};
  var job = jobs[{id}];
  if (!job || job.kind !== 'stream') return {{ ok: false, error: 'job missing' }};
  var read = await job.reader.read();
  if (read.done) {{
    try {{ job.reader.releaseLock(); }} catch (e) {{}}
    delete jobs[{id}];
    return {{ ok: true, done: true, b64: '' }};
  }}
  var u8 = read.value instanceof Uint8Array ? read.value : new Uint8Array(read.value);
  return {{ ok: true, done: false, b64: __webclawB64(u8) }};
}})()"#,
                    btoa_fn = btoa_chunk_js(),
                    id = serde_json::json!(job_id)
                )
            };
            let chunk_v = client.evaluate_download(&step).await?;
            if chunk_v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
                let err = chunk_v
                    .get("error")
                    .and_then(|x| x.as_str())
                    .unwrap_or("分块读取失败");
                return Err(err.to_string());
            }
            if let Some(b64) = chunk_v.get("b64").and_then(|x| x.as_str()) {
                if !b64.is_empty() {
                    let part = engine
                        .decode(b64)
                        .map_err(|e| format!("分块解码失败: {e}"))?;
                    if out.len().saturating_add(part.len()) > MAX_BYTES {
                        return Err(format!("下载超过上限 {} MB", MAX_BYTES / (1024 * 1024)));
                    }
                    out.extend_from_slice(&part);
                }
            }
            if chunk_v.get("done").and_then(|x| x.as_bool()) == Some(true) {
                break;
            }
        }
        Ok(())
    }
    .await;

    if result.is_err() {
        let cleanup = format!(
            r#"(function () {{
  var jobs = window.__webclawDlJobs || {{}};
  var job = jobs[{id}];
  if (job && job.reader) {{ try {{ job.reader.cancel(); }} catch (e) {{}} }}
  delete jobs[{id}];
}})()"#,
            id = serde_json::json!(job_id)
        );
        let _ = client.evaluate(&cleanup).await;
    }
    result?;

    if out.is_empty() {
        return Err("响应体为空".into());
    }
    Ok((out, cd, ct))
}

async fn fetch_via_reqwest(
    client: &CdpClient,
    url: &str,
) -> Result<(Vec<u8>, reqwest::header::HeaderMap), String> {
    let replay = crate::browser_network::DownloadReplayRequest {
        method: "GET".into(),
        headers: Default::default(),
        post_data: None,
    };
    fetch_via_reqwest_with_replay(client, url, &replay).await
}

async fn fetch_via_reqwest_with_replay(
    client: &CdpClient,
    url: &str,
    replay: &crate::browser_network::DownloadReplayRequest,
) -> Result<(Vec<u8>, reqwest::header::HeaderMap), String> {
    let cookie_header = build_cookie_header(client, url).await?;
    let referer = if client.target_url.is_empty() {
        origin_of(url).unwrap_or_default()
    } else {
        client.target_url.clone()
    };

    let method = if replay.method.trim().is_empty() {
        "GET".to_string()
    } else {
        replay.method.to_ascii_uppercase()
    };

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let mut builder = match method.as_str() {
        "POST" => http.post(url),
        "PUT" => http.put(url),
        "PATCH" => http.patch(url),
        "HEAD" => http.head(url),
        _ => http.get(url),
    };

    // 先铺回放头（含 Authorization），再补 Cookie / Referer（不覆盖已有）
    for (k, v) in &replay.headers {
        if k.eq_ignore_ascii_case("cookie")
            || k.eq_ignore_ascii_case("host")
            || k.eq_ignore_ascii_case("content-length")
        {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            builder = builder.header(name, val);
        }
    }
    if !cookie_header.is_empty()
        && !replay
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("cookie"))
    {
        builder = builder.header("Cookie", cookie_header);
    }
    if !referer.is_empty()
        && !replay
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("referer"))
    {
        builder = builder.header("Referer", &referer);
    }
    if !replay
        .headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("user-agent"))
    {
        builder = builder.header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        );
    }
    if !replay
        .headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("accept"))
    {
        builder = builder.header("Accept", "*/*");
    }
    if let Some(body) = &replay.post_data {
        if method != "GET" && method != "HEAD" {
            builder = builder.body(body.clone());
        }
    }

    let resp = builder
        .send()
        .await
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }

    let headers = resp.headers().clone();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取响应体失败: {e}"))?;

    if bytes.is_empty() {
        return Err("响应体为空".into());
    }

    Ok((bytes.to_vec(), headers))
}

async fn build_cookie_header(client: &CdpClient, url: &str) -> Result<String, String> {
    let origin = origin_of(url).unwrap_or_else(|| url.to_string());
    let v = client
        .call_result(
            "Network.getCookies",
            json!({ "urls": [url, origin] }),
        )
        .await?;
    let cookies = v.get("cookies").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    Ok(cookies
        .iter()
        .filter_map(|c| {
            let name = c.get("name")?.as_str()?;
            let value = c.get("value")?.as_str()?;
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; "))
}

fn decode_filename_component(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = bytes[i + 1] as char;
            let lo = bytes[i + 2] as char;
            if let (Some(h), Some(l)) = (hi.to_digit(16), lo.to_digit(16)) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn filename_from_download_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    for (k, v) in parsed.query_pairs() {
        let key = k.to_ascii_lowercase();
        if key == "file_name" || key == "filename" || key == "name" || key == "file" {
            let name = decode_filename_component(v.trim());
            if !name.is_empty() && !is_generic_download_basename(&name) {
                return Some(sanitize_download_filename(&name));
            }
        }
    }
    let last = parsed
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|p| !p.is_empty() && *p != "common_excel")
        .map(|s| decode_filename_component(s))?;
    let last_l = last.to_ascii_lowercase();
    // 旧 MES exportbypage：无文件名参数，用 date 拼可读名
    if last_l == "exportbypage" || last_l.starts_with("exportby") {
        let date = parsed
            .query_pairs()
            .find(|(k, _)| k.eq_ignore_ascii_case("date"))
            .map(|(_, v)| v.trim().to_string())
            .filter(|s| !s.is_empty());
        let name = match date {
            Some(d) => format!("考勤月报_{d}.xls"),
            None => format!("{last}.xls"),
        };
        return Some(sanitize_download_filename(&name));
    }
    // /export、/download 等是接口动作名，不是文件名
    if is_generic_download_basename(&last) {
        return None;
    }
    Some(sanitize_download_filename(&last))
}

/// API 路径末段或占位名：不能当真实下载文件名
fn is_generic_download_basename(name: &str) -> bool {
    let n = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_lowercase();
    let stem = n
        .rsplit_once('.')
        .filter(|(_, ext)| {
            matches!(
                *ext,
                "xlsx"
                    | "xls"
                    | "csv"
                    | "pdf"
                    | "zip"
                    | "doc"
                    | "docx"
                    | "xlsm"
                    | "bin"
            )
        })
        .map(|(s, _)| s)
        .unwrap_or(&n);
    matches!(
        stem,
        "export"
            | "download"
            | "downfile"
            | "downloadfile"
            | "filedownload"
            | "file"
            | "api"
            | "index"
            | "common_excel"
            | "download_bin"
            | "download.bin"
    ) || n == "export"
        || n == "download"
}

/// 合成最终落盘名：CD → CDP 建议名 → URL → 类型/魔数兜底；无后缀时补扩展名
fn resolve_download_filename(
    cd: Option<&str>,
    suggested: Option<&str>,
    url: &str,
    content_type: Option<&str>,
    bytes: &[u8],
    client_port: Option<u16>,
) -> String {
    let from_cd = cd.and_then(filename_from_content_disposition_str);
    let from_suggested = suggested
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "download.bin")
        .filter(|s| !is_generic_download_basename(s))
        .map(sanitize_download_filename);
    // 页内 fetch 常因 CORS 读不到 CD：再窥探 CDP 已记下的建议名
    let from_peek = if from_cd.is_none() && from_suggested.is_none() {
        client_port.and_then(|port| {
            crate::browser_network::capture_for_port(port)
                .and_then(|cap| cap.peek_suggested_filename(url))
                .filter(|s| !is_generic_download_basename(s))
                .map(|s| sanitize_download_filename(&s))
        })
    } else {
        None
    };
    let from_url = filename_from_download_url(url);
    let mut name = from_cd
        .or(from_suggested)
        .or(from_peek)
        .or(from_url)
        .unwrap_or_else(|| {
            fallback_filename_from_ct(content_type.unwrap_or(""))
        });
    if Path::new(&name).extension().is_none() {
        let ext = sniff_download_extension(bytes, content_type.unwrap_or(""));
        if is_generic_download_basename(&name) {
            name = format!(
                "download_{}.{}",
                chrono::Local::now().format("%Y%m%d_%H%M%S"),
                ext
            );
        } else {
            name = format!("{name}.{ext}");
        }
    }
    name
}

fn sniff_download_extension(bytes: &[u8], ct: &str) -> &'static str {
    if bytes.starts_with(b"PK") {
        return "xlsx";
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        return "xls";
    }
    if bytes.starts_with(b"%PDF") {
        return "pdf";
    }
    let c = ct.to_ascii_lowercase();
    if c.contains("spreadsheet") || c.contains("excel") || c.contains("sheet") {
        "xlsx"
    } else if c.contains("csv") {
        "csv"
    } else if c.contains("pdf") {
        "pdf"
    } else if c.contains("zip") {
        "zip"
    } else if c.contains("ms-excel") || c.contains("xls") {
        "xls"
    } else {
        "bin"
    }
}

fn filename_from_content_disposition_str(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let mut utf8_name: Option<String> = None;
    let mut plain_name: Option<String> = None;
    for token in raw.split(';') {
        let t = token.trim();
        let lower = t.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix("filename*=")
            .map(|_| &t["filename*=".len()..])
            .or_else(|| {
                // filename*= 大小写混用
                let idx = lower.find("filename*=")?;
                Some(&t[idx + "filename*=".len()..])
            })
        {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            let encoded = v
                .strip_prefix("UTF-8''")
                .or_else(|| v.strip_prefix("utf-8''"))
                .unwrap_or(v);
            let name = decode_filename_component(encoded);
            if !name.is_empty() {
                utf8_name = Some(name);
            }
        } else if lower.starts_with("filename=") || lower.starts_with("filename =") {
            let v = t
                .split_once('=')
                .map(|(_, v)| v.trim().trim_matches('"').trim_matches('\''))
                .unwrap_or("");
            let name = decode_filename_component(v);
            if !name.is_empty() {
                plain_name = Some(name);
            }
        }
    }
    utf8_name
        .or(plain_name)
        .filter(|n| !is_generic_download_basename(n))
        .map(|n| sanitize_download_filename(&n))
}

fn fallback_filename_from_ct(ct: &str) -> String {
    let ext = if ct.contains("spreadsheet") || ct.contains("excel") {
        "xlsx"
    } else if ct.contains("csv") {
        "csv"
    } else if ct.contains("pdf") {
        "pdf"
    } else if ct.contains("zip") {
        "zip"
    } else {
        "bin"
    };
    format!(
        "download_{}.{}",
        chrono::Local::now().format("%Y%m%d_%H%M%S"),
        ext
    )
}

fn filename_from_content_disposition(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let raw = headers.get(CONTENT_DISPOSITION)?.to_str().ok()?;
    filename_from_content_disposition_str(raw)
}

fn fallback_filename(headers: &reqwest::header::HeaderMap) -> String {
    let ext = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            if ct.contains("spreadsheet") || ct.contains("excel") {
                "xlsx"
            } else if ct.contains("csv") {
                "csv"
            } else if ct.contains("pdf") {
                "pdf"
            } else if ct.contains("zip") {
                "zip"
            } else {
                "bin"
            }
        })
        .unwrap_or("bin");
    format!(
        "download_{}.{}",
        chrono::Local::now().format("%Y%m%d_%H%M%S"),
        ext
    )
}

fn sanitize_download_filename(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    base.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c > '\u{007f}' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn origin_of(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let scheme = parsed.scheme();
    Some(format!("{scheme}://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hcm_file_name_query() {
        let url = "https://cleanpro.hcmcloud.cn/document/exp/common_excel?file_name=%E8%80%83%E5%8B%A4%E6%9C%88%E6%8A%A5_test.xlsx&category=x";
        let name = filename_from_download_url(url).unwrap();
        assert!(name.contains("考勤月报"));
        assert!(name.ends_with(".xlsx"));
    }

    #[test]
    fn decodes_percent_encoded_filename() {
        let raw = "%E4%BC%91%E5%81%87%E5%8F%B0%E8%B4%A6_20260610155425.xlsx";
        let name = decode_filename_component(raw);
        assert!(name.contains("休假台账"));
        assert!(name.ends_with(".xlsx"));
    }

    #[test]
    fn rejects_chrome_native_print_preview_urls() {
        assert!(!looks_like_download_url(
            "chrome-untrusted://print/6EF118328BE13764E4AE20D30C775851/0/print.pdf"
        ));
        assert!(!looks_like_download_url(
            "chrome://print/pdf/index_print.html?chrome-untrusted://print/abc/0/print.pdf"
        ));
        assert!(is_browser_internal_url("chrome://print/"));
    }

    #[test]
    fn detects_hcm_and_ext_urls() {
        assert!(looks_like_download_url(
            "https://cleanpro.hcmcloud.cn/document/exp/common_excel?file_name=a.xlsx"
        ));
        assert!(looks_like_download_url("http://127.0.0.1:9/report.xlsx"));
        assert!(!looks_like_download_url("http://127.0.0.1:9/f.bin"));
        assert!(!looks_like_download_url("https://example.com/api/list"));
    }

    #[test]
    fn rejects_weaver_portal_filedownload() {
        assert!(!looks_like_download_url(
            "https://example.com/"
        ));
    }

    #[test]
    fn rejects_wps_junk_payload() {
        assert!(is_junk_download_payload(b"WPSWFM10....", "12.bin"));
        assert!(is_junk_download_payload(b"\x89PNG....", "850X850.PNG"));
        assert!(!is_junk_download_payload(b"PK\x03\x04....", "a.xlsx"));
    }

    #[test]
    fn detects_ruoyi_style_post_export_url() {
        assert!(looks_like_download_url(
            "https://example.com/"
        ));
        assert!(replay_needs_full_request(
            &crate::browser_network::DownloadReplayRequest {
                method: "POST".into(),
                headers: std::collections::HashMap::from([(
                    "authorization".into(),
                    "Bearer x".into()
                )]),
                post_data: Some("{}".into()),
            }
        ));
        // 路径末段 export 不能当文件名
        assert!(filename_from_download_url(
            "https://example.com/"
        )
        .is_none());
        let name = resolve_download_filename(
            Some("attachment;filename=%E6%B2%B9%E6%96%99%E5%AF%BC%E5%87%BA.xlsx"),
            None,
            "https://example.com/",
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            b"PK\x03\x04",
            None,
        );
        assert!(name.contains("xlsx"), "{name}");
        assert!(!name.eq_ignore_ascii_case("export"), "{name}");
    }

    #[test]
    fn detects_mes_exportbypage_url() {
        let url = "http://39.106.23.248:18999/cloud/staff/attendance/monthlyreport/exportbypage?date=2026-06&companyIds=e7ede50d870b4af0b0c63f4d79a7a5f2&pageIndex=1&pageSize=10&downloadAll=true&isEHR=false&tenantId=64a121438d1e440ba9a0bbde10439c25&userId=85efba87cabc48bbb1695da63a49b998";
        assert!(looks_like_download_url(url));
    }

    #[test]
    fn detects_feishu_hosts() {
        assert!(is_feishu_related_url(
            "https://pcnzwk7m6zlv.feishu.cn/drive/folder/abc"
        ));
        assert!(is_feishu_related_url(
            "https://open.feishu.cn/open-apis/drive/v1/files/boxcnX/download"
        ));
        assert!(is_feishu_related_url(
            "https://www.feishu.cn/approval/admin/api/export/download?locale=zh_cn&exportId=1"
        ));
        assert!(looks_like_download_url(
            "https://www.feishu.cn/approval/admin/api/export/download?locale=zh_cn&exportId=1"
        ));
        assert!(!is_feishu_related_url("https://cleanpro.hcmcloud.cn/a.xlsx"));
        assert!(feishu_same_site("www.feishu.cn", "accounts.feishu.cn"));
        assert!(!feishu_same_site("www.feishu.cn", "example.com"));
    }

    #[test]
    fn rejects_feishu_export_json_apis() {
        assert!(!looks_like_download_url(
            "https://www.feishu.cn/approval/admin/api/export/queryByIds?locale=zh_cn"
        ));
        assert!(!looks_like_download_url(
            "https://www.feishu.cn/approval/admin/api/export/queryList?locale=zh_cn"
        ));
        assert!(!looks_like_download_url(
            "https://www.feishu.cn/approval/admin/api/v2/export/batchExport?locale=zh_cn"
        ));
        assert!(looks_like_download_url(
            "https://www.feishu.cn/approval/admin/api/export/download?locale=zh_cn&exportId=7677468293673831612"
        ));
    }
}

