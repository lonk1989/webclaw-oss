//! 方案 A：自动化期间拦截 `window.print()`，用 CDP `Page.printToPDF` 静默出 PDF，
//! 写入运行产物 `downloads/`，与现有「等待下载」对齐。

use crate::cdp::CdpClient;
use crate::run_artifacts::{RunArtifact, RunSession};
use base64::Engine;
use serde_json::json;
use std::time::Duration;

/// 仅在 `__webclawClientDownload===true`（自动化捕获开）时拦截；空闲放行原生打印。
/// 用 `__webclawPrintWrapped === window.print` 判断钩子是否仍在；页面覆盖 `print` 后可重装，
/// 避免旧逻辑 `if (__webclawPrintHook) return` 导致标志仍在、拦截却已失效。
pub const PRINT_HOOK_SCRIPT: &str = r#"
(function () {
  window.__webclawPendingPrints = window.__webclawPendingPrints || [];
  function captureEnabled() {
    try { return window.__webclawClientDownload === true; } catch (e) { return false; }
  }
  function enqueuePrint() {
    var q = window.__webclawPendingPrints;
    q.push({
      ts: Date.now(),
      title: (document.title || '').trim(),
      url: location.href || ''
    });
    try {
      window.dispatchEvent(new Event('beforeprint'));
      window.dispatchEvent(new Event('afterprint'));
    } catch (e) {}
  }
  function hookedPrint() {
    if (captureEnabled()) {
      enqueuePrint();
      return;
    }
    var orig = window.__webclawOrigPrint;
    if (typeof orig === 'function') return orig();
  }
  // 钩子仍挂在 window.print 上则跳过；被页面覆盖后必须重装
  if (window.__webclawPrintWrapped === window.print) return;
  try {
    var cur = window.print;
    if (typeof cur === 'function' && cur !== window.__webclawPrintWrapped) {
      window.__webclawOrigPrint = cur.bind(window);
    }
  } catch (e) {}
  window.print = hookedPrint;
  window.__webclawPrintWrapped = hookedPrint;
  window.__webclawPrintHook = true;
})();
"#;

const DRAIN_PENDING_PRINTS_SCRIPT: &str = r#"
(() => {
  const q = window.__webclawPendingPrints || [];
  window.__webclawPendingPrints = [];
  return q;
})()
"#;

const INJECT_SAME_ORIGIN_FRAMES: &str = r#"
(function (src) {
  try { eval(src); } catch (e) {}
  document.querySelectorAll('iframe').forEach(function (el) {
    try {
      var w = el.contentWindow;
      // 始终 eval：脚本内部按 __webclawPrintWrapped 决定是否重装
      if (w) w.eval(src);
    } catch (e) {}
  });
})
"#;

#[derive(Debug, Clone)]
pub struct PendingPrint {
    pub title: String,
    pub url: String,
}

/// 与下载钩子一并安装：当前文档 + 新文档 + 同源 iframe + OOPIF。
pub async fn install_print_hooks(client: &CdpClient) -> Result<(), String> {
    install_print_hooks_fast(client).await?;
    let wrapped = format!(
        "({INJECT_SAME_ORIGIN_FRAMES})({script})",
        script = serde_json::to_string(PRINT_HOOK_SCRIPT).map_err(|e| e.to_string())?
    );
    let _ = client.evaluate(&wrapped).await;
    for ctx_id in client.frame_context_ids().await {
        let _ = client
            .evaluate_in_context(PRINT_HOOK_SCRIPT, Some(ctx_id))
            .await;
    }
    if let Ok(iframes) =
        crate::chrome::list_iframe_targets_for_parent_on(client.port, &client.target_id).await
    {
        for iframe in iframes {
            if let Ok(child) = CdpClient::connect_to_any_target_on(client.port, &iframe.id).await {
                let _ = child
                    .ensure_new_document_script("print-hook", PRINT_HOOK_SCRIPT)
                    .await;
                let _ = child.evaluate(PRINT_HOOK_SCRIPT).await;
            }
        }
    }
    Ok(())
}

/// 轻量安装：仅当前文档 + 新文档引导，不扫 iframe/OOPIF（供 early_attach 抢窗口.print）。
pub async fn install_print_hooks_fast(client: &CdpClient) -> Result<(), String> {
    client
        .ensure_new_document_script("print-hook", PRINT_HOOK_SCRIPT)
        .await?;
    client.evaluate(PRINT_HOOK_SCRIPT).await?;
    Ok(())
}

pub async fn drain_pending_prints(client: &CdpClient) -> Result<Vec<PendingPrint>, String> {
    let mut out = drain_pending_prints_on_client(client).await?;
    if let Ok(iframes) =
        crate::chrome::list_iframe_targets_for_parent_on(client.port, &client.target_id).await
    {
        for iframe in iframes {
            if let Ok(child) = CdpClient::connect_to_any_target_light_on(client.port, &iframe.id).await
            {
                if let Ok(more) = drain_pending_prints_on_client(&child).await {
                    out.extend(more);
                }
            }
        }
    }
    Ok(out)
}

async fn drain_pending_prints_on_client(client: &CdpClient) -> Result<Vec<PendingPrint>, String> {
    let v = client.evaluate(DRAIN_PENDING_PRINTS_SCRIPT).await?;
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for item in arr {
            let title = item
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let url = item
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            out.push(PendingPrint { title, url });
        }
    }
    Ok(out)
}

/// 静默打印当前页为 PDF（流式，避免大文件撑爆 WebSocket）。
pub async fn print_page_to_pdf_bytes(client: &CdpClient) -> Result<Vec<u8>, String> {
    let resp = client
        .call_with_timeout(
            "Page.printToPDF",
            json!({
                "printBackground": true,
                "preferCSSPageSize": true,
                "transferMode": "ReturnAsStream",
            }),
            Duration::from_secs(120),
        )
        .await?;
    if let Some(handle) = resp.pointer("/result/stream").and_then(|x| x.as_str()) {
        let bytes = read_io_stream(client, handle).await?;
        if bytes.is_empty() {
            return Err("Page.printToPDF 返回空内容".into());
        }
        if !bytes.starts_with(b"%PDF") {
            return Err("Page.printToPDF 返回非 PDF 数据".into());
        }
        return Ok(bytes);
    }

    // 回退：部分 Chrome / 未带 transferMode 时直接给 base64 data
    let data = resp
        .pointer("/result/data")
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("Page.printToPDF 无 stream/data: {resp}"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("PDF base64 解码失败: {e}"))?;
    if !bytes.starts_with(b"%PDF") {
        return Err("Page.printToPDF 返回非 PDF 数据".into());
    }
    Ok(bytes)
}

async fn read_io_stream(client: &CdpClient, handle: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    const CHUNK: u64 = 262_144;
    loop {
        let resp = client
            .call_with_timeout(
                "IO.read",
                json!({ "handle": handle, "size": CHUNK }),
                Duration::from_secs(60),
            )
            .await?;
        let result = resp.get("result").unwrap_or(&resp);
        let eof = result.get("eof").and_then(|x| x.as_bool()).unwrap_or(false);
        if let Some(data) = result.get("data").and_then(|x| x.as_str()) {
            if !data.is_empty() {
                let chunk = if result
                    .get("base64Encoded")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false)
                {
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|e| format!("IO.read base64 失败: {e}"))?
                } else {
                    data.as_bytes().to_vec()
                };
                out.extend_from_slice(&chunk);
            }
        }
        if eof {
            break;
        }
        // 防止异常空转
        if out.len() > 512 * 1024 * 1024 {
            let _ = client.call("IO.close", json!({ "handle": handle })).await;
            return Err("PDF 超过 512MB 上限".into());
        }
    }
    let _ = client.call("IO.close", json!({ "handle": handle })).await;
    Ok(out)
}

pub fn filename_for_print(title: &str) -> String {
    let base = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string();
    let base = if base.is_empty() {
        "打印".to_string()
    } else if base.len() > 80 {
        base.chars().take(80).collect()
    } else {
        base
    };
    if base.to_ascii_lowercase().ends_with(".pdf") {
        base
    } else {
        format!("{base}.pdf")
    }
}

/// Chrome 原生打印预览页（会抢焦点，自动化表现为「卡住等人点取消」）。
pub fn is_chrome_print_ui_url(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("chrome://print")
        || u.starts_with("chrome-untrusted://print")
        || u.starts_with("edge://print")
}

/// 关闭原生打印预览相关 target；返回关闭数量。
pub async fn dismiss_native_print_ui(port: u16) -> usize {
    let Ok(targets) = crate::chrome::list_targets_on(port).await else {
        return 0;
    };
    let mut closed = 0;
    for t in targets {
        if !is_chrome_print_ui_url(&t.url) {
            continue;
        }
        if crate::chrome::close_page_target_on(port, &t.id).await.is_ok() {
            closed += 1;
        }
    }
    closed
}

/// 关打印预览 + 对当前页发 Escape（覆盖非独立 target 的预览遮罩）。
pub async fn dismiss_native_print_ui_with_escape(client: &CdpClient) -> usize {
    let n = dismiss_native_print_ui(client.port).await;
    let _ = client
        .dispatch_key_event("Escape", "Escape", 27, None)
        .await;
    n
}

/// 处理页面 `__webclawPendingPrints`：printToPDF → `downloads/`。
/// 若队列空但当前为 OA `isprint=1` 打印页，则主动导出（print 常在 CDP 附着前已跑完）。
pub async fn process_pending_prints(
    client: &CdpClient,
    session: &RunSession,
    step_index: Option<u32>,
) -> Result<Vec<RunArtifact>, String> {
    let pending = drain_pending_prints(client).await?;
    if !pending.is_empty() {
        let mut added = Vec::new();
        let mut last_err: Option<String> = None;
        for req in pending {
            let title = if req.title.is_empty() {
                "(无标题)"
            } else {
                req.title.as_str()
            };
            session.append_download_trace(format!("打印拦截 | 开始静默导出 PDF（{title}）"));
            match export_pdf_to_session(client, session, &req.title, step_index).await {
                Ok(art) => added.push(art),
                Err(e) => last_err = Some(e),
            }
        }
        if added.is_empty() {
            if let Some(e) = last_err {
                return Err(e);
            }
        }
        return Ok(added);
    }

    if client.target_url.contains("isprint=1") {
        return export_isprint_page_once(client, session, step_index).await;
    }
    Ok(Vec::new())
}

async fn export_pdf_to_session(
    client: &CdpClient,
    session: &RunSession,
    title: &str,
    step_index: Option<u32>,
) -> Result<RunArtifact, String> {
    let bytes = print_page_to_pdf_bytes(client).await.map_err(|e| {
        session.append_download_trace(format!("打印拦截 | printToPDF 失败: {e}"));
        e
    })?;
    let name = filename_for_print(title);
    match session.save_download_bytes(&name, &bytes, step_index) {
        Ok(art) => {
            session.append_download_trace(format!(
                "打印拦截 | 已落盘: {} ({} 字节)",
                art.name, art.size_bytes
            ));
            Ok(art)
        }
        Err(e) => {
            session.append_download_trace(format!("打印拦截 | 落盘失败: {e}"));
            Err(e)
        }
    }
}

/// isprint 页兜底：window.print 未入队时主动 Page.printToPDF。
async fn export_isprint_page_once(
    client: &CdpClient,
    session: &RunSession,
    step_index: Option<u32>,
) -> Result<Vec<RunArtifact>, String> {
    let key = format!("isprint-pdf:{}", client.target_id);
    if !session.try_begin_download_url(&key) {
        return Ok(Vec::new());
    }

    let title = client
        .evaluate("(document.title||'').trim()")
        .await
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    let title = if title.is_empty() {
        "打印".to_string()
    } else {
        title
    };

    session.append_download_trace(format!(
        "打印兜底 | isprint 页无 window.print 入队，主动 Page.printToPDF（title={title}）"
    ));

    // 先关掉原生打印预览，否则焦点在打印框上，后续步骤会像「卡死」
    let _ = dismiss_native_print_ui_with_escape(client).await;

    match export_pdf_to_session(client, session, &title, step_index).await {
        Ok(art) => {
            let _ = dismiss_native_print_ui(client.port).await;
            session.mark_download_url_handled(&key);
            Ok(vec![art])
        }
        Err(e) => {
            let _ = dismiss_native_print_ui_with_escape(client).await;
            session.release_download_url_inflight(&key);
            if session
                .record_download_fetch_failure(&key, 3)
                .is_none()
            {
                session.append_download_trace(format!("打印兜底 | 已放弃: {e}"));
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_hook_overrides_window_print() {
        assert!(PRINT_HOOK_SCRIPT.contains("window.print"));
        assert!(PRINT_HOOK_SCRIPT.contains("__webclawPendingPrints"));
        assert!(PRINT_HOOK_SCRIPT.contains("__webclawClientDownload"));
        assert!(PRINT_HOOK_SCRIPT.contains("beforeprint"));
        assert!(PRINT_HOOK_SCRIPT.contains("__webclawPrintWrapped"));
        assert!(
            !PRINT_HOOK_SCRIPT.contains("if (window.__webclawPrintHook) return;"),
            "早退会阻止 print 被覆盖后的重装"
        );
    }

    #[test]
    fn filename_for_print_sanitizes() {
        assert_eq!(filename_for_print(""), "打印.pdf");
        assert_eq!(filename_for_print("月报/汇总"), "月报_汇总.pdf");
        assert_eq!(filename_for_print("已有.pdf"), "已有.pdf");
    }

    #[test]
    fn detects_chrome_print_ui_urls() {
        assert!(is_chrome_print_ui_url("chrome://print/"));
        assert!(is_chrome_print_ui_url(
            "chrome-untrusted://print/ABC/0/print.pdf"
        ));
        assert!(!is_chrome_print_ui_url("http://oa.example/isprint=1"));
    }
}
