//! 浏览器断言脚本运行时（Rhai + CDP XPath）。
//! 与 `rhai_runtime` 隔离：无本机文件 API，不改动既有脚本引擎。

use crate::cdp::CdpClient;
use crate::commands::frame_eval;
use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// 实时 print 回调
pub type AssertPrintSink = Arc<dyn Fn(&str) + Send + Sync>;

pub struct AssertRunResult {
    pub message: String,
    pub logs: Vec<String>,
}

#[derive(Clone, Copy)]
enum ProbeKind {
    Count,
    Visible,
    Text,
    Attr,
    Url,
    Title,
}

struct ProbeReq {
    kind: ProbeKind,
    xpath: String,
    attr_name: String,
    reply: std::sync::mpsc::SyncSender<Result<ProbeResult, String>>,
}

#[derive(Clone, Debug)]
struct ProbeResult {
    count: i64,
    text: String,
    attr: String,
    visible: bool,
    url: String,
    title: String,
}

impl Default for ProbeResult {
    fn default() -> Self {
        Self {
            count: 0,
            text: String::new(),
            attr: String::new(),
            visible: false,
            url: String::new(),
            title: String::new(),
        }
    }
}

/// 在页面上执行 XPath / URL / 标题探测（断言体系私有，不进入点击/等待）。
pub async fn run_assert_script(
    source: &str,
    args: &str,
    client: &CdpClient,
    cancel: Option<&AtomicBool>,
    sticky_oopif_target_id: Option<String>,
    on_print: Option<AssertPrintSink>,
) -> Result<AssertRunResult, String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<ProbeReq>();
    let source = source.to_string();
    let args = args.to_string();
    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let logs_bg = logs.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_bg = cancel_flag.clone();

    let join = tokio::task::spawn_blocking(move || {
        run_rhai_engine(&source, &args, tx, logs_bg, on_print, cancel_bg)
    });

    while let Some(req) = rx.recv().await {
        if cancel.map(|c| c.load(Ordering::SeqCst)).unwrap_or(false) {
            cancel_flag.store(true, Ordering::SeqCst);
            let _ = req.reply.send(Err("已取消".into()));
            continue;
        }
        let result = probe_once(client, sticky_oopif_target_id.as_deref(), &req).await;
        let _ = req.reply.send(result);
    }

    let engine_result = join
        .await
        .map_err(|e| format!("断言脚本线程失败: {e}"))?;

    let log_lines = logs.lock().unwrap().clone();
    engine_result?;
    let message = if log_lines.is_empty() {
        "断言脚本执行完成".to_string()
    } else {
        log_lines
            .last()
            .cloned()
            .unwrap_or_else(|| "断言脚本执行完成".to_string())
    };
    Ok(AssertRunResult {
        message,
        logs: log_lines,
    })
}

fn run_rhai_engine(
    source: &str,
    args: &str,
    tx: mpsc::UnboundedSender<ProbeReq>,
    logs: Arc<Mutex<Vec<String>>>,
    on_print: Option<AssertPrintSink>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut engine = Engine::new();
    engine.set_max_expr_depths(64, 32);

    {
        let logs = logs.clone();
        let on_print = on_print.clone();
        engine.on_print(move |s| {
            let line = s.to_string();
            logs.lock().unwrap().push(line.clone());
            if let Some(ref sink) = on_print {
                sink(&line);
            }
        });
    }

    register_assert_fns(&mut engine, tx, cancel);
    register_arg_helpers(&mut engine);

    let mut scope = Scope::new();
    scope.push("ARGS", args.trim().to_string());

    let _ = engine
        .eval_with_scope::<Dynamic>(&mut scope, source)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 内置断言脚本用的 ARGS 解析小工具（与业务 rhai_runtime 隔离）
fn register_arg_helpers(engine: &mut Engine) {
    engine.register_fn("str_trim", |s: &str| -> String { s.trim().to_string() });
    engine.register_fn("str_index_of", |s: &str, sub: &str| -> i64 {
        s.find(sub).map(|i| i as i64).unwrap_or(-1)
    });
    engine.register_fn("str_len", |s: &str| -> i64 { s.chars().count() as i64 });
    engine.register_fn("str_slice", |s: &str, start: i64, end: i64| -> String {
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len() as i64;
        let a = start.clamp(0, n) as usize;
        let b = end.clamp(0, n) as usize;
        if a >= b {
            return String::new();
        }
        chars[a..b].iter().collect()
    });
    engine.register_fn("arg_int", |s: &str| -> i64 {
        s.trim().parse::<i64>().unwrap_or(0)
    });
    engine.register_fn("arg_part", |s: &str, sep: &str, idx: i64| -> String {
        if sep.is_empty() {
            return if idx == 0 {
                s.trim().to_string()
            } else {
                String::new()
            };
        }
        let parts: Vec<&str> = s.split(sep).collect();
        parts
            .get(idx as usize)
            .map(|p| p.trim().to_string())
            .unwrap_or_default()
    });
    engine.register_fn("arg_parts", |s: &str, sep: &str| -> i64 {
        if sep.is_empty() {
            return if s.is_empty() { 0 } else { 1 };
        }
        s.split(sep).count() as i64
    });
}

fn register_assert_fns(
    engine: &mut Engine,
    tx: mpsc::UnboundedSender<ProbeReq>,
    cancel: Arc<AtomicBool>,
) {
    let bridge = AssertBridge { tx, cancel };

    {
        let b = bridge.clone();
        engine.register_fn("xpath_count", move |xpath: &str| -> Result<i64, Box<EvalAltResult>> {
            Ok(b.probe_now(ProbeKind::Count, xpath, "", 0.0)?.count)
        });
    }
    {
        let b = bridge.clone();
        engine.register_fn("xpath_text", move |xpath: &str| -> Result<String, Box<EvalAltResult>> {
            Ok(b.probe_now(ProbeKind::Text, xpath, "", 0.0)?.text)
        });
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "xpath_attr",
            move |xpath: &str, name: &str| -> Result<String, Box<EvalAltResult>> {
                Ok(b.probe_now(ProbeKind::Attr, xpath, name, 0.0)?.attr)
            },
        );
    }

    // assert_exists
    {
        let b = bridge.clone();
        engine.register_fn("assert_exists", move |xpath: &str| -> Result<(), Box<EvalAltResult>> {
            b.assert_exists(xpath, 5.0)
        });
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_exists",
            move |xpath: &str, timeout: f64| -> Result<(), Box<EvalAltResult>> {
                b.assert_exists(xpath, timeout)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_exists",
            move |xpath: &str, timeout: i64| -> Result<(), Box<EvalAltResult>> {
                b.assert_exists(xpath, timeout as f64)
            },
        );
    }

    // assert_not_exists
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_not_exists",
            move |xpath: &str| -> Result<(), Box<EvalAltResult>> {
                b.assert_not_exists(xpath, 5.0)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_not_exists",
            move |xpath: &str, timeout: f64| -> Result<(), Box<EvalAltResult>> {
                b.assert_not_exists(xpath, timeout)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_not_exists",
            move |xpath: &str, timeout: i64| -> Result<(), Box<EvalAltResult>> {
                b.assert_not_exists(xpath, timeout as f64)
            },
        );
    }

    // assert_visible
    {
        let b = bridge.clone();
        engine.register_fn("assert_visible", move |xpath: &str| -> Result<(), Box<EvalAltResult>> {
            b.assert_visible(xpath, 5.0)
        });
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_visible",
            move |xpath: &str, timeout: f64| -> Result<(), Box<EvalAltResult>> {
                b.assert_visible(xpath, timeout)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_visible",
            move |xpath: &str, timeout: i64| -> Result<(), Box<EvalAltResult>> {
                b.assert_visible(xpath, timeout as f64)
            },
        );
    }

    // assert_text (contains)
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_text",
            move |xpath: &str, expected: &str| -> Result<(), Box<EvalAltResult>> {
                b.assert_text(xpath, expected, 5.0)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_text",
            move |xpath: &str, expected: &str, timeout: f64| -> Result<(), Box<EvalAltResult>> {
                b.assert_text(xpath, expected, timeout)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_text",
            move |xpath: &str, expected: &str, timeout: i64| -> Result<(), Box<EvalAltResult>> {
                b.assert_text(xpath, expected, timeout as f64)
            },
        );
    }

    // assert_attr
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_attr",
            move |xpath: &str, name: &str, expected: &str| -> Result<(), Box<EvalAltResult>> {
                b.assert_attr(xpath, name, expected, 5.0)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_attr",
            move |xpath: &str,
                  name: &str,
                  expected: &str,
                  timeout: f64|
                  -> Result<(), Box<EvalAltResult>> {
                b.assert_attr(xpath, name, expected, timeout)
            },
        );
    }

    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_url_contains",
            move |substr: &str| -> Result<(), Box<EvalAltResult>> {
                b.assert_url_contains(substr, 5.0)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_url_equals",
            move |url: &str| -> Result<(), Box<EvalAltResult>> { b.assert_url_equals(url, 5.0) },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_title_contains",
            move |substr: &str| -> Result<(), Box<EvalAltResult>> {
                b.assert_title_contains(substr, 5.0)
            },
        );
    }
    {
        let b = bridge.clone();
        engine.register_fn(
            "assert_title_equals",
            move |title: &str| -> Result<(), Box<EvalAltResult>> {
                b.assert_title_equals(title, 5.0)
            },
        );
    }
}

#[derive(Clone)]
struct AssertBridge {
    tx: mpsc::UnboundedSender<ProbeReq>,
    cancel: Arc<AtomicBool>,
}

impl AssertBridge {
    fn probe_now(
        &self,
        kind: ProbeKind,
        xpath: &str,
        attr_name: &str,
        _timeout: f64,
    ) -> Result<ProbeResult, Box<EvalAltResult>> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(err_runtime("已取消".into()));
        }
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        self.tx
            .send(ProbeReq {
                kind,
                xpath: xpath.to_string(),
                attr_name: attr_name.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| err_runtime("断言运行时已关闭".into()))?;
        reply_rx
            .recv()
            .map_err(|_| err_runtime("断言探测中断".into()))?
            .map_err(err_runtime)
    }

    fn poll_until(
        &self,
        kind: ProbeKind,
        xpath: &str,
        attr_name: &str,
        timeout_secs: f64,
        mut ok: impl FnMut(&ProbeResult) -> bool,
        fail_msg: impl Fn(&ProbeResult) -> String,
    ) -> Result<(), Box<EvalAltResult>> {
        let timeout = Duration::from_secs_f64(timeout_secs.max(0.0));
        let deadline = Instant::now() + timeout;
        let mut last = ProbeResult::default();
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err(err_runtime("已取消".into()));
            }
            last = self.probe_now(kind, xpath, attr_name, 0.0)?;
            if ok(&last) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(err_runtime(fail_msg(&last)));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn assert_exists(&self, xpath: &str, timeout: f64) -> Result<(), Box<EvalAltResult>> {
        self.poll_until(
            ProbeKind::Count,
            xpath,
            "",
            timeout,
            |r| r.count >= 1,
            |r| format!("断言失败 assert_exists({xpath})：匹配数={}", r.count),
        )
    }

    fn assert_not_exists(&self, xpath: &str, timeout: f64) -> Result<(), Box<EvalAltResult>> {
        self.poll_until(
            ProbeKind::Count,
            xpath,
            "",
            timeout,
            |r| r.count == 0,
            |r| format!("断言失败 assert_not_exists({xpath})：仍匹配 {} 个", r.count),
        )
    }

    fn assert_visible(&self, xpath: &str, timeout: f64) -> Result<(), Box<EvalAltResult>> {
        self.poll_until(
            ProbeKind::Visible,
            xpath,
            "",
            timeout,
            |r| r.count >= 1 && r.visible,
            |r| {
                format!(
                    "断言失败 assert_visible({xpath})：count={} visible={}",
                    r.count, r.visible
                )
            },
        )
    }

    fn assert_text(
        &self,
        xpath: &str,
        expected: &str,
        timeout: f64,
    ) -> Result<(), Box<EvalAltResult>> {
        let expected = expected.to_string();
        self.poll_until(
            ProbeKind::Text,
            xpath,
            "",
            timeout,
            |r| {
                r.count >= 1 && compact_text(&r.text).contains(&compact_text(&expected))
            },
            |r| {
                format!(
                    "断言失败 assert_text({xpath}, {expected:?})：实际={:?}",
                    r.text
                )
            },
        )
    }

    fn assert_attr(
        &self,
        xpath: &str,
        name: &str,
        expected: &str,
        timeout: f64,
    ) -> Result<(), Box<EvalAltResult>> {
        let expected = expected.trim().to_string();
        let name_owned = name.to_string();
        self.poll_until(
            ProbeKind::Attr,
            xpath,
            name,
            timeout,
            |r| r.count >= 1 && r.attr.trim() == expected,
            |r| {
                format!(
                    "断言失败 assert_attr({xpath}, {name_owned}, {expected:?})：实际={:?}",
                    r.attr
                )
            },
        )
    }

    fn assert_url_contains(&self, substr: &str, timeout: f64) -> Result<(), Box<EvalAltResult>> {
        let substr = substr.to_string();
        self.poll_until(
            ProbeKind::Url,
            "",
            "",
            timeout,
            |r| r.url.contains(&substr),
            |r| format!("断言失败 assert_url_contains({substr:?})：实际 URL={}", r.url),
        )
    }

    fn assert_url_equals(&self, url: &str, timeout: f64) -> Result<(), Box<EvalAltResult>> {
        let url = url.trim().to_string();
        self.poll_until(
            ProbeKind::Url,
            "",
            "",
            timeout,
            |r| r.url.trim() == url,
            |r| format!("断言失败 assert_url_equals({url:?})：实际={}", r.url),
        )
    }

    fn assert_title_contains(&self, substr: &str, timeout: f64) -> Result<(), Box<EvalAltResult>> {
        let substr = substr.to_string();
        self.poll_until(
            ProbeKind::Title,
            "",
            "",
            timeout,
            |r| compact_text(&r.title).contains(&compact_text(&substr)),
            |r| {
                format!(
                    "断言失败 assert_title_contains({substr:?})：实际标题={:?}",
                    r.title
                )
            },
        )
    }

    fn assert_title_equals(&self, title: &str, timeout: f64) -> Result<(), Box<EvalAltResult>> {
        let title = title.trim().to_string();
        self.poll_until(
            ProbeKind::Title,
            "",
            "",
            timeout,
            |r| compact_text(&r.title) == compact_text(&title),
            |r| format!("断言失败 assert_title_equals({title:?})：实际={:?}", r.title),
        )
    }
}

/// 去掉全部空白，用于「查 询」与「查询」这类页面插空格的宽松匹配
fn compact_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn err_runtime(msg: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(msg.into(), rhai::Position::NONE))
}

async fn probe_once(
    client: &CdpClient,
    sticky_oopif: Option<&str>,
    req: &ProbeReq,
) -> Result<ProbeResult, String> {
    match req.kind {
        ProbeKind::Url => {
            let v = client
                .evaluate_in_context_with_timeout(
                    "(function(){ return { url: String(location.href||'') }; })()",
                    None,
                    Duration::from_secs(10),
                )
                .await?;
            Ok(ProbeResult {
                url: json_str(&v, "url"),
                ..Default::default()
            })
        }
        ProbeKind::Title => {
            let v = client
                .evaluate_in_context_with_timeout(
                    "(function(){ return { title: String(document.title||'') }; })()",
                    None,
                    Duration::from_secs(10),
                )
                .await?;
            Ok(ProbeResult {
                title: json_str(&v, "title"),
                ..Default::default()
            })
        }
        ProbeKind::Count | ProbeKind::Visible | ProbeKind::Text | ProbeKind::Attr => {
            let expr = build_xpath_expr(&req.xpath, req.kind, &req.attr_name);
            probe_xpath_across_frames(client, sticky_oopif, req.kind, &expr).await
        }
    }
}

/// 主文档 → 同源 iframe → sticky OOPIF → 其余 OOPIF。
/// Count：各文档匹配数求和；Visible/Text/Attr：返回首个 count≥1 的结果。
async fn probe_xpath_across_frames(
    client: &CdpClient,
    sticky_oopif: Option<&str>,
    kind: ProbeKind,
    expr: &str,
) -> Result<ProbeResult, String> {
    let timeout = Duration::from_secs(10);
    let mut total = ProbeResult::default();
    let mut any_ok = false;
    let mut last_err: Option<String> = None;
    let want_first_hit = !matches!(kind, ProbeKind::Count);

    // 1) 主文档
    match client
        .evaluate_in_context_with_timeout(expr, None, timeout)
        .await
    {
        Ok(v) => match parse_xpath_value(&v) {
            Ok(parsed) => {
                any_ok = true;
                if want_first_hit && parsed.count >= 1 {
                    return Ok(parsed);
                }
                merge_probe(&mut total, parsed);
            }
            Err(e) => last_err = Some(e),
        },
        Err(e) => last_err = Some(e),
    }

    // 2) 同源 iframe（同 CDP target，不同 execution context）
    for ctx_id in client.child_frame_context_ids().await {
        match client
            .evaluate_in_context_with_timeout(expr, Some(ctx_id), timeout)
            .await
        {
            Ok(v) => {
                if let Ok(parsed) = parse_xpath_value(&v) {
                    any_ok = true;
                    if want_first_hit && parsed.count >= 1 {
                        return Ok(parsed);
                    }
                    merge_probe(&mut total, parsed);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }

    // 3) OOPIF：先 sticky，再发现其余
    let mut oopif_ids: Vec<String> = Vec::new();
    if let Some(tid) = sticky_oopif {
        if !tid.is_empty() && tid != client.target_id {
            oopif_ids.push(tid.to_string());
        }
    }
    let discovered = crate::chrome::discover_oopif_targets_for_parent(
        client.port,
        &client.target_id,
        Some(client),
        crate::chrome::DiscoverOopifMode::Peek,
    )
    .await;
    for t in discovered {
        if t.id != client.target_id && !oopif_ids.iter().any(|x| x == &t.id) {
            oopif_ids.push(t.id);
        }
    }

    for tid in oopif_ids {
        match frame_eval::evaluate_on_target(client, Some(&tid), expr).await {
            Ok(v) => {
                if let Ok(parsed) = parse_xpath_value(&v) {
                    any_ok = true;
                    if want_first_hit && parsed.count >= 1 {
                        return Ok(parsed);
                    }
                    merge_probe(&mut total, parsed);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }

    if any_ok {
        Ok(total)
    } else {
        Err(last_err.unwrap_or_else(|| "XPath 探测失败（主文档与 iframe 均不可用）".into()))
    }
}

fn merge_probe(total: &mut ProbeResult, parsed: ProbeResult) {
    total.count += parsed.count;
    if total.text.is_empty() {
        total.text = parsed.text;
    }
    if total.attr.is_empty() {
        total.attr = parsed.attr;
    }
    total.visible = total.visible || parsed.visible;
}

pub fn build_xpath_expr(xpath: &str, kind: impl Into<XpathExprKind>, attr_name: &str) -> String {
    let kind = kind.into();
    let xp = serde_json::to_string(xpath).unwrap_or_else(|_| "\"\"".into());
    let attr = serde_json::to_string(attr_name).unwrap_or_else(|_| "\"\"".into());
    let need_visible = matches!(kind, XpathExprKind::Visible);
    let need_text = matches!(kind, XpathExprKind::Text | XpathExprKind::Visible);
    let need_attr = matches!(kind, XpathExprKind::Attr);
    format!(
        r#"(function(){{
  const xp = {xp};
  const attrName = {attr};
  const compact = (s) => String(s || '').replace(/\s+/g, '');
  let count = 0, text = '', attr = '', visible = false;
  const fillFromNode = (n) => {{
    if (!n) return;
    if ({need_text}) {{
      text = (n.textContent || '').replace(/\s+/g, ' ').trim();
    }}
    if ({need_attr} && attrName) {{
      attr = (n.getAttribute && n.getAttribute(attrName)) || '';
      attr = String(attr).trim();
    }}
    if ({need_visible}) {{
      const el = n.nodeType === 1 ? n : n.parentElement;
      if (el && el.getBoundingClientRect) {{
        const st = window.getComputedStyle(el);
        const rect = el.getBoundingClientRect();
        visible = st.display !== 'none' && st.visibility !== 'hidden'
          && Number(st.opacity) !== 0
          && rect.width > 0 && rect.height > 0;
      }}
    }}
  }};
  try {{
    const r = document.evaluate(xp, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
    count = r.snapshotLength || 0;
    if (count > 0) {{
      fillFromNode(r.snapshotItem(0));
    }} else {{
      // 页面常在字间插空格（「查 询」），原生 contains(.,'查询') 会失败 → 去空白后再匹配
      const m = xp.match(/^(.+?)\[contains\(\s*\.\s*,\s*(['"])([\s\S]*?)\2\s*\)\](.*)$/);
      if (m) {{
        const baseXp = m[1] + (m[4] || '');
        const needle = compact(m[3]);
        if (needle) {{
          const r2 = document.evaluate(baseXp, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
          const hits = [];
          for (let i = 0; i < r2.snapshotLength; i++) {{
            const n = r2.snapshotItem(i);
            if (n && compact(n.textContent).indexOf(needle) >= 0) hits.push(n);
          }}
          count = hits.length;
          if (count > 0) fillFromNode(hits[0]);
        }}
      }}
    }}
  }} catch (e) {{
    return {{ ok: false, error: String(e && e.message || e), count: 0, text: '', attr: '', visible: false }};
  }}
  return {{ ok: true, count: count, text: text, attr: attr, visible: visible }};
}})()"#
    )
}

#[derive(Clone, Copy)]
pub enum XpathExprKind {
    Count,
    Visible,
    Text,
    Attr,
}

impl From<ProbeKind> for XpathExprKind {
    fn from(k: ProbeKind) -> Self {
        match k {
            ProbeKind::Count => XpathExprKind::Count,
            ProbeKind::Visible => XpathExprKind::Visible,
            ProbeKind::Text => XpathExprKind::Text,
            ProbeKind::Attr => XpathExprKind::Attr,
            ProbeKind::Url | ProbeKind::Title => XpathExprKind::Count,
        }
    }
}

fn parse_xpath_value(v: &Value) -> Result<ProbeResult, String> {
    if v.get("ok").and_then(|x| x.as_bool()) == Some(false) {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("XPath 错误");
        return Err(err.to_string());
    }
    Ok(ProbeResult {
        count: v.get("count").and_then(|x| x.as_i64()).unwrap_or(0),
        text: json_str(v, "text"),
        attr: json_str(v, "attr"),
        visible: v.get("visible").and_then(|x| x.as_bool()).unwrap_or(false),
        url: String::new(),
        title: String::new(),
    })
}

fn json_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_text_strips_inner_spaces() {
        assert_eq!(compact_text("查 询"), "查询");
        assert_eq!(compact_text("  查\u{3000}询\n"), "查询");
        assert!(compact_text("查 询").contains(&compact_text("查询")));
    }

    #[test]
    fn all_builtin_assert_scripts_compile() {
        let mut engine = Engine::new();
        engine.set_max_expr_depths(64, 32);
        register_arg_helpers(&mut engine);
        // stub assert APIs so bodies that call them still compile
        engine.register_fn("assert_exists", |_: &str| ());
        engine.register_fn("assert_exists", |_: &str, _: i64| ());
        engine.register_fn("assert_exists", |_: &str, _: f64| ());
        engine.register_fn("assert_not_exists", |_: &str| ());
        engine.register_fn("assert_not_exists", |_: &str, _: i64| ());
        engine.register_fn("assert_not_exists", |_: &str, _: f64| ());
        engine.register_fn("assert_visible", |_: &str| ());
        engine.register_fn("assert_visible", |_: &str, _: i64| ());
        engine.register_fn("assert_visible", |_: &str, _: f64| ());
        engine.register_fn("assert_text", |_: &str, _: &str| ());
        engine.register_fn("assert_text", |_: &str, _: &str, _: i64| ());
        engine.register_fn("assert_text", |_: &str, _: &str, _: f64| ());
        engine.register_fn("assert_attr", |_: &str, _: &str, _: &str| ());
        engine.register_fn("assert_attr", |_: &str, _: &str, _: &str, _: f64| ());
        engine.register_fn("assert_url_contains", |_: &str| ());
        engine.register_fn("assert_url_equals", |_: &str| ());
        engine.register_fn("assert_title_contains", |_: &str| ());
        engine.register_fn("assert_title_equals", |_: &str| ());
        engine.register_fn("xpath_count", |_: &str| -> i64 { 0 });
        for s in crate::assert_scripts::list_assert_scripts().unwrap_or_default() {
            if !s.builtin {
                continue;
            }
            engine
                .compile(&s.script)
                .unwrap_or_else(|e| panic!("compile {} ({}): {e}", s.name, s.id));
        }
    }

    #[test]
    fn build_xpath_expr_embeds_escaped_xpath() {
        let expr = build_xpath_expr("//button[@id=\"ok\"]", XpathExprKind::Count, "");
        assert!(expr.contains("document.evaluate"));
        assert!(expr.contains("button"));
    }

    #[test]
    fn builtin_assert_script_bodies_compile() {
        let mut engine = Engine::new();
        engine.set_max_expr_depths(64, 32);
        let title = r#"
let expect = ARGS;
if expect == "" { throw "x"; }
assert_title_contains(expect);
"#;
        let exists = r#"
let xp = ARGS;
if xp == "" { throw "x"; }
assert_exists(xp, 10);
"#;
        engine.compile(title).expect("title script compiles");
        engine.compile(exists).expect("exists script compiles");
    }
}
