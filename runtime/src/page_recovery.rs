//! 步骤失败后的页面自动恢复：刷新 → 等待锚点 → 重试（用户菜谱无需额外写法）

use crate::cdp::CdpClient;
use crate::commands::scripts::{self, is_structural_target};
use crate::recipe::Step;
use crate::run_artifacts::RunSession;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 含首次尝试在内的最大执行次数
pub const MAX_ATTEMPTS: u32 = 3;

const LOAD_TIMEOUT_SECS: u64 = 20;
const ANCHOR_TIMEOUT_SECS: u64 = 30;
const MASK_GONE_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAnchor {
    Text(String),
    Selector(String),
    Structural(String),
}

/// 从「等待」步骤参数提取可作为恢复锚点的目标
pub fn anchor_from_wait_args(args: &str) -> Option<RecoveryAnchor> {
    let s = args.trim().trim_end_matches('秒').trim();
    if s.is_empty() {
        return None;
    }
    if s.parse::<f64>().is_ok() {
        return None;
    }
    if s.starts_with("下载") || s.starts_with("加载") || s.starts_with("消失") || s.starts_with("遮罩") {
        return None;
    }
    if let Some(rest) = s.strip_prefix("选择器") {
        let body = strip_trailing_timeout(rest.trim_start_matches([':', '：']).trim());
        if body.is_empty() {
            return None;
        }
        return Some(RecoveryAnchor::Selector(body));
    }
    if let Some(rest) = s.strip_prefix("文字") {
        let body = strip_trailing_timeout(rest.trim_start_matches([':', '：']).trim());
        if body.is_empty() {
            return None;
        }
        return Some(RecoveryAnchor::Text(body));
    }
    let body = strip_trailing_timeout(s);
    if body.is_empty() {
        return None;
    }
    if is_structural_target(&body) {
        return Some(RecoveryAnchor::Structural(body));
    }
    Some(RecoveryAnchor::Text(body))
}

fn structural_anchor_from_step_args(keyword: &str, args: &str) -> Option<RecoveryAnchor> {
    match keyword {
        "点击" => {
            let t = args.trim();
            if is_structural_target(t) {
                Some(RecoveryAnchor::Structural(t.to_string()))
            } else {
                None
            }
        }
        "输入" => {
            let (_, field) = crate::commands::scripts::parse_input_step_args(args);
            if !field.is_empty() {
                if is_structural_target(&field) {
                    return Some(RecoveryAnchor::Structural(field));
                }
            }
            None
        }
        _ => None,
    }
}

/// 登录页常见文案，不宜作为跨页恢复锚点
fn is_weak_login_anchor(a: &RecoveryAnchor) -> bool {
    const WEAK: &[&str] = &[
        "登录", "手机号", "密码", "忘记密码", "请输入手机", "请输入密码", "邮箱",
    ];
    match a {
        RecoveryAnchor::Text(t) => {
            let tl = t.trim().to_lowercase();
            WEAK.iter().any(|w| tl == w.to_lowercase() || tl.contains(&w.to_lowercase()))
        }
        RecoveryAnchor::Structural(_) | RecoveryAnchor::Selector(_) => false,
    }
}

/// 步骤失败后用于恢复的锚点：优先用本步期望目标，而非上一步的「登录」等
pub fn recovery_anchor_for_step(
    step: &Step,
    last_anchor: Option<&RecoveryAnchor>,
) -> Option<RecoveryAnchor> {
    if step.keyword == "等待" {
        if let Some(a) = anchor_from_wait_args(&step.args) {
            if !is_weak_login_anchor(&a) {
                return Some(a);
            }
        }
    }
    if matches!(step.keyword.as_str(), "点击" | "输入") {
        if let Some(a) = structural_anchor_from_step_args(&step.keyword, &step.args) {
            return Some(a);
        }
        let t = step.args.trim();
        if !t.is_empty() {
            let a = RecoveryAnchor::Text(t.to_string());
            if !is_weak_login_anchor(&a) {
                return Some(a);
            }
        }
    }
    last_anchor
        .filter(|a| !is_weak_login_anchor(a))
        .cloned()
}

/// 步骤成功后是否应更新「最近可用锚点」
pub fn anchor_after_success(step: &Step) -> Option<RecoveryAnchor> {
    let anchor = if step.keyword == "等待" {
        anchor_from_wait_args(&step.args)
    } else if step.keyword == "点击" {
        structural_anchor_from_step_args("点击", &step.args).or_else(|| {
            let t = step.args.trim();
            if t.is_empty() {
                None
            } else {
                Some(RecoveryAnchor::Text(t.to_string()))
            }
        })
    } else if step.keyword == "输入" {
        structural_anchor_from_step_args("输入", &step.args)
    } else {
        None
    }?;
    if is_weak_login_anchor(&anchor) {
        None
    } else {
        Some(anchor)
    }
}

/// 是否可对失败步骤做自动刷新重试
/// 失败原因与页面状态无关（如输入框只读/禁用），刷新重试只会白等 → 直接返回提示
pub fn is_unrecoverable_error(err: &str) -> bool {
    if err.contains(scripts::LOCKED_INPUT_HINT_TAIL) {
        return true;
    }
    // 文案点击未命中：刷新会关掉弹层/向导态，越恢复越丢目标
    if err.contains("后仍未找到") && err.contains("需要 ≥") {
        return true;
    }
    false
}

pub fn is_recoverable(step: &Step) -> bool {
    // 括号试探分支失败应立刻改走备选，不触发页面刷新重试
    if step.is_or_try() {
        return false;
    }
    match step.keyword.as_str() {
        "参数" | "动态参数" | "技能" | "宏" | "上传" | "飞书上传" | "飞书新增" | "飞书更新"
        | "飞书删除" | "飞书查询" | "飞书同步" | "断言" => false,
        "打开" | "点击" | "输入" | "打开标签" => true,
        "等待" => !is_wait_download(&step.args) && !is_wait_sleep_only(&step.args),
        _ => {
            if let Ok(Some(cmd)) = crate::commands::get_by_keyword(&step.keyword) {
                !matches!(
                    cmd.runner,
                    crate::commands::RunnerKind::Rhai
                        | crate::commands::RunnerKind::NamedScript
                        | crate::commands::RunnerKind::NamedAssert
                        | crate::commands::RunnerKind::FeishuBitable
                        | crate::commands::RunnerKind::FeishuDrive
                        | crate::commands::RunnerKind::RustUpload
                )
            } else {
                false
            }
        }
    }
}

fn is_wait_download(args: &str) -> bool {
    args.trim().trim_end_matches('秒').trim().starts_with("下载")
}

fn is_wait_sleep_only(args: &str) -> bool {
    let s = args.trim().trim_end_matches('秒').trim();
    !s.is_empty() && s.parse::<f64>().is_ok()
}

fn strip_trailing_timeout(s: &str) -> String {
    let t = s.trim();
    if let Some((body, timeout)) = t.rsplit_once([' ', '\t']) {
        let body = body.trim();
        if !timeout.is_empty()
            && timeout.parse::<f64>().is_ok()
            && is_structural_target(body)
        {
            return body.to_string();
        }
    }
    // 结构化目标末尾 `#N` + 超时秒数紧挨时，优先剥出常见超时，留下合法 `类型#N`
    if let Some(digits) = regex::Regex::new(r"(\d+(?:\.\d+)?)$")
        .ok()
        .and_then(|re| re.find(t).map(|m| m.as_str().to_string()))
    {
        let preferred: &[f64] = &[
            5.0, 10.0, 15.0, 20.0, 30.0, 45.0, 60.0, 90.0, 120.0, 180.0, 200.0, 300.0,
        ];
        for i in 1..digits.len() {
            let timeout_str = &digits[i..];
            if let Ok(timeout) = timeout_str.parse::<f64>() {
                if preferred.contains(&timeout) {
                    let body = t[..t.len() - timeout_str.len()].trim();
                    if is_structural_target(body) {
                        return body.to_string();
                    }
                }
            }
        }
        let body = t[..t.len() - digits.len()].trim().to_string();
        if is_structural_target(&body) || !body.is_empty() {
            return body;
        }
    }
    t.to_string()
}

/// 刷新页面并等待恢复到可继续执行的状态
pub async fn recover_page(
    client: &mut CdpClient,
    session: &RunSession,
    step_index: u32,
    attempt: u32,
    anchor: Option<&RecoveryAnchor>,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
        return Err("已取消".into());
    }

    client.ensure_attached().await?;

    let _ = save_recovery_screenshot(client, session, step_index, attempt).await;

    if client.reload(false).await.is_err() {
        client.ensure_attached().await?;
        client.reload(false).await?;
    }
    wait_document_ready(client, LOAD_TIMEOUT_SECS, cancel).await?;

    if let Ok(Some(reason)) = page_unhealthy_reason(client).await {
        let _ = reason;
    }

    wait_loading_masks_gone(client, MASK_GONE_TIMEOUT_SECS, cancel).await?;

    if let Some(a) = anchor {
        wait_recovery_anchor(client, a, ANCHOR_TIMEOUT_SECS, cancel).await?;
    }

    let _ = client.sync_page_meta().await;
    Ok(())
}

async fn save_recovery_screenshot(
    client: &mut CdpClient,
    session: &RunSession,
    step_index: u32,
    attempt: u32,
) -> Result<(), String> {
    if client.ensure_attached().await.is_err() {
        return Ok(());
    }
    let png = match client.capture_screenshot_png().await {
        Ok(p) => p,
        Err(e) if CdpClient::is_session_lost_error(&e) => {
            client.ensure_attached().await.ok();
            return Ok(());
        }
        Err(_) => return Ok(()),
    };
    let label = format!("recovery-a{attempt}");
    let _ = session.save_step_screenshot(&png, step_index, &label)?;
    Ok(())
}

async fn wait_document_ready(
    client: &mut CdpClient,
    timeout_secs: u64,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
            return Err("已取消".into());
        }
        let _ = client.ensure_attached().await;
        let ready = client
            .evaluate_resilient("document.readyState")
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        if ready == "complete" || ready == "interactive" {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Err(format!("刷新后页面加载超时（{timeout_secs}s）"))
}

async fn page_unhealthy_reason(client: &mut CdpClient) -> Result<Option<String>, String> {
    let v = client.evaluate_resilient(PAGE_HEALTH_JS).await?;
    parse_health_result(&v)
}

async fn wait_loading_masks_gone(
    client: &mut CdpClient,
    timeout_secs: u64,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    // 单次探测，由 Rust 轮询以便停止可在约 300ms 内生效（勿在 JS 内嵌整段超时）
    const PROBE: &str = r#"( () => {
  const sel = '.loading, .ant-spin, .el-loading-mask, [class*="loading"], [class*="Loading"], [class*="spinner"]';
  try {
    for (const el of document.querySelectorAll(sel)) {
      const s = window.getComputedStyle(el);
      if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) continue;
      const r = el.getBoundingClientRect();
      if (r.width >= 2 && r.height >= 2) return { ok: false, busy: true };
    }
  } catch (_) {}
  return { ok: true };
})()"#;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
            return Err("已取消".into());
        }
        let v = client.evaluate_resilient(PROBE).await.unwrap_or(Value::Null);
        if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    // 遮罩超时非致命，老系统常有常驻 loading
    Ok(())
}

async fn wait_recovery_anchor(
    client: &mut CdpClient,
    anchor: &RecoveryAnchor,
    timeout_secs: u64,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    // 单次探测 + Rust 轮询，避免 JS 内嵌 30s 导致停止卡住
    let expr = match anchor {
        RecoveryAnchor::Text(text) => {
            let q = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
            format!(
                r#"( () => {{
  const query = {q};
  const isVisible = (el) => {{
    if (!el || !(el instanceof Element)) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width >= 2 && r.height >= 2 && r.bottom > 0 && r.right > 0;
  }};
  const textMatches = (text, q) => {{
    const norm = (s) => (s || '').trim().toLowerCase().replace(/\s+/g, '');
    const t = norm(text);
    const ql = norm(q);
    return t && ql && (t === ql || t.includes(ql));
  }};
  const getText = (el) => {{
    const parts = [];
    try {{ const it = (el.innerText || '').trim(); if (it) parts.push(it); }} catch (_) {{}}
    for (const p of [el.value, el.getAttribute('value'), el.getAttribute('placeholder'), el.getAttribute('aria-label'), el.textContent]) {{
      const t = (p || '').trim(); if (t) parts.push(t);
    }}
    return parts.join(' ').trim();
  }};
  const nodes = document.querySelectorAll('a, button, input, textarea, label, span, div, p, li, td, th, h1, h2, h3, h4, h5, h6, [role="button"], [role="link"], [role="menuitem"]');
  for (const el of nodes) {{
    if (!isVisible(el)) continue;
    if (textMatches(getText(el), query)) return {{ ok: true }};
  }}
  return {{ ok: false }};
}})()"#
            )
        }
        RecoveryAnchor::Selector(sel) => {
            let s = serde_json::to_string(sel).unwrap_or_else(|_| "\"\"".into());
            format!(
                r#"( () => {{
  const sel = {s};
  try {{
    const el = document.querySelector(sel);
    if (el) {{
      const st = window.getComputedStyle(el);
      if (st.display !== 'none' && st.visibility !== 'hidden') return {{ ok: true }};
    }}
  }} catch (_) {{}}
  return {{ ok: false }};
}})()"#
            )
        }
        RecoveryAnchor::Structural(spec) => {
            let spec_json = serde_json::to_string(spec).unwrap_or_else(|_| "\"\"".into());
            let resolver = scripts::STRUCTURAL_TARGET_RESOLVER;
            format!(
                r#"( () => {{
{resolver}
  const spec = {spec_json};
  const parsed = parseStructuralTarget(spec);
  if (!parsed) return {{ ok: false, error: '无效结构化锚点' }};
  const el = resolveStructuralTarget(parsed.path);
  if (el && structuralElementVisible(el)) return {{ ok: true }};
  return {{ ok: false }};
}})()"#
            )
        }
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
            return Err("已取消".into());
        }
        let v = client.evaluate_resilient(&expr).await.unwrap_or(Value::Null);
        if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
            return Ok(());
        }
        if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
            return Err(err.to_string());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Err("等待页面锚点超时".into())
}

fn parse_health_result(v: &Value) -> Result<Option<String>, String> {
    if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        return Ok(None);
    }
    Ok(v
        .get("reason")
        .and_then(|x| x.as_str())
        .map(String::from))
}

const PAGE_HEALTH_JS: &str = r#"(() => {
  const href = (location.href || '').toLowerCase();
  if (href.startsWith('chrome-error://') || href.includes('about:neterror')) {
    return { ok: false, reason: '浏览器错误页' };
  }
  const patterns = [
    '系统异常', '服务不可用', '请稍后重试', '服务器错误', '内部错误',
    'Internal Server Error', '502 Bad Gateway', '503 Service', '504 Gateway',
    '无法访问此网站', 'ERR_CONNECTION', 'Whitelabel Error Page',
    'Application Error', '页面不存在', '404 Not Found', '500 Internal',
  ];
  const bodyText = (document.body && (document.body.innerText || document.body.textContent)) || '';
  const sample = bodyText.slice(0, 12000);
  for (const p of patterns) {
    if (sample.includes(p)) return { ok: false, reason: p };
  }
  if (document.readyState === 'complete' && sample.replace(/\s+/g, '').length < 16) {
    return { ok: false, reason: '页面内容为空' };
  }
  return { ok: true };
})()"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_wait_anchor() {
        let a = anchor_from_wait_args("时间管理").unwrap();
        assert_eq!(a, RecoveryAnchor::Text("时间管理".into()));
        let a = anchor_from_wait_args("登录30").unwrap();
        assert_eq!(a, RecoveryAnchor::Text("登录".into()));
    }

    #[test]
    fn parse_selector_anchor() {
        let a = anchor_from_wait_args("选择器#submit15").unwrap();
        assert_eq!(a, RecoveryAnchor::Selector("#submit".into()));
    }

    #[test]
    fn locked_input_error_skips_recovery() {
        assert!(is_unrecoverable_error("输入框只读，请用点击"));
        assert!(is_unrecoverable_error("输入框已禁用，请用点击"));
        assert!(!is_unrecoverable_error("等待 30s 后仍未找到字段「月份」"));
        assert!(is_unrecoverable_error(
            "等待 15s 后仍未找到「请选择人员」（最高 textScore: 0，需要 ≥ 0.92）"
        ));
    }

    #[test]
    fn skip_download_and_sleep() {
        assert!(anchor_from_wait_args("下载30").is_none());
        assert!(anchor_from_wait_args("5").is_none());
        assert!(anchor_from_wait_args("加载10").is_none());
    }

    #[test]
    fn recoverable_keywords() {
        assert!(is_recoverable(&Step::new("点击", "确定")));
        assert!(!is_recoverable(&Step::new("等待", "下载30")));
        assert!(!is_recoverable(&Step::new("上传", "")));
        assert!(!is_recoverable(&Step::new("飞书上传", "考勤")));
        assert!(!is_recoverable(&Step::new("飞书新增", "项目名称=测试")));
        assert!(!is_recoverable(&Step::new("断言", "检查页面标题")));
        assert!(!is_recoverable(&crate::recipe::Step {
            keyword: "点击".into(),
            args: "文件夹".into(),
            or_group: Some(crate::recipe::OrGroupMeta {
                id: 1,
                branch: crate::recipe::OrBranch::Try,
            }),
        }));
    }

    #[test]
    fn recovery_prefers_failing_wait_target() {
        let step = Step::new("等待", "时间管理");
        let last = Some(RecoveryAnchor::Text("登录".into()));
        let a = recovery_anchor_for_step(&step, last.as_ref()).unwrap();
        assert_eq!(a, RecoveryAnchor::Text("时间管理".into()));
    }

    #[test]
    fn weak_login_anchor_not_stored() {
        let step = Step::new("点击", "登录");
        assert!(anchor_after_success(&step).is_none());
    }

    #[test]
    fn parse_structural_wait_anchor() {
        let a = anchor_from_wait_args("按钮#2 10").unwrap();
        assert_eq!(a, RecoveryAnchor::Structural("按钮#2".into()));
        let a = anchor_from_wait_args("表格#1行#1图标按钮#215").unwrap();
        assert_eq!(
            a,
            RecoveryAnchor::Structural("表格#1行#1图标按钮#2".into())
        );
    }

    #[test]
    fn structural_click_recovery_anchor() {
        let step = Step::new("点击", "图标按钮#1");
        let a = recovery_anchor_for_step(&step, None).unwrap();
        assert_eq!(a, RecoveryAnchor::Structural("图标按钮#1".into()));
    }

    #[test]
    fn structural_input_recovery_anchor() {
        let step = Step::new("输入", "hello到输入框#2");
        let a = recovery_anchor_for_step(&step, None).unwrap();
        assert_eq!(a, RecoveryAnchor::Structural("输入框#2".into()));
    }
}
