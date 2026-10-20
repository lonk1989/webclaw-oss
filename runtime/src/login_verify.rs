//! 登录结果校验：提交后检测页面错误提示 / 是否仍停在登录页，失败则中断任务

use crate::cdp::CdpClient;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const POLL_MS: u64 = 400;
/// 导航等待之后再短轮询；多数站点错误 tip 会在 1～3s 内出现
const DEFAULT_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    Success(String),
    Failure(String),
    Pending,
}

/// 轮询至成功、明确失败或超时。超时仍停在登录页视为失败。
pub async fn await_login_outcome(
    client: &mut CdpClient,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    await_login_outcome_with_timeout(client, cancel, DEFAULT_TIMEOUT_SECS).await
}

pub async fn await_login_outcome_with_timeout(
    client: &mut CdpClient,
    cancel: Option<&AtomicBool>,
    timeout_secs: u64,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
            return Err("已取消".into());
        }

        match probe_login_outcome(client).await? {
            LoginOutcome::Success(msg) => return Ok(msg),
            LoginOutcome::Failure(msg) => return Err(msg),
            LoginOutcome::Pending => {}
        }

        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }

    // 超时：再探一次，避免边界误判
    match probe_login_outcome(client).await? {
        LoginOutcome::Success(msg) => Ok(msg),
        LoginOutcome::Failure(msg) => Err(msg),
        LoginOutcome::Pending => Err(friendly_timeout_failure()),
    }
}

fn friendly_timeout_failure() -> String {
    "登录未成功，页面仍停留在登录页。请检查账号密码是否正确，或点击「打开网站账号设置」更新凭据后重试。"
        .into()
}

async fn probe_login_outcome(client: &CdpClient) -> Result<LoginOutcome, String> {
    let v = client.evaluate(PROBE_JS).await?;
    parse_probe_value(&v)
}

fn parse_probe_value(v: &Value) -> Result<LoginOutcome, String> {
    let status = v
        .get("status")
        .and_then(|x| x.as_str())
        .unwrap_or("pending");
    let tip = v
        .get("tip")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim();
    let reason = v
        .get("reason")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim();

    match status {
        "success" => {
            let msg = if reason.is_empty() {
                "登录成功".to_string()
            } else {
                format!("登录成功（{reason}）")
            };
            Ok(LoginOutcome::Success(msg))
        }
        "failure" => Ok(LoginOutcome::Failure(friendly_failure_message(tip, reason))),
        _ => Ok(LoginOutcome::Pending),
    }
}

/// 将页面原始 tip 整理成对用户友好的中断说明
pub fn friendly_failure_message(tip: &str, reason: &str) -> String {
    let tip = tip.trim();
    let reason = reason.trim();
    if !tip.is_empty() {
        if looks_like_credential_error(tip) {
            return format!(
                "登录失败：{tip}。请核对账号密码，或打开「网站账号设置」更新后重试。"
            );
        }
        return format!("登录失败：{tip}。任务已中断。");
    }
    if !reason.is_empty() {
        return format!("登录失败（{reason}）。请检查账号密码后重试。");
    }
    "登录失败。请检查账号密码后重试。".into()
}

pub fn looks_like_credential_error(text: &str) -> bool {
    let t = text.to_lowercase();
    const KEYS: &[&str] = &[
        "密码错误",
        "密码不正确",
        "密码有误",
        "密码不对",
        "账号或密码",
        "帐户或密码",
        "用户名或密码",
        "用户名/密码",
        "账号密码",
        "手机号或密码",
        "手机或密码",
        "账号不存在",
        "帐户不存在",
        "用户不存在",
        "账号错误",
        "帐户错误",
        "登录失败",
        "认证失败",
        "鉴权失败",
        "凭证",
        "incorrect password",
        "wrong password",
        "invalid password",
        "invalid credentials",
        "invalid user",
        "authentication failed",
        "login failed",
        "bad credentials",
    ];
    KEYS.iter().any(|k| t.contains(&k.to_lowercase()))
}

const PROBE_JS: &str = r#"(() => {
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const normUi = (s) => norm(s).toLowerCase().replace(/\s+/g, '');

  const isVisible = (el) => {
    if (!el || !(el instanceof Element)) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width >= 2 && r.height >= 2 && r.bottom > 0 && r.right > 0
      && r.top < window.innerHeight && r.left < window.innerWidth;
  };

  const FAIL_RE = /密码错误|密码不正确|密码有误|密码不对|账号或密码|帐户或密码|用户名或密码|用户名\/密码|账号密码|手机号或密码|手机或密码|账号不存在|帐户不存在|用户不存在|账号错误|帐户错误|登录失败|认证失败|鉴权失败|账号已被?锁定|账户已?禁用|验证码错误|验证码不正确|incorrect\s*password|wrong\s*password|invalid\s*(password|credentials|user)|authentication\s*failed|login\s*failed|bad\s*credentials/i;

  const tipSelectors = [
    '.login-tip',
    '.login-error',
    '.error-tip',
    '.error-msg',
    '.err-msg',
    '.form-error',
    '.ant-form-item-explain-error',
    '.ant-message-error',
    '.ant-alert-error',
    '.el-form-item__error',
    '.el-message--error',
    '.el-alert--error',
    '[role="alert"]',
    '.toast-error',
    '.notify-error',
  ];

  const collectTips = () => {
    const out = [];
    const seen = new Set();
    const push = (raw) => {
      const t = norm(raw);
      if (!t || t.length < 2 || t.length > 120) return;
      const key = normUi(t);
      if (seen.has(key)) return;
      seen.add(key);
      out.push(t);
    };
    for (const sel of tipSelectors) {
      try {
        for (const el of document.querySelectorAll(sel)) {
          if (!isVisible(el)) continue;
          push(el.innerText || el.textContent || '');
        }
      } catch (_) {}
    }
    // 登录表单附近短文本（排除改密弹窗，避免「确认密码」等误伤）
    try {
      const form = document.querySelector('form.login-form, form[class*="login"], .login-form, .login-box, .login-panel');
      if (form && !form.closest('.ant-modal, [role="dialog"], .el-dialog')) {
        for (const el of form.querySelectorAll('div, span, p, label, li')) {
          if (!isVisible(el)) continue;
          const t = norm(el.innerText || el.textContent || '');
          if (t && t.length <= 80 && FAIL_RE.test(t)) push(t);
        }
      }
    } catch (_) {}
    return out;
  };

  const tips = collectTips();
  const failTip = tips.find((t) => FAIL_RE.test(t)) || '';
  if (failTip) {
    return { status: 'failure', tip: failTip, reason: 'page_tip' };
  }

  const dialogTitle = (root) => {
    if (!root) return '';
    const el = root.querySelector(
      '.ant-modal-title, .el-dialog__title, .modal-title, [class*="modal-title"], h1, h2, h3, h4'
    );
    return norm(el ? (el.innerText || el.textContent || '') : '');
  };

  const CHANGE_PWD_RE = /修改密码|更改密码|重置密码|强制.*密码|首次登录.*密码|完善密码|change\s*password|reset\s*password|force.*password|update\s*password/i;

  // 「已在别处登录 / 是否继续登录」确认框：凭证已通过，需后续步骤点确定，勿当登录失败中断
  const RELOGIN_CONFIRM_RE = /已经登录|已登录|正在使用|异地登录|重复登录|挤下线|强制下线|是否.*继续|是否.*登录|确认.*登录|重新登录|继续登录|踢出|强制登录|会话.*存在|already\s*logged\s*in|already\s*signed\s*in|session\s*(already\s*)?exists|continue\s*(to\s*)?login|force\s*login/i;

  // 登录成功后的「修改密码」弹窗（含旧/新/确认密码框）≠ 仍停在登录页
  const detectChangePasswordUi = () => {
    for (const modal of document.querySelectorAll('.ant-modal, [role="dialog"], .el-dialog, .modal')) {
      if (!isVisible(modal)) continue;
      const title = dialogTitle(modal);
      if (CHANGE_PWD_RE.test(title)) return title || '修改密码';
      const cls = String(modal.className || '');
      const wrap = modal.closest('[class*="changePassword"], [class*="change-password"], [class*="ChangePassword"]');
      if (wrap || /changePassword|change-password|modifyPassword|modify-password/i.test(cls)) {
        return title || '修改密码';
      }
      // 弹窗内同时出现旧密码+新密码/确认密码
      const body = norm(modal.innerText || '').slice(0, 400);
      if (/旧\s*密\s*码/.test(body) && (/新\s*密\s*码/.test(body) || /确认密码/.test(body))) {
        return title || '修改密码';
      }
    }
    return '';
  };

  const detectReloginConfirmUi = () => {
    const roots = document.querySelectorAll(
      '.ant-modal, [role="dialog"], .el-dialog, .modal, .messager-window, .messager-body, .panel.window, .window, .layui-layer, .swal2-popup, .aui_dialog'
    );
    for (const modal of roots) {
      if (!isVisible(modal)) continue;
      // EasyUI messager 正文常在 .messager-body
      const title = dialogTitle(modal);
      const body = norm(
        (modal.querySelector('.messager-body, .ant-modal-body, .el-dialog__body, .layui-layer-content')
          || modal).innerText || modal.textContent || ''
      ).slice(0, 500);
      if (CHANGE_PWD_RE.test(title) || CHANGE_PWD_RE.test(body)) continue;
      if (RELOGIN_CONFIRM_RE.test(title) || RELOGIN_CONFIRM_RE.test(body)) {
        return title || body.slice(0, 48) || '已登录确认';
      }
    }
    return '';
  };

  const changePwdTitle = detectChangePasswordUi();
  if (changePwdTitle) {
    return { status: 'success', tip: '', reason: '检测到修改密码弹窗（已进入系统）' };
  }

  const reloginConfirm = detectReloginConfirmUi();
  if (reloginConfirm) {
    return {
      status: 'success',
      tip: '',
      reason: '检测到已登录确认弹窗（可继续点确定）',
    };
  }

  const meta = (el) => {
    const type = ((el.type || 'text') + '').toLowerCase();
    const blob = `${el.placeholder||''} ${el.name||''} ${el.id||''} ${el.className||''} ${el.getAttribute('aria-label')||''} ${el.getAttribute('title')||''}`.toLowerCase();
    return { type, blob };
  };
  const isCaptcha = (blob) => /验证码|校验码|captcha|vcode|verifycode/i.test(blob || '');
  const isChangePasswordField = (el, m) => {
    const blob = `${m.blob} ${(el.id || '')}`.toLowerCase();
    if (/旧密码|新密码|确认密码|oldpassword|old_password|old-password|newpassword|new_password|new-password|confirmpassword|confirm_password|confirm-password|confirmpwd|repeatpassword/i.test(blob)) {
      return true;
    }
    const label = el.closest('.ant-form-item, .el-form-item, .form-item, .input-field');
    if (label) {
      const lt = normUi(label.innerText || '').slice(0, 40);
      if (/旧密码|新密码|确认密码/.test(lt)) return true;
    }
    return false;
  };

  // 仅把「登录页密码框」视为仍在登录；改密弹窗内的 password 不计
  const hasLoginPassword = [...document.querySelectorAll('input')].some((el) => {
    if (!isVisible(el)) return false;
    const m = meta(el);
    if (isCaptcha(m.blob)) return false;
    if (!(m.type === 'password' || /密码|password|pwd/i.test(m.blob))) return false;
    if (isChangePasswordField(el, m)) return false;
    const dialog = el.closest('.ant-modal, [role="dialog"], .el-dialog, .modal');
    if (dialog) {
      const title = dialogTitle(dialog);
      if (CHANGE_PWD_RE.test(title)) return false;
      // 其它弹窗里的密码框一般也不是登录页
      return false;
    }
    return true;
  });

  const hasLoginAccount = [...document.querySelectorAll('input, textarea')].some((el) => {
    if (!isVisible(el)) return false;
    if (el.closest('.ant-modal, [role="dialog"], .el-dialog, .modal')) return false;
    const m = meta(el);
    if (['hidden','checkbox','radio','submit','button','reset','file','image','password'].includes(m.type)) return false;
    if (isCaptcha(m.blob)) return false;
    return /手机|电话|账号|邮箱|用户|工号|mobile|phone|user|account|tel|mail|username|login/i.test(m.blob);
  });

  const looksLikeLoginForm = hasLoginPassword && hasLoginAccount;

  const logoutRe = /退出|注销|登出|logout|signout|sign.?out/;
  let hasLogout = false;
  for (const el of document.querySelectorAll('a, button, [role="button"], span, div')) {
    if (!isVisible(el)) continue;
    const t = normUi(`${el.innerText||''} ${el.value||''} ${el.getAttribute('aria-label')||''}`);
    if (t && logoutRe.test(t) && t.length < 40) { hasLogout = true; break; }
  }

  const href = (location.href || '').toLowerCase();
  const loginLikeUrl = /login|signin|sign-in|passport|oauth|sso|\/auth\b/i.test(href)
    || /#\/?(login|signin|auth|passport)/i.test(href);

  if (hasLogout) {
    return { status: 'success', tip: '', reason: '检测到退出入口' };
  }
  if (!looksLikeLoginForm && !loginLikeUrl) {
    return { status: 'success', tip: '', reason: '已离开登录页' };
  }
  if (!looksLikeLoginForm && loginLikeUrl) {
    return { status: 'success', tip: '', reason: '登录表单已消失' };
  }

  return { status: 'pending', tip: '', reason: 'still_login_form' };
})()"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_keywords() {
        assert!(looks_like_credential_error("账号或密码错误"));
        assert!(looks_like_credential_error("密码不正确"));
        assert!(looks_like_credential_error("Invalid credentials"));
        assert!(!looks_like_credential_error("网络超时"));
    }

    #[test]
    fn friendly_message_includes_tip() {
        let m = friendly_failure_message("账号或密码错误", "page_tip");
        assert!(m.contains("账号或密码错误"));
        assert!(m.contains("网站账号"));
    }

    #[test]
    fn parse_failure_probe() {
        let v = json!({ "status": "failure", "tip": "密码错误", "reason": "page_tip" });
        match parse_probe_value(&v).unwrap() {
            LoginOutcome::Failure(msg) => assert!(msg.contains("密码错误")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_success_probe() {
        let v = json!({ "status": "success", "tip": "", "reason": "已离开登录页" });
        match parse_probe_value(&v).unwrap() {
            LoginOutcome::Success(msg) => assert!(msg.contains("登录成功")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_change_password_modal_as_success() {
        let v = json!({
            "status": "success",
            "tip": "",
            "reason": "检测到修改密码弹窗（已进入系统）"
        });
        match parse_probe_value(&v).unwrap() {
            LoginOutcome::Success(msg) => {
                assert!(msg.contains("登录成功"));
                assert!(msg.contains("修改密码"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_relogin_confirm_modal_as_success() {
        let v = json!({
            "status": "success",
            "tip": "",
            "reason": "检测到已登录确认弹窗（可继续点确定）"
        });
        match parse_probe_value(&v).unwrap() {
            LoginOutcome::Success(msg) => {
                assert!(msg.contains("登录成功"));
                assert!(msg.contains("已登录确认") || msg.contains("继续点确定"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
