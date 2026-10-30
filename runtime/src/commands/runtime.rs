use super::focus_context::{self, FocusContext};
use super::frame_eval::{self, ScriptEvalHit};
use super::store::{step_needs_browser, wait_args_browser_free, CommandDef, RunnerKind};
use crate::cdp::CdpClient;
use crate::client_download;
use crate::chrome;
use crate::recipe;
use crate::rhai_runtime;
use crate::run_artifacts::{self, RunSession};
use crate::task_progress::{self, DownloadProgressSink};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use crate::ui_host::{AppHandle, Emitter, Manager, Runtime};

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), String> {
    if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
        return Err("已取消".into());
    }
    Ok(())
}

async fn sleep_cancellable(d: Duration, cancel: Option<&AtomicBool>) -> Result<(), String> {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline {
        check_cancel(cancel)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

pub struct CommandContext<'a, R: Runtime> {
    pub cancel: Option<Arc<AtomicBool>>,
    pub app: Option<&'a AppHandle<R>>,
    pub task_id: Option<&'a str>,
    pub focus: &'a mut FocusContext,
    /// 括号试探步：缩短元素等待，快速失败后改走备选
    pub or_probe: bool,
    /// Rhai print → 主窗口对话日志（后台任务为 None）
    pub on_print: Option<rhai_runtime::RhaiPrintSink>,
}

fn script_extra_ctx<R: Runtime>(ctx: &Option<CommandContext<'_, R>>) -> Option<Value> {
    let c = ctx.as_ref()?;
    let mut obj = serde_json::Map::new();
    obj.insert("focusAnchor".into(), c.focus.to_json());
    if let Some((tid, warm)) = c.focus.sticky_oopif() {
        obj.insert("stickyOopifTargetId".into(), serde_json::json!(tid));
        obj.insert("stickyOopifWarm".into(), serde_json::json!(warm));
    }
    match c.focus.last_surface() {
        super::focus_context::FrameSurface::Parent => {
            obj.insert("preferParent".into(), serde_json::json!(true));
        }
        super::focus_context::FrameSurface::Oopif => {
            obj.insert("preferParent".into(), serde_json::json!(false));
        }
        super::focus_context::FrameSurface::Unknown => {}
    }
    if c.or_probe {
        obj.insert("orProbe".into(), serde_json::json!(true));
        // 试探只需确认目标是否已在页面上，勿用完整 30s 等待
        obj.insert("elementWaitTimeout".into(), serde_json::json!(2.0));
    }
    Some(Value::Object(obj))
}

fn frame_hot_from_extra(extra: Option<&Value>) -> frame_eval::FrameHotPath {
    let Some(obj) = extra.and_then(|v| v.as_object()) else {
        return frame_eval::FrameHotPath::default();
    };
    frame_eval::FrameHotPath {
        preferred_oopif: obj
            .get("stickyOopifTargetId")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        preferred_warm: obj
            .get("stickyOopifWarm")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        prefer_parent: obj
            .get("preferParent")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    }
}

fn remember_frame_hit<R: Runtime>(ctx: &mut Option<CommandContext<'_, R>>, hit: &ScriptEvalHit) {
    if let Some(c) = ctx.as_mut() {
        c.focus.remember_frame_hit(&hit.target_id, hit.is_parent);
    }
}

pub async fn run_command<R: Runtime>(
    mut client: Option<&mut CdpClient>,
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    ctx: Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let cancel_flag = ctx.as_ref().and_then(|c| c.cancel.clone());
    let cancel = cancel_flag.as_deref();
    // 本机步骤：不连 Chrome、不抢焦点（即使 commands.json 里 runner 被误存为 JavaScript）
    if cmd.keyword == "脚本" {
        return run_named_script(args, session, step_index, &ctx).await;
    }
    if cmd.keyword == "断言" || matches!(cmd.runner, RunnerKind::NamedAssert) {
        if !crate::settings::assert_feature_enabled() {
            return Err(
                "断言功能未开启。请在「设置 → 帮助」连续点击版本号 6 次开启，或在「通用」中打开断言能力。"
                    .into(),
            );
        }
    }
    if cmd.keyword == "AI" {
        return run_ai_analyze(session, step_index, args, cancel, ctx.as_ref()).await;
    }
    if matches!(
        cmd.keyword.as_str(),
        "推送消息" | "推送附件" | "推送AI"
    ) || matches!(cmd.runner, RunnerKind::FeishuPush)
    {
        return run_feishu_push(cmd, args, session, step_index, cancel, ctx.as_ref()).await;
    }
    // 「等待」纯秒数：Rust sleep，不依赖页内 JS / Chrome（即使 runner 是 JavaScript）
    if cmd.keyword == "等待" {
        if let Some(secs) = wait_pure_sleep_secs(args) {
            return run_wait(&secs, cancel).await;
        }
        if wait_args_browser_free("等待", args) && args.trim().starts_with("下载") {
            let timeout = wait_timeout_secs_hint(args);
            let v = json!({
                "timeoutSecs": timeout,
                "message": format!("等待下载完成（最多 {timeout}s）"),
            });
            if let Some(c) = client.as_deref_mut() {
                return run_wait_download(c, &v, session, ctx).await;
            }
            return run_wait_download_files_only(&v, session, cancel, ctx.as_ref()).await;
        }
    }
    let need_browser = step_needs_browser(&cmd.keyword, args);
    if need_browser && client.is_none() {
        return Err(format!(
            "「{}」需要浏览器，请先执行「打开」或「登录」",
            cmd.keyword
        ));
    }
    // 统一入口：是否跟随活动标签由 RunnerKind::follows_active_tab + 步骤豁免决定
    if should_follow_active_tab(cmd.runner, &cmd.keyword, args) {
        let client = client
            .as_deref_mut()
            .ok_or_else(|| format!("「{}」需要浏览器，请先执行「打开」或「登录」", cmd.keyword))?;
        let _ = client.sync_to_active_tab().await;
    }
    let result = match cmd.runner {
        RunnerKind::RustOpen => {
            run_open(
                client.ok_or_else(|| "「打开」需要浏览器".to_string())?,
                args,
                cancel,
                ctx.as_ref(),
            )
            .await
        }
        RunnerKind::RustWait => run_wait(args, cancel).await,
        RunnerKind::RustReload => {
            run_reload(
                client.ok_or_else(|| "「刷新」需要浏览器".to_string())?,
                cancel,
            )
            .await
        }
        RunnerKind::RustNewTab => {
            run_new_tab(
                client.ok_or_else(|| "「打开标签」需要浏览器".to_string())?,
                args,
                session,
                cancel,
                ctx.as_ref(),
            )
            .await
        }
        RunnerKind::RustCloseTab => {
            run_close_tab(
                client.ok_or_else(|| "「关闭标签」需要浏览器".to_string())?,
                cancel,
            )
            .await
        }
        RunnerKind::RustClick => {
            run_click(
                client.ok_or_else(|| "「点击」需要浏览器".to_string())?,
                cmd,
                args,
                session,
                step_index,
                ctx,
            )
            .await
        }
        RunnerKind::RustInput => {
            run_input(
                client.ok_or_else(|| "「输入」需要浏览器".to_string())?,
                cmd,
                args,
                session,
                step_index,
                ctx,
            )
            .await
        }
        RunnerKind::RustLogin => {
            run_login(
                client.ok_or_else(|| "「登录」需要浏览器".to_string())?,
                cmd,
                args,
                session,
                step_index,
                cancel,
                ctx.as_ref(),
            )
            .await
        }
        RunnerKind::RustHumanAssist => {
            run_human_assist(
                client.ok_or_else(|| "「人机验证」需要浏览器".to_string())?,
                args,
                &ctx,
            )
            .await
        }
        RunnerKind::Rhai => run_rhai(cmd, args, session, step_index, &ctx).await,
        RunnerKind::NamedScript => run_named_script(args, session, step_index, &ctx).await,
        RunnerKind::NamedAssert => {
            run_named_assert(
                client.ok_or_else(|| "「断言」需要浏览器，请先执行「打开」或「登录」".to_string())?,
                args,
                session,
                step_index,
                &ctx,
            )
            .await
        }
        RunnerKind::FeishuBitable => {
            run_feishu_bitable(
                client.ok_or_else(|| format!("「{}」需要浏览器", cmd.keyword))?,
                cmd,
                args,
                session,
                ctx,
            )
            .await
        }
        RunnerKind::FeishuDrive => {
            run_feishu_drive(
                client.ok_or_else(|| "「飞书上传」需要浏览器".to_string())?,
                args,
                session,
            )
            .await
        }
        RunnerKind::FeishuPush => {
            run_feishu_push(cmd, args, session, step_index, cancel, ctx.as_ref()).await
        }
        RunnerKind::RustUpload => {
            run_upload(
                client.ok_or_else(|| "「上传」需要浏览器".to_string())?,
                args,
                session,
                ctx,
            )
            .await
        }
        RunnerKind::RustTab => {
            run_tab(
                client.ok_or_else(|| "「页签」需要浏览器".to_string())?,
                args,
                cancel,
            )
            .await
        }
        RunnerKind::RustClipboard => {
            run_clipboard(
                client.ok_or_else(|| "「剪切板」需要浏览器".to_string())?,
                args,
                cancel,
            )
            .await
        }
        RunnerKind::JavaScript => {
            run_js_only(
                client.ok_or_else(|| format!("「{}」需要浏览器", cmd.keyword))?,
                cmd,
                args,
                session,
                step_index,
                ctx,
            )
            .await
        }
    };
    result
}

async fn run_rhai<R: Runtime>(
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    ctx: &Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let on_print = ctx.as_ref().and_then(|c| c.on_print.clone());
    let cancel = ctx.as_ref().and_then(|c| c.cancel.clone());
    let app = ctx.as_ref().and_then(|c| c.app.cloned());
    run_rhai_source(
        &cmd.script,
        args,
        session,
        step_index,
        on_print,
        cancel,
        app,
    )
    .await
}

async fn run_named_script<R: Runtime>(
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    ctx: &Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let (script, script_args) = crate::scripts::resolve_invocation(args)?;
    let on_print = ctx.as_ref().and_then(|c| c.on_print.clone());
    let cancel = ctx.as_ref().and_then(|c| c.cancel.clone());
    let app = ctx.as_ref().and_then(|c| c.app.cloned());
    run_rhai_source(
        &script.script,
        &script_args,
        session,
        step_index,
        on_print,
        cancel,
        app,
    )
    .await
}

async fn run_named_assert<R: Runtime>(
    client: &CdpClient,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    ctx: &Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let (script, script_args) = crate::assert_scripts::resolve_invocation(args)?;
    let cancel_flag = ctx.as_ref().and_then(|c| c.cancel.clone());
    let cancel = cancel_flag.as_deref();
    let sticky = ctx
        .as_ref()
        .and_then(|c| c.focus.sticky_oopif().map(|(tid, _)| tid.to_string()));
    let on_print: Option<crate::assert_runtime::AssertPrintSink> =
        ctx.as_ref().and_then(|c| c.on_print.clone());

    let outcome = crate::assert_runtime::run_assert_script(
        &script.script,
        &script_args,
        client,
        cancel,
        sticky,
        on_print,
    )
    .await;

    let (ok, detail) = match &outcome {
        Ok(r) => {
            let d = if r.logs.is_empty() {
                r.message.clone()
            } else {
                r.logs.last().cloned().unwrap_or_else(|| r.message.clone())
            };
            (true, d)
        }
        Err(e) => (false, e.clone()),
    };
    if let Some(session) = session {
        let _ = session.record_assert_result(crate::run_artifacts::AssertReportEntry {
            step_index,
            script_name: script.name.clone(),
            args: script_args.clone(),
            ok,
            detail: detail.clone(),
        });
    }

    match outcome {
        Ok(result) => {
            if result.logs.is_empty() {
                Ok(result.message)
            } else {
                Ok(result.logs.join("\n"))
            }
        }
        Err(e) => Err(e),
    }
}

async fn run_rhai_source<R: Runtime>(
    script: &str,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    on_print: Option<rhai_runtime::RhaiPrintSink>,
    cancel: Option<Arc<AtomicBool>>,
    app: Option<AppHandle<R>>,
) -> Result<String, String> {
    let session = session.ok_or("脚本需要运行产物目录（请在快捷指令/任务中执行）")?;
    let base_dir = session.run_dir.clone();
    let script = script.to_string();
    let args = args.to_string();
    let streamed = on_print.is_some();

    let templates_dir = crate::templates::templates_dir();
    crate::templates::ensure_template_files()?;

    let on_artifact: Option<rhai_runtime::RhaiArtifactSink> = {
        let session = Arc::clone(session);
        let app = app.clone();
        Some(Arc::new(move |path: &std::path::Path| {
            if session
                .register_existing(run_artifacts::ArtifactKind::Other, path, step_index, None)
                .is_ok()
            {
                if let Some(ref app) = app {
                    run_artifacts::emit_artifacts_update(app, &session);
                }
            }
        }))
    };

    let join_result = tokio::task::spawn_blocking({
        let on_artifact = on_artifact.clone();
        move || {
            rhai_runtime::run_script_cancel(
                &script,
                &base_dir,
                &templates_dir,
                &args,
                on_print,
                cancel,
                on_artifact,
            )
        }
    })
    .await
    .map_err(|e| format!("Rhai 执行线程失败: {e}"))?;

    // 成功或失败（含停止）都扫 files/，避免实时模式写出 Excel 后取消却不进产物列表
    let _ = session.sync_files();
    if let Some(ref app) = app {
        run_artifacts::emit_artifacts_update(app, session);
    }

    let result = join_result?;

    let mut msg = result.message;
    for art in &result.artifacts {
        let rel = art
            .path
            .strip_prefix(&session.run_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| art.path.to_string_lossy().into_owned());
        let value = serde_json::json!({
            "ok": true,
            "artifacts": [{
                "kind": "other",
                "name": art.name,
                "path": art.path.to_string_lossy(),
                "relPath": rel,
            }]
        });
        if let Ok(added) = session.process_value(&value, step_index) {
            if let Some(last) = added.last() {
                msg = format!("{msg} | 产物: {}", last.name);
            }
        }
    }
    let _ = session.sync_files();

    // 已实时推送到对话区时不再整包塞进步骤结果，避免 run-log / 步骤摘要重复刷屏
    if !streamed && result.logs.len() > 1 {
        let summary = result.logs.join("\n");
        msg = format!("{msg}\n{summary}");
    }

    Ok(msg)
}

/// 关掉当前标签并在新标签打开（先开后关，避免 Chrome 在 0 标签时退出）。
/// 「打开」「登录」用此路径杀掉旧站渲染进程，避免跨站 navigate 把旧页冻进 bfcache。
async fn recycle_current_tab(
    client: &mut CdpClient,
    url: Option<&str>,
    cancel: Option<&AtomicBool>,
) -> Result<bool, String> {
    check_cancel(cancel)?;
    let port = client.port;
    let old_id = client.target_id.clone();
    let _ = client.unblock_debugger().await;
    let _ = chrome::close_devtools_page_targets_on(port).await;

    let target = crate::browser_network::with_manual_new_tab_unpaused(port, || async {
        chrome::create_page_target_on(port, None).await
    })
    .await?;
    *client = CdpClient::connect_to_target_on(port, &target.id).await?;
    client.prepare_tab_for_automation().await?;

    let closed_old = if !old_id.is_empty() && old_id != client.target_id {
        match chrome::close_page_target_on(port, &old_id).await {
            Ok(()) => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                true
            }
            Err(_) => false,
        }
    } else {
        false
    };
    let _ = client.activate_tab().await;

    if let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) {
        client.navigate(url).await?;
        client.wait_after_navigation_cancel(30, cancel).await?;
        let _ = client.sync_page_meta().await;
    }
    check_cancel(cancel)?;
    client.prepare_tab_for_automation().await?;
    Ok(closed_old)
}

async fn maybe_wait_feishu_qr_login<R: Runtime>(
    ctx: Option<&CommandContext<'_, R>>,
    client: &mut CdpClient,
    cancel: Option<&AtomicBool>,
    url_hint: &str,
) -> Result<(), String> {
    let page_url = if client.target_url.trim().is_empty() {
        url_hint
    } else {
        client.target_url.as_str()
    };
    if !crate::feishu::url_needs_login_wait(page_hint(url_hint, page_url)) {
        return Ok(());
    }
    let c = ctx.ok_or("飞书登录等待需要运行上下文")?;
    let app = c.app.ok_or("飞书登录等待需要应用句柄")?;
    let task_id = c.task_id.unwrap_or("worker");
    let cancel_owned = Arc::new(AtomicBool::new(false));
    let cancel_ref: &AtomicBool = cancel.unwrap_or(cancel_owned.as_ref());
    let state = app
        .try_state::<crate::AppState>()
        .ok_or("飞书登录等待：应用状态不可用")?;
    let push = |line: String| {
        if let Some(ctrl) = state.task_registry.get(task_id) {
            crate::run_user_log::log_user(&ctrl, state.inner(), app, line);
        } else {
            state.push_log(line);
        }
    };
    #[cfg(feature = "desktop")]
    {
        crate::feishu::ensure_feishu_logged_in(app, task_id, client, Some(cancel_ref), push).await
    }
    #[cfg(not(feature = "desktop"))]
    {
        let _ = (app, task_id, client, cancel_ref, push);
        Ok(())
    }
}

fn page_hint<'a>(nav_arg: &'a str, page_url: &'a str) -> &'a str {
    let arg = nav_arg.trim();
    if arg.is_empty() {
        page_url
    } else {
        arg
    }
}

async fn run_open<R: Runtime>(
    client: &mut CdpClient,
    args: &str,
    cancel: Option<&AtomicBool>,
    ctx: Option<&CommandContext<'_, R>>,
) -> Result<String, String> {
    let url = normalize_url(args);
    let closed_old = recycle_current_tab(client, Some(&url), cancel).await?;
    maybe_wait_feishu_qr_login(ctx, client, cancel, &url).await?;
    if closed_old {
        Ok(format!("已打开 {url} | 已关闭旧标签"))
    } else {
        Ok(format!("已打开 {url}"))
    }
}

async fn run_reload(
    client: &mut CdpClient,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    check_cancel(cancel)?;
    client.reload(false).await?;
    check_cancel(cancel)?;
    Ok("已刷新页面".into())
}

/// 纯秒数等待（含尾部标点）；返回规范化秒数字符串。非纯延时返回 None。
fn wait_pure_sleep_secs(args: &str) -> Option<String> {
    if !wait_args_browser_free("等待", args) {
        return None;
    }
    let a = args.trim();
    if a.starts_with("下载") {
        return None;
    }
    let s = strip_wait_tail_punct(a.trim_end_matches('秒').trim());
    if s.is_empty() {
        return Some("1".into());
    }
    let n: f64 = s.parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some(s.to_string())
}

async fn run_wait(args: &str, cancel: Option<&AtomicBool>) -> Result<String, String> {
    let s = strip_wait_tail_punct(
        args.trim()
            .trim_end_matches('秒')
            .trim(),
    );
    let n: f64 = s
        .parse()
        .map_err(|_| format!("等待秒数无效: {args}"))?;
    if !n.is_finite() || n < 0.0 {
        return Err(format!("等待秒数无效: {args}"));
    }
    sleep_cancellable(Duration::from_secs_f64(n), cancel).await?;
    Ok(format!("已等待 {n} 秒"))
}

async fn run_human_assist<R: Runtime>(
    client: &mut CdpClient,
    args: &str,
    ctx: &Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let (timeout_secs, hint) = crate::captcha::parse_human_assist_args(args);
    let task_id = ctx
        .as_ref()
        .and_then(|c| c.task_id)
        .unwrap_or("worker");
    let cancel_owned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_flag = ctx
        .as_ref()
        .and_then(|c| c.cancel.clone())
        .unwrap_or(cancel_owned);
    let cancel: &std::sync::atomic::AtomicBool = cancel_flag.as_ref();

    // 伙伴空间：文件人机桥，无需桌面 AppHandle
    if let Some(bridge) = crate::human_bridge::FileHumanBridge::from_env() {
        let _ = client.activate_tab().await;
        let hint = if hint.trim().is_empty() {
            "请在浏览器中完成需要人工操作的步骤，完成后回复「完成」".to_string()
        } else {
            hint
        };
        let prompt_id = uuid::Uuid::new_v4().to_string();
        let req = crate::human_bridge::HumanRequest {
            prompt_id,
            task_id: task_id.to_string(),
            kind: "human".into(),
            hint: hint.clone(),
            image_path: None,
            image_base64: None,
        };
        bridge.write_request(&req, None)?;
        crate::run_sink::emit_log(format!("人机验证：{hint}"));
        match bridge
            .wait_reply(cancel, std::time::Duration::from_secs(timeout_secs.clamp(10, 3600)))
            .await?
        {
            crate::human_bridge::HumanReply::Cancelled => return Err("已取消人机验证".into()),
            _ => {
                crate::run_sink::emit_log("人机验证已确认完成");
                return Ok("人机验证已完成".into());
            }
        }
    }

    let c = ctx.as_ref().ok_or("人机验证需要运行上下文")?;
    let app = c.app.ok_or("人机验证需要应用句柄")?;
    let state = app
        .try_state::<crate::AppState>()
        .ok_or("人机验证：应用状态不可用")?;
    let waiters = state.captcha_waiters.clone();
    let push = |line: String| {
        if let Some(ctrl) = state.task_registry.get(task_id) {
            crate::run_user_log::log_user(&ctrl, state.inner(), app, line);
        } else {
            state.push_log(line);
        }
    };
    crate::captcha::wait_human_assist(
        app,
        &waiters,
        task_id,
        client,
        cancel,
        &hint,
        timeout_secs,
        push,
    )
    .await?;
    Ok("人机验证已完成".into())
}

async fn run_new_tab<R: Runtime>(
    client: &mut CdpClient,
    url: &str,
    _session: Option<&Arc<RunSession>>,
    cancel: Option<&AtomicBool>,
    ctx: Option<&CommandContext<'_, R>>,
) -> Result<String, String> {
    check_cancel(cancel)?;
    let url = url.trim();
    let port = client.port;
    let _ = client.unblock_debugger().await;
    let _ = chrome::close_devtools_page_targets_on(port).await;
    let target = crate::browser_network::with_manual_new_tab_unpaused(port, || async {
        chrome::create_page_target_on(port, None).await
    })
    .await?;
    *client = CdpClient::connect_to_target_on(port, &target.id).await?;
    client.prepare_tab_for_automation().await?;
    if !url.is_empty() {
        client.navigate(&normalize_url(url)).await?;
        client.wait_after_navigation_cancel(30, cancel).await?;
        let _ = client.sync_page_meta().await;
    }
    check_cancel(cancel)?;
    client.prepare_tab_for_automation().await?;
    maybe_wait_feishu_qr_login(ctx, client, cancel, url).await?;
    Ok(format!("已新建标签 | {}", client.target_url))
}

/// 关闭当前标签（与「页签关闭」相同，无参数入口）
async fn run_close_tab(
    client: &mut CdpClient,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    let msg = run_tab(client, "关闭", cancel).await?;
    Ok(msg.replacen("已关闭页签", "已关闭标签", 1))
}

#[derive(Debug, PartialEq)]
enum TabAction {
    /// 1-based index
    Switch(usize),
    /// None = current tab
    Close(Option<usize>),
}

fn parse_tab_args(args: &str) -> Result<TabAction, String> {
    let a = recipe::normalize_automation_text(args).trim().to_string();
    if a.is_empty() {
        return Err("请写序号，如 页签#2；关闭用 页签关闭 或 页签关闭#1".into());
    }
    if a.starts_with("关闭") {
        let rest = a["关闭".len()..].trim().trim_start_matches(['#', '＃']);
        if rest.is_empty() || rest == "当前" {
            return Ok(TabAction::Close(None));
        }
        let n: usize = rest
            .parse()
            .map_err(|_| format!("页签关闭序号无效: {rest}"))?;
        if n == 0 {
            return Err("页签序号从 1 开始".into());
        }
        return Ok(TabAction::Close(Some(n)));
    }
    let rest = a
        .strip_prefix("切换")
        .unwrap_or(a.as_str())
        .trim()
        .trim_start_matches(['#', '＃']);
    let n: usize = rest
        .parse()
        .map_err(|_| format!("页签序号无效（例：页签#2、页签关闭）：{a}"))?;
    if n == 0 {
        return Err("页签序号从 1 开始".into());
    }
    Ok(TabAction::Switch(n))
}

fn format_tab_list(pages: &[chrome::TargetInfo]) -> String {
    pages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let title = if p.title.trim().is_empty() {
                "(无标题)"
            } else {
                p.title.trim()
            };
            format!("#{} {}", i + 1, title)
        })
        .collect::<Vec<_>>()
        .join("；")
}

async fn activate_and_attach_page(
    client: &mut CdpClient,
    page: &chrome::TargetInfo,
) -> Result<(), String> {
    *client = CdpClient::connect_to_target_on(client.port, &page.id).await?;
    client.activate_tab().await?;
    client.prepare_tab_for_automation().await?;
    let _ = client.sync_page_meta().await;
    Ok(())
}

/// 关闭/切换后：用探针找活跃页签并附着激活
async fn reattach_active_tab_probe(client: &mut CdpClient) -> Result<String, String> {
    let port = client.port;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let page = chrome::find_foreground_page_on_ex(port, true).await?;
    activate_and_attach_page(client, &page).await?;
    Ok(format!(
        "「{}」| {}",
        if client.target_title.is_empty() {
            "(无标题)"
        } else {
            &client.target_title
        },
        client.target_url
    ))
}

async fn run_tab(
    client: &mut CdpClient,
    args: &str,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    check_cancel(cancel)?;
    let action = parse_tab_args(args)?;
    let port = client.port;
    let _ = chrome::close_devtools_page_targets_on(port).await;
    let pages = chrome::list_recipe_page_targets_on(port).await?;
    if pages.is_empty() {
        return Err("没有可用的浏览器页签".into());
    }

    match action {
        TabAction::Switch(n) => {
            let page = pages.get(n - 1).ok_or_else(|| {
                format!(
                    "页签#{n} 不存在（共 {} 个）：{}",
                    pages.len(),
                    format_tab_list(&pages)
                )
            })?;
            activate_and_attach_page(client, page).await?;
            check_cancel(cancel)?;
            Ok(format!(
                "已切换到页签#{n}「{}」| {}",
                if client.target_title.is_empty() {
                    "(无标题)"
                } else {
                    &client.target_title
                },
                client.target_url
            ))
        }
        TabAction::Close(idx) => {
            let (close_id, label) = if let Some(n) = idx {
                let page = pages.get(n - 1).ok_or_else(|| {
                    format!(
                        "页签#{n} 不存在（共 {} 个）：{}",
                        pages.len(),
                        format_tab_list(&pages)
                    )
                })?;
                (page.id.clone(), format!("#{n}"))
            } else {
                let current = pages
                    .iter()
                    .find(|p| p.id == client.target_id)
                    .or_else(|| pages.first())
                    .ok_or("没有可关闭的页签")?;
                let n = pages.iter().position(|p| p.id == current.id).unwrap_or(0) + 1;
                (current.id.clone(), format!("#{n}(当前)"))
            };

            if pages.len() <= 1 {
                return Err("只剩 1 个页签，无法关闭".into());
            }

            chrome::close_page_target_on(port, &close_id).await?;
            check_cancel(cancel)?;
            let active = reattach_active_tab_probe(client).await?;
            Ok(format!("已关闭页签{label} → 活跃页签 {active}"))
        }
    }
}

#[derive(Debug, PartialEq)]
enum ClipboardAction {
    /// 写入系统剪切板
    CopyText(String),
    /// 复制页面选中/焦点内容到系统剪切板
    CopySelection,
    /// 系统剪切板 → 当前焦点 insertText
    Paste,
}

fn parse_clipboard_args(args: &str) -> Result<ClipboardAction, String> {
    let a = recipe::normalize_automation_text(args).trim().to_string();
    if a.is_empty() {
        return Err("请写：剪切板复制=内容、剪切板复制、剪切板粘贴".into());
    }
    if a.starts_with("粘贴") {
        return Ok(ClipboardAction::Paste);
    }
    if a.starts_with("复制") {
        let rest = a["复制".len()..].trim();
        if rest.is_empty() {
            return Ok(ClipboardAction::CopySelection);
        }
        if let Some(text) = rest.strip_prefix('=') {
            return Ok(ClipboardAction::CopyText(text.to_string()));
        }
        return Ok(ClipboardAction::CopyText(rest.to_string()));
    }
    if let Some(text) = a.strip_prefix('=') {
        return Ok(ClipboardAction::CopyText(text.to_string()));
    }
    Err("请写：剪切板复制=内容、剪切板复制、剪切板粘贴".into())
}

fn os_clipboard_set(text: &str) -> Result<(), String> {
    let mut clip =
        arboard::Clipboard::new().map_err(|e| format!("无法打开系统剪切板: {e}"))?;
    clip.set_text(text.to_string())
        .map_err(|e| format!("写入剪切板失败: {e}"))
}

/// 读取系统剪切板文本；空或不可读时返回空串（供 `{{剪切板}}` 展开）。
pub fn read_system_clipboard() -> String {
    os_clipboard_get().unwrap_or_default()
}

fn os_clipboard_get() -> Result<String, String> {
    let mut clip =
        arboard::Clipboard::new().map_err(|e| format!("无法打开系统剪切板: {e}"))?;
    clip.get_text()
        .map_err(|e| format!("读取剪切板失败: {e}"))
}

const PAGE_SELECTION_JS: &str = r#"(function () {
  try {
    const sel = window.getSelection && window.getSelection();
    if (sel) {
      const t = String(sel.toString() || '');
      if (t) return t;
    }
    const el = document.activeElement;
    if (!el) return '';
    const tag = (el.tagName || '').toUpperCase();
    if (tag === 'INPUT' || tag === 'TEXTAREA') {
      const start = el.selectionStart, end = el.selectionEnd;
      const val = String(el.value || '');
      if (typeof start === 'number' && typeof end === 'number' && end > start) {
        return val.slice(start, end);
      }
      return val;
    }
    if (el.isContentEditable) return String(el.innerText || el.textContent || '');
    return '';
  } catch (e) {
    return '';
  }
})()"#;

async fn run_clipboard(
    client: &mut CdpClient,
    args: &str,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    check_cancel(cancel)?;
    let action = parse_clipboard_args(args)?;
    match action {
        ClipboardAction::CopyText(text) => {
            os_clipboard_set(&text)?;
            let n = text.chars().count();
            let preview: String = text.chars().take(40).collect();
            let preview = if n > 40 {
                format!("{preview}…")
            } else {
                preview
            };
            Ok(format!("已复制到剪切板（{n} 字）: {preview}"))
        }
        ClipboardAction::CopySelection => {
            client.prepare_tab_for_automation().await?;
            let v = client.evaluate(PAGE_SELECTION_JS).await?;
            let text = v.as_str().unwrap_or("").to_string();
            if text.is_empty() {
                return Err("页面没有可复制的选中文字（请先选中，或聚焦输入框）".into());
            }
            os_clipboard_set(&text)?;
            let n = text.chars().count();
            let preview: String = text.chars().take(40).collect();
            let preview = if n > 40 {
                format!("{preview}…")
            } else {
                preview
            };
            Ok(format!("已从页面复制到剪切板（{n} 字）: {preview}"))
        }
        ClipboardAction::Paste => {
            client.prepare_tab_for_automation().await?;
            let text = os_clipboard_get()?;
            if text.is_empty() {
                return Err("剪切板为空".into());
            }
            client.insert_text(&text).await?;
            let n = text.chars().count();
            Ok(format!("已粘贴到当前焦点（{n} 字）"))
        }
    }
}

/// 从脚本返回值解析 CDP 点击坐标；`clicked: true` 表示脚本内已 DOM 点击，跳过 CDP
fn click_coords_from_script(v: &Value) -> Result<Option<(f64, f64)>, String> {
    if v.get("clicked").and_then(|x| x.as_bool()) == Some(true) {
        return Ok(None);
    }
    let x = v.get("x").and_then(|x| x.as_f64()).ok_or("脚本未返回 x")?;
    let y = v.get("y").and_then(|x| x.as_f64()).ok_or("脚本未返回 y")?;
    Ok(Some((x, y)))
}

async fn dispatch_script_click(client: &mut CdpClient, v: &Value) -> Result<(), String> {
    if let Some((x, y)) = click_coords_from_script(v)? {
        client.click_at(x, y).await?;
    }
    Ok(())
}

async fn run_click<R: Runtime>(
    client: &mut CdpClient,
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    mut ctx: Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let cancel_flag = ctx.as_ref().and_then(|c| c.cancel.clone());
    let cancel = cancel_flag.as_deref();
    check_cancel(cancel)?;
    let _ = client_download::ensure_automation_capture(client).await;
    let _ = crate::browser_network::BrowserNetworkCapture::shared_for_port(client.port).await;

    // 像素坐标点击：视口 (x,y)，无参默认 0 0。
    // 无参时主文档与各 iframe 各点一次 (0,0)，用于关掉 iframe 内遮罩弹层。
    if let Some((x, y)) = super::scripts::parse_pixel_click_args(args) {
        let blank_default = super::scripts::is_default_blank_pixel_click(args);
        let _ = client.sync_to_active_tab().await;
        client.prepare_tab_for_automation().await?;

        let mut surfaces: Vec<&'static str> = Vec::new();
        // 1) 主文档
        client.click_at(x, y).await?;
        surfaces.push("主文档");

        if blank_default {
            // 2) 同源子 frame：其本地 (0,0) = 父视口偏移后的点
            let child_ctxs = client.child_frame_context_ids().await;
            for ctx_id in child_ctxs {
                match client.frame_context_viewport_offset(ctx_id).await {
                    Ok((ox, oy)) => {
                        let _ = client.click_at(ox + x, oy + y).await;
                        surfaces.push("同源iframe");
                    }
                    Err(_) => {}
                }
            }
            // 3) 跨域 OOPIF：各自 target 视口 (0,0)
            let mut seen = std::collections::HashSet::new();
            seen.insert(client.target_id.clone());
            let oopifs = crate::chrome::discover_oopif_targets_for_parent(
                client.port,
                &client.target_id,
                Some(client),
                crate::chrome::DiscoverOopifMode::Peek,
            )
            .await;
            for t in oopifs {
                if !seen.insert(t.id.clone()) {
                    continue;
                }
                match frame_eval::click_at_on_target(client, Some(t.id.as_str()), x, y).await {
                    Ok(()) => surfaces.push("跨域iframe"),
                    Err(e) => {
                        eprintln!("[pixel_click] OOPIF {} 点击失败: {e}", t.id);
                    }
                }
            }
            // sticky 热路径再补一次（可能刚被发现）
            let extra_ctx = script_extra_ctx(&ctx);
            let hot = frame_hot_from_extra(extra_ctx.as_ref());
            if let Some(tid) = hot.preferred_oopif.as_deref() {
                if seen.insert(tid.to_string()) {
                    let _ = frame_eval::click_at_on_target(client, Some(tid), x, y).await;
                    surfaces.push("sticky-iframe");
                    if let Some(c) = ctx.as_mut() {
                        c.focus.remember_frame_hit(tid, false);
                    }
                }
            }
        } else {
            // 显式坐标：优先 sticky iframe，否则仅主文档
            let extra_ctx = script_extra_ctx(&ctx);
            let hot = frame_hot_from_extra(extra_ctx.as_ref());
            if !hot.prefer_parent && hot.preferred_warm {
                if let Some(tid) = hot.preferred_oopif.as_deref() {
                    let _ = frame_eval::click_at_on_target(client, Some(tid), x, y).await;
                    surfaces.push("sticky-iframe");
                    if let Some(c) = ctx.as_mut() {
                        c.focus.remember_frame_hit(tid, false);
                    }
                }
            }
        }

        check_cancel(cancel)?;
        client.click_may_open_tab = false;
        let settle = if ctx.as_ref().is_some_and(|c| c.or_probe) {
            400
        } else if blank_default {
            500
        } else {
            600
        };
        sleep_cancellable(Duration::from_millis(settle), cancel).await?;
        let xi = x.round() as i64;
        let yi = y.round() as i64;
        let surface_n = surfaces.len();
        let frame_hint = if surface_n > 1 {
            format!(" | 共点 {surface_n} 处（主文档+iframe）")
        } else if surfaces.iter().any(|s| s.contains("iframe")) {
            " | frame: iframe".to_string()
        } else {
            String::new()
        };
        let detail = format!("已点击坐标 ({xi}, {yi}){frame_hint} | 查询: {args}");
        return Ok(apply_script_result(
            json!({
                "ok": true,
                "pixelClick": true,
                "blankDefault": blank_default,
                "surfaces": surface_n,
                "x": x,
                "y": y,
                "message": detail,
            }),
            session,
            step_index,
            &detail,
        ));
    }

    let extra_ctx = script_extra_ctx(&ctx);
    let hit = eval_user_script_hit(client, cmd, args, session, extra_ctx, cancel).await?;
    check_cancel(cancel)?;
    let v = hit.value.clone();
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("点击脚本失败")
            .to_string());
    }
    remember_frame_hit(&mut ctx, &hit);
    let same_origin_frame = if let Some(ctx_id) = hit.context_id {
        match client.frame_id_for_context(ctx_id).await {
            Some(fid) => {
                let main = client.main_frame_id_public().await;
                main.as_deref() != Some(fid.as_str())
            }
            None => true, // 有 context 但无映射：仍按子 frame 处理，偏移失败会报错
        }
    } else {
        false
    };
    frame_eval::click_on_hit(client, &hit).await?;
    check_cancel(cancel)?;
    client.click_may_open_tab = v.get("opensNewTab").and_then(|x| x.as_bool()) == Some(true);
    // 试探命中后略等即可；完整步骤保留原 1.2s 缓冲
    let settle = if ctx.as_ref().is_some_and(|c| c.or_probe) {
        400
    } else {
        1200
    };
    sleep_cancellable(Duration::from_millis(settle), cancel).await?;
    let matched = v
        .get("matched")
        .and_then(|x| x.as_str())
        .unwrap_or(args)
        .to_string();
    let mut detail = format_click_log(&v, &matched, args);
    if !hit.is_parent {
        detail = format!("{detail} | frame: iframe");
    } else if same_origin_frame || v.get("inFrame").and_then(|x| x.as_bool()) == Some(true) {
        detail = format!("{detail} | frame: 同源iframe");
    }
    if v.get("topX").and_then(|x| x.as_f64()).is_some()
        && v.get("inFrame").and_then(|x| x.as_bool()) == Some(true)
    {
        detail = format!("{detail} | topCoords");
    }
    Ok(apply_script_result(v, session, step_index, &detail))
}

fn format_click_log(v: &Value, matched: &str, fallback_query: &str) -> String {
    let query = v
        .get("query")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_query);
    let mut parts = Vec::new();
    if let Some(msg) = v
        .get("message")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(msg.to_string());
    } else {
        parts.push(matched.to_string());
    }
    parts.push(format!("查询: {query}"));
    if let Some(score) = v.get("score").and_then(|x| x.as_f64()) {
        parts.push(format!("score: {score:.3}"));
    }
    if let Some(text_score) = v.get("textScore").and_then(|x| x.as_f64()) {
        parts.push(format!("textScore: {text_score:.2}"));
    }
    if let Some(n) = v.get("matchIndex").and_then(|x| x.as_u64()) {
        if n > 1 {
            parts.push(format!("序号: #{n}"));
        }
    }
    if let Some(n) = v.get("strongCount").and_then(|x| x.as_u64()) {
        parts.push(format!("候选: {n}"));
    } else if let Some(n) = v.get("hitCount").and_then(|x| x.as_u64()) {
        parts.push(format!("候选: {n}"));
    }
    parts.join(" | ")
}

async fn run_input<R: Runtime>(
    client: &mut CdpClient,
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    mut ctx: Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let script_args = super::scripts::format_input_script_args(args);

    let extra_ctx = script_extra_ctx(&ctx);
    let cancel_flag = ctx.as_ref().and_then(|c| c.cancel.clone());
    let cancel = cancel_flag.as_deref();
    check_cancel(cancel)?;

    let hit =
        eval_user_script_hit(client, cmd, &script_args, session, extra_ctx, cancel).await?;
    let mut v = hit.value.clone();
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("输入脚本失败")
            .to_string());
    }

    remember_frame_hit(&mut ctx, &hit);

    // 把命中 frame 的 targetId 写入焦点锚点，供后续「按回车」等跨步骤恢复
    if let Some(obj) = v.get_mut("focusAnchor").and_then(|x| x.as_object_mut()) {
        obj.insert("targetId".into(), serde_json::json!(hit.target_id));
    }

    if let Some(focus) = v.get("focusAnchor") {
        if let Some(c) = ctx.as_mut() {
            c.focus.set_from_value(focus);
        }
    }

    if let Some(text) = v.get("needInsertText").and_then(|x| x.as_str()) {
        if !text.is_empty() {
            frame_eval::insert_text_on_hit(client, &hit, text).await?;
            if let Some(anchor) = ctx.as_ref().and_then(|c| c.focus.last.clone()) {
                let refocus = focus_context::refocus_from_anchor(client, &anchor).await?;
                if refocus.get("ok").and_then(|x| x.as_bool()) != Some(true) {
                    let err = refocus
                        .get("error")
                        .and_then(|x| x.as_str())
                        .unwrap_or("refocus 失败");
                    return Err(format!("输入后无法恢复焦点: {err}"));
                }
                let _ = focus_context::sync_controlled_input_value_on(
                    client,
                    anchor.target_id(),
                    text,
                )
                .await;
            }
        }
    }

    let mut base = v
        .get("message")
        .and_then(|x| x.as_str())
        .unwrap_or("已输入")
        .to_string();
    if let Ok(active) =
        focus_context::describe_active_element_on(client, Some(hit.target_id.as_str())).await
    {
        base = format!("{base} | 焦点: {active}");
    }
    if !hit.is_parent {
        base = format!("{base} | frame: iframe");
    }
    let matched = v.get("matched").and_then(|x| x.as_str());
    let msg = if let Some(m) = matched {
        format!("{base} → {m}")
    } else {
        base
    };
    Ok(apply_script_result(v, session, step_index, &msg))
}

async fn run_login<R: Runtime>(
    client: &mut CdpClient,
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    cancel: Option<&AtomicBool>,
    ctx: Option<&CommandContext<'_, R>>,
) -> Result<String, String> {
    check_cancel(cancel)?;
    let system = crate::web_systems::find_web_system_by_query(args)?;
    let login_url = crate::web_systems::login_url_for(&system);

    let closed_old = recycle_current_tab(client, Some(&login_url), cancel)
        .await
        .map_err(|e| format!("打开登录页失败: {e}"))?;
    check_cancel(cancel)?;

    let credentials = serde_json::json!({
        "systemId": system.id,
        "systemName": system.name,
        "url": system.url,
        "loginUrl": login_url,
        "username": system.username,
        "password": system.password,
    });

    let v = eval_user_script(
        client,
        &cmd.script,
        args,
        session,
        Some(serde_json::json!({ "credentials": credentials })),
    )
    .await?;

    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("登录脚本失败")
            .to_string());
    }

    let already_in = v.get("alreadyLoggedIn").and_then(|x| x.as_bool()) == Some(true)
        || v.get("skipped").and_then(|x| x.as_bool()) == Some(true);

    client.skip_next_captcha = already_in;

    if !already_in {
        // 表单内嵌图形验证码（如旧 MES）须先填码再点登录；检测成功则跳过预点击，
        // 由执行器 captcha::resolve_if_present(retry_login) 填码后点击登录。
        // 滑块通常在点击登录后出现，此处仍先点击。
        let inline_captcha = crate::captcha::page_has_captcha(client)
            .await
            .unwrap_or(false);
        if !inline_captcha {
            dispatch_script_click(client, &v).await?;
            tokio::time::sleep(Duration::from_millis(1200)).await;
        }
    }

    let base = v
        .get("message")
        .and_then(|x| x.as_str())
        .unwrap_or(if already_in {
            "已处于登录态"
        } else {
            "已登录"
        });
    let tab_note = if closed_old { " | 已关闭旧标签" } else { "" };
    maybe_wait_feishu_qr_login(ctx, client, cancel, &login_url).await?;
    let detail = if already_in {
        format!("{base} | 系统: {} | {}{tab_note}", system.name, login_url)
    } else {
        format!(
            "{base} | 系统: {} | 账号: {} | {}{tab_note}",
            system.name, system.username, login_url
        )
    };
    Ok(apply_script_result(v, session, step_index, &detail))
}

async fn run_js_only<R: Runtime>(
    client: &mut CdpClient,
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    mut ctx: Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let extra_ctx = script_extra_ctx(&ctx);
    let cancel_flag = ctx.as_ref().and_then(|c| c.cancel.clone());
    let cancel = cancel_flag.as_deref();
    check_cancel(cancel)?;

    let hit = eval_user_script_hit(client, cmd, args, session, extra_ctx, cancel).await?;
    let v = hit.value;
    if let Some(d) = v.get("delegate").and_then(|x| x.as_str()) {
        return match d {
            "rust_open" => run_open(client, args, cancel, ctx.as_ref()).await,
            "rust_wait" => run_wait(args, cancel).await,
            "rust_new_tab" => run_new_tab(client, args, session, cancel, ctx.as_ref()).await,
            "rust_close_tab" => run_close_tab(client, cancel).await,
            "rust_screenshot" => run_screenshot(client, session, step_index, args).await,
            "rust_collect_page" => run_collect_page(client, session, step_index, args).await,
            "rust_ai_analyze" => run_ai_analyze(session, step_index, args, cancel, ctx.as_ref()).await,
            "rust_wait_download" => run_wait_download(client, &v, session, ctx).await,
            "rust_tab" => run_tab(client, args, cancel).await,
            "rust_clipboard" => run_clipboard(client, args, cancel).await,
            "rust_press_key" => {
                let focus_ref = ctx.as_mut().map(|c| &mut *c.focus);
                run_press_key(client, &v, focus_ref).await
            }
            // 兼容旧 commands.json 误存为 java_script 的飞书 stub
            "feishu_bitable_create"
            | "feishu_bitable_update"
            | "feishu_bitable_delete"
            | "feishu_bitable_query"
            | "feishu_bitable_sync" => {
                run_feishu_bitable(client, cmd, args, session, ctx).await
            }
            "feishu_drive_upload" | "rust_upload" => {
                if d == "feishu_drive_upload" {
                    run_feishu_drive(client, args, session).await
                } else {
                    run_upload(client, args, session, ctx).await
                }
            }
            "feishu_push" => {
                run_feishu_push(cmd, args, session, step_index, cancel, ctx.as_ref()).await
            }
            _ => Err(format!("未知 delegate: {d}")),
        };
    }
    if let Some(session) = session {
        let _ = session.process_value(&v, step_index)?;
    }
    if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        let msg = v
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("完成")
            .to_string();
        return Ok(apply_script_result(v, session, step_index, &msg));
    }
    Err(v
        .get("error")
        .and_then(|x| x.as_str())
        .unwrap_or("脚本执行失败")
        .to_string())
}

fn apply_script_result(
    v: Value,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    base_msg: &str,
) -> String {
    if let Some(session) = session {
        if let Ok(added) = session.process_value(&v, step_index) {
            if let Some(last) = added.last() {
                return format!("{base_msg} | 产物: {}", last.name);
            }
        }
    }
    base_msg.to_string()
}

async fn run_press_key(
    client: &mut CdpClient,
    v: &Value,
    mut focus: Option<&mut FocusContext>,
) -> Result<String, String> {
    let key = v
        .get("key")
        .and_then(|x| x.as_str())
        .ok_or("按键脚本未返回 key")?;
    let code = v.get("code").and_then(|x| x.as_str()).unwrap_or(key);
    let key_code = v
        .get("keyCode")
        .and_then(|x| x.as_u64())
        .unwrap_or(0) as u32;
    let text = v.get("text").and_then(|x| x.as_str());
    let repeat = v
        .get("repeat")
        .and_then(|x| x.as_u64())
        .unwrap_or(1)
        .clamp(1, 20) as u32;

    if let Some(fc) = focus.as_mut() {
        if let Some(anchor_val) = v.get("focusAnchor") {
            fc.set_from_value(anchor_val);
        }
    }

    let anchor = focus.as_ref().and_then(|f| f.last.clone());
    let mut focus_note = String::new();
    if let Some(ref anchor) = anchor {
        match focus_context::refocus_from_anchor(client, anchor).await {
            Ok(r) => {
                if r.get("ok").and_then(|x| x.as_bool()) == Some(true) {
                    let summary = r
                        .get("summary")
                        .and_then(|x| x.as_str())
                        .unwrap_or("?");
                    focus_note = format!("焦点: {summary}");
                } else {
                    let err = r
                        .get("error")
                        .and_then(|x| x.as_str())
                        .unwrap_or("refocus 失败");
                    focus_note = format!("焦点恢复失败: {err}");
                }
            }
            Err(e) => focus_note = format!("焦点恢复失败: {e}"),
        }
    } else if key == "Enter" {
        focus_note = "焦点: 无锚点（使用当前 activeElement）".into();
    }

    // OOPIF：按键必须发到焦点所在的 iframe target
    let key_target_id = anchor.as_ref().and_then(|a| a.target_id()).map(|s| s.to_string());
    let key_client_owned = match key_target_id.as_deref() {
        Some(tid) if tid != client.target_id => {
            Some(CdpClient::connect_to_any_target_on(client.port, tid).await?)
        }
        _ => None,
    };
    let key_client: &CdpClient = key_client_owned.as_ref().unwrap_or(client);

    let before_url = key_client
        .evaluate("location.href")
        .await
        .ok()
        .and_then(|x| x.as_str().map(String::from))
        .unwrap_or_default();

    for _ in 0..repeat {
        key_client
            .dispatch_key_event(key, code, key_code, text)
            .await?;
    }

    let mut msg = v
        .get("message")
        .and_then(|x| x.as_str())
        .unwrap_or("已按键")
        .to_string();
    if !focus_note.is_empty() {
        msg = format!("{msg} | {focus_note}");
    }

    if key == "Enter" {
        tokio::time::sleep(Duration::from_millis(450)).await;
        let mut after_url = key_client
            .evaluate("location.href")
            .await
            .ok()
            .and_then(|x| x.as_str().map(String::from))
            .unwrap_or_default();

        if after_url == before_url {
            let submit_tid = key_target_id.as_deref();
            if let Ok(submit) =
                focus_context::try_generic_submit_on(client, submit_tid).await
            {
                if submit.get("ok").and_then(|x| x.as_bool()) == Some(true) {
                    let method = submit
                        .get("method")
                        .and_then(|x| x.as_str())
                        .unwrap_or("submit");
                    if method == "click" {
                        if let (Some(x), Some(y)) = (
                            submit.get("x").and_then(|x| x.as_f64()),
                            submit.get("y").and_then(|x| x.as_f64()),
                        ) {
                            frame_eval::click_at_on_target(client, submit_tid, x, y).await?;
                            let label = submit
                                .get("label")
                                .and_then(|x| x.as_str())
                                .unwrap_or("提交");
                            msg = format!("{msg} | DOM回退: 点击 {label}");
                        }
                    } else {
                        msg = format!("{msg} | DOM回退: {method}");
                    }
                    tokio::time::sleep(Duration::from_millis(450)).await;
                    after_url = key_client
                        .evaluate("location.href")
                        .await
                        .ok()
                        .and_then(|x| x.as_str().map(String::from))
                        .unwrap_or_default();
                }
            }
        }

        if after_url == before_url {
            msg = format!(
                "{msg} | Enter 未触发跳转，可尝试 点击按钮#N/图标按钮#N"
            );
        }
    }

    Ok(msg)
}

async fn run_screenshot(
    client: &mut CdpClient,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    args: &str,
) -> Result<String, String> {
    let session = session.ok_or("当前运行未启用产物目录")?;
    let png = client.capture_screenshot_png().await?;
    let label = if args.trim().is_empty() {
        "manual"
    } else {
        args.trim()
    };
    let idx = step_index.unwrap_or(0);
    let art = session.save_step_screenshot(&png, idx, label)?;
    Ok(format!("已截图 → {}", art.name))
}

async fn run_collect_page(
    client: &mut CdpClient,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    args: &str,
) -> Result<String, String> {
    let session = session.ok_or("当前运行未启用产物目录")?;
    let scope = crate::page_collect::parse_collect_args(args)?;
    // 活动标签跟随已由 run_command → should_follow_active_tab 统一处理
    let _ = client.sync_page_meta().await;

    // 当前页为飞书多维表格且需要表格时：走开放平台读表，写入「表格」分类
    if scope.tables {
        let page_url = client.target_url.trim().to_string();
        if !page_url.is_empty() && crate::feishu::parse_bitable_url(&page_url).is_ok() {
            return run_collect_feishu_bitable(client, &session, step_index, &page_url).await;
        }
    }

    let (md, total) = crate::page_collect::collect_page_markdown_scoped(client, scope).await?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let idx = step_index.unwrap_or(0);
    let slug = scope
        .file_slug()
        .map(|s| format!("-{s}"))
        .unwrap_or_default();
    let file_name = if idx > 0 {
        format!("页面采集{slug}-step-{idx:02}-{stamp}.md")
    } else {
        format!("页面采集{slug}-{stamp}.md")
    };
    let art = session
        .save_text_document(&file_name, &md, step_index)
        .map_err(|e| e.to_string())?;
    let range = scope.display_label();
    let tab = if client.target_title.is_empty() {
        client.target_url.clone()
    } else {
        client.target_title.clone()
    };
    Ok(format!(
        "已采集{range} {total} 条 | 「{tab}」| 报告：documents/{}，可在运行产物中预览",
        art.name
    ))
}

async fn run_collect_feishu_bitable(
    client: &CdpClient,
    session: &Arc<RunSession>,
    step_index: Option<u32>,
    page_url: &str,
) -> Result<String, String> {
    let table = crate::feishu::resolve_for_command(Some(page_url), None)?;
    let empty_filter = std::collections::HashMap::<String, String>::new();
    let records =
        crate::feishu::search_records(&table.app_token, &table.table_id, &empty_filter).await?;
    let column_order =
        crate::feishu::table_field_order(&table.app_token, &table.table_id).await?;
    let (headers, rows) = crate::feishu::records_to_plain_table(&records, &column_order);

    let url = client.target_url.clone();
    let title = if client.target_title.is_empty() {
        table.name.clone()
    } else {
        client.target_title.clone()
    };
    let (md, total) = crate::page_collect::collect_markdown_from_plain_table(
        &url,
        &title,
        &table.name,
        headers,
        rows,
    );

    let scope = crate::page_collect::CollectScope::tables_only();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let idx = step_index.unwrap_or(0);
    let slug = scope
        .file_slug()
        .map(|s| format!("-{s}"))
        .unwrap_or_default();
    let file_name = if idx > 0 {
        format!("页面采集{slug}-step-{idx:02}-{stamp}.md")
    } else {
        format!("页面采集{slug}-{stamp}.md")
    };
    let art = session
        .save_text_document(&file_name, &md, step_index)
        .map_err(|e| e.to_string())?;
    let tab = if title.is_empty() { url } else { title };
    Ok(format!(
        "已采集表格 {total} 条 | 「{tab}」| 报告：documents/{}，可在运行产物中预览",
        art.name
    ))
}

async fn run_ai_analyze<R: Runtime>(
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    args: &str,
    cancel: Option<&AtomicBool>,
    ctx: Option<&CommandContext<'_, R>>,
) -> Result<String, String> {
    let session = session.ok_or("AI 分析需要运行产物目录")?;
    check_cancel(cancel)?;
    let (report, summary, answer) =
        crate::ai_analyze::analyze_run_artifacts(session, args, cancel).await?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let idx = step_index.unwrap_or(0);
    let file_name = if idx > 0 {
        format!("AI分析-step-{idx:02}-{stamp}.md")
    } else {
        format!("AI分析-{stamp}.md")
    };
    let art = session
        .save_text_document(&file_name, &report, step_index)
        .map_err(|e| e.to_string())?;

    // 正文同步到客户端（不含报告标题 / 参考文件 / Token 统计）
    let answer = answer.trim();
    if !answer.is_empty() {
        if crate::run_sink::worker_mode() {
            crate::run_sink::emit_log(format!("⟦AI分析⟧\n{answer}\n⟦/AI分析⟧"));
        } else if let Some(app) = ctx.and_then(|c| c.app) {
            let task_id = ctx.and_then(|c| c.task_id).unwrap_or("main");
            let payload = serde_json::json!({
                "content": answer,
                "document": art.name,
                "taskId": task_id,
            });
            let _ = app.emit("ai-analyze-result", &payload);
            if task_id != "main" {
                let label = crate::task_run::window_label(task_id);
                if let Some(win) = app.get_webview_window(&label) {
                    let _ = win.emit("ai-analyze-result", &payload);
                }
                // 写入任务日志，便于结束后同步到主界面历史对话
                if let Some(state) = app.try_state::<crate::AppState>() {
                    if let Some(ctrl) = state.task_registry.get(task_id) {
                        crate::run_user_log::log_user(
                            &ctrl,
                            state.inner(),
                            app,
                            format!("⟦AI分析⟧\n{answer}\n⟦/AI分析⟧"),
                        );
                    }
                }
            }
        }
    }

    Ok(format!(
        "{summary} | 报告：documents/{}，可在运行产物中预览",
        art.name
    ))
}

async fn run_wait_download_files_only<R: Runtime>(
    v: &Value,
    session: Option<&Arc<RunSession>>,
    cancel: Option<&AtomicBool>,
    ctx: Option<&CommandContext<'_, R>>,
) -> Result<String, String> {
    let timeout_secs = v
        .get("timeoutSecs")
        .and_then(|x| x.as_u64())
        .unwrap_or(200)
        .max(1);
    let base_msg = v
        .get("message")
        .and_then(|x| x.as_str())
        .unwrap_or("等待下载");
    let session = session.ok_or("下载等待需要运行产物目录")?;
    let until = Instant::now() + Duration::from_secs(timeout_secs);
    session.set_auto_fetch_armed(true);
    session.append_download_trace("「等待下载」开始（无浏览器，仅轮询产物目录）");
    let _fetch_guard = client_download::AutoFetchWaitGuard::enter(session);

    while Instant::now() < until {
        check_cancel(cancel)?;
        let pending = session.poll_wait_download_ready();
        if !pending.is_empty() {
            let names: Vec<_> = pending.iter().map(|a| a.name.as_str()).collect();
            session.ack_wait_downloads(&pending);
            return Ok(format!("{base_msg} | 已下载: {}", names.join(", ")));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let _ = ctx;
    Err(format!(
        "已等待 {timeout_secs} 秒（未出现新的下载文件；当前无浏览器连接）"
    ))
}

async fn run_wait_download<R: Runtime>(
    client: &CdpClient,
    v: &Value,
    session: Option<&Arc<RunSession>>,
    ctx: Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let timeout_secs = v
        .get("timeoutSecs")
        .and_then(|x| x.as_u64())
        .unwrap_or(200)
        .max(1);
    let base_msg = v
        .get("message")
        .and_then(|x| x.as_str())
        .unwrap_or("等待下载");
    let session = session.ok_or("下载等待需要运行产物目录")?;
    let cancel_flag = ctx.as_ref().and_then(|c| c.cancel.clone());
    let cancel = cancel_flag.as_deref();
    let until = Instant::now() + Duration::from_secs(timeout_secs);
    let started = Instant::now();
    let progress = progress_sink(&ctx);
    let mut last_wait_emit = Instant::now();
    let mut last_wait_pct = 0u32;

    if let Some(p) = progress.as_ref() {
        p.waiting("正在等待下载…", Some(0));
    }

    // 本步骤武装自动落盘；结束（含超时/取消）后由 AutoFetchWaitGuard 解除
    session.set_auto_fetch_armed(true);
    session.append_download_trace("「等待下载」开始：已武装自动落盘");
    let _fetch_guard = client_download::AutoFetchWaitGuard::enter(session);

    let _ = crate::browser_network::BrowserNetworkCapture::shared_for_port(client.port).await;
    let _ = client_download::ensure_automation_capture(client).await;
    let _ = crate::print_pdf::dismiss_native_print_ui_with_escape(client).await;
    let _ = crate::browser_network::arm_ephemeral_download_capture(client.port).await;
    // 点击阶段已截获但尚未拉取的 URL：从近期请求捞回队列，并立刻 process_pending 一次
    crate::browser_network::requeue_recent_download_urls(client.port, session);
    let mut last_reinject = Instant::now();
    // 首轮立刻自拉取（瞬时下载常在「点击」阶段入队，不能干等到 5s）
    let mut last_self_fetch = Instant::now() - Duration::from_secs(10);
    let mut seen_targets = HashSet::new();
    let mut last_fetch_err: Option<String> = None;

    while Instant::now() < until {
        if cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
            if let Some(p) = progress.as_ref() {
                p.hide();
            }
            if let Some(tid) = ctx.as_ref().and_then(|c| c.task_id) {
                task_progress::clear_download_progress_dedup(tid);
            }
            crate::browser_network::disarm_ephemeral_download_capture(client.port).await;
            return Err("已取消".into());
        }

        // 自动泵可能在「点击导出」时已落盘：优先消费未确认队列 / 上次确认后的新文件
        let pending = session.poll_wait_download_ready();
        if !pending.is_empty() {
            let names: Vec<_> = pending.iter().map(|a| a.name.as_str()).collect();
            session.ack_wait_downloads(&pending);
            if let Some(p) = progress.as_ref() {
                p.done(names[0]);
            }
            if let Some(tid) = ctx.as_ref().and_then(|c| c.task_id) {
                task_progress::clear_download_progress_dedup(tid);
            }
            crate::browser_network::disarm_ephemeral_download_capture(client.port).await;
            return Ok(format!("{base_msg} | 已下载: {}", names.join(", ")));
        }

        // 钩子/策略：最多每 30s 检查一次；完好则只刷新 deny，避免与自动泵双双重装脚本
        if last_reinject.elapsed() >= Duration::from_secs(30) {
            if client_download::capture_hooks_alive(client).await {
                let _ = crate::browser_network::set_download_behavior_on(client.port, true).await;
            } else {
                let _ = client_download::ensure_automation_capture(client).await;
            }
            crate::browser_network::requeue_recent_download_urls(client.port, session);
            last_reinject = Instant::now();
        }

        if let Some(p) = progress.as_ref() {
            let pct = ((started.elapsed().as_secs_f64() / timeout_secs as f64) * 95.0).min(95.0)
                as u32;
            if last_wait_emit.elapsed() >= Duration::from_millis(900)
                || pct.saturating_sub(last_wait_pct) >= 10
            {
                p.waiting("正在等待下载…", Some(pct));
                last_wait_emit = Instant::now();
                last_wait_pct = pct;
            }
        }

        // 与自动泵分工：等待循环以 poll 为主；仅每 5s 自拉取一次兜底，避免双循环 400ms 狂扫 CDP
        if last_self_fetch.elapsed() >= Duration::from_secs(5) {
            match client_download::process_pending(
                client,
                session,
                None,
                &mut seen_targets,
                progress.as_ref(),
            )
            .await
            {
                Ok(added) if !added.is_empty() => {
                    if let Some(app) = ctx.as_ref().and_then(|c| c.app) {
                        run_artifacts::emit_artifacts_update(app, session);
                    }
                    let names: Vec<_> = added.iter().map(|a| a.name.as_str()).collect();
                    session.ack_wait_downloads(&added);
                    if let Some(p) = progress.as_ref() {
                        p.done(names[0]);
                    }
                    if let Some(tid) = ctx.as_ref().and_then(|c| c.task_id) {
                        task_progress::clear_download_progress_dedup(tid);
                    }
                    crate::browser_network::disarm_ephemeral_download_capture(client.port).await;
                    return Ok(format!("{base_msg} | 已下载: {}", names.join(", ")));
                }
                Err(e) => last_fetch_err = Some(e),
                _ => {}
            }
            last_self_fetch = Instant::now();
        }

        let _ = session.sync_downloads();
        let pending = session.poll_wait_download_ready();
        if !pending.is_empty() {
            let names: Vec<_> = pending.iter().map(|a| a.name.as_str()).collect();
            session.ack_wait_downloads(&pending);
            if let Some(p) = progress.as_ref() {
                p.done(names[0]);
            }
            if let Some(tid) = ctx.as_ref().and_then(|c| c.task_id) {
                task_progress::clear_download_progress_dedup(tid);
            }
            crate::browser_network::disarm_ephemeral_download_capture(client.port).await;
            return Ok(format!("{base_msg} | 已下载: {}", names.join(", ")));
        }

        tokio::time::sleep(Duration::from_millis(800)).await;
    }

    if let Some(p) = progress.as_ref() {
        if last_fetch_err.is_some() {
            p.error("下载失败");
        } else {
            p.hide();
        }
    }
    if let Some(tid) = ctx.as_ref().and_then(|c| c.task_id) {
        task_progress::clear_download_progress_dedup(tid);
    }

    crate::browser_network::disarm_ephemeral_download_capture(client.port).await;

    let diag = client_download::download_diagnostic(client).await;
    let pending_n = session.pending_wait_download_count();
    let mut tail = if last_fetch_err.is_some() {
        format!("捕获到下载 URL 但拉取失败 | pending={pending_n} | {diag}")
    } else {
        format!("已等待 {timeout_secs} 秒（未出现新的下载文件）| pending={pending_n} | {diag}")
    };
    if let Some(e) = last_fetch_err {
        tail.push_str(&format!(" | {e}"));
    }
    Err(format!("{base_msg} | {tail}"))
}

fn progress_sink<'a, R: Runtime>(
    ctx: &'a Option<CommandContext<'a, R>>,
) -> Option<DownloadProgressSink<'a, R>> {
    let c = ctx.as_ref()?;
    let app = c.app?;
    let task_id = c.task_id?;
    Some(DownloadProgressSink::new(app, task_id))
}

fn session_bits<'a>(
    session: Option<&'a Arc<RunSession>>,
) -> Option<(String, String, String)> {
    let s = session?;
    Some((
        s.run_dir_display(),
        s.run_id.clone(),
        s.run_dir.join("downloads").to_string_lossy().into_owned(),
    ))
}

async fn eval_user_script(
    client: &CdpClient,
    script_body: &str,
    args: &str,
    session: Option<&Arc<RunSession>>,
    extra_ctx: Option<Value>,
) -> Result<Value, String> {
    let bits = session_bits(session);
    let bits_ref = bits
        .as_ref()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()));
    let expr = frame_eval::build_script_expr(script_body, args, bits_ref, extra_ctx.as_ref())?;
    let v = client.evaluate(&expr).await?;
    if v.is_null() {
        return Err("脚本返回 null".into());
    }
    Ok(v)
}

/// 按能力关键字决定是否扇出到 OOPIF；命中后返回 value + target_id
async fn eval_user_script_hit(
    client: &CdpClient,
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    extra_ctx: Option<Value>,
    cancel: Option<&AtomicBool>,
) -> Result<ScriptEvalHit, String> {
    check_cancel(cancel)?;
    let bits = session_bits(session);
    let bits_ref = bits
        .as_ref()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()));
    let hot = frame_hot_from_extra(extra_ctx.as_ref());
    let or_probe = extra_ctx
        .as_ref()
        .and_then(|v| v.get("orProbe"))
        .and_then(|x| x.as_bool())
        == Some(true);

    // 单段结构化目标（图标按钮#15）：按探针连续编号定位到具体 frame，再改写为本地序号执行
    if let Some((type_name, global_index, args_template)) =
        remap_global_structural_args(&cmd.keyword, args)
    {
        let owner = frame_eval::locate_global_structural_with_hot(
            client,
            &type_name,
            global_index,
            &hot,
            cancel,
        )
        .await?;
        let local_args =
            args_template.replace("__LOCAL__", &owner.local_index.to_string());
        // 已知 owner 后缩短元素等待（热 sticky 更短）
        let mut owner_ctx = extra_ctx.clone().unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = owner_ctx.as_object_mut() {
            if !or_probe && !obj.contains_key("elementWaitTimeout") {
                let secs = if hot.preferred_warm
                    && hot
                        .preferred_oopif
                        .as_deref()
                        .is_some_and(|p| p == owner.target_id)
                {
                    frame_eval::STICKY_ELEMENT_WAIT_SECS
                } else {
                    5.0
                };
                obj.insert("elementWaitTimeout".into(), serde_json::json!(secs));
            }
        }
        let expr =
            frame_eval::build_script_expr(&cmd.script, &local_args, bits_ref, Some(&owner_ctx))?;
        let eval_timeout = if or_probe {
            Duration::from_secs(8)
        } else if hot.preferred_warm
            && hot
                .preferred_oopif
                .as_deref()
                .is_some_and(|p| p == owner.target_id)
        {
            frame_eval::STICKY_TRY_EVAL_TIMEOUT
        } else {
            match cmd.keyword.as_str() {
                "点击" | "输入" | "按" => Duration::from_secs(10),
                "等待" => wait_script_cdp_timeout(args),
                _ => Duration::from_secs(30),
            }
        };
        return frame_eval::eval_on_structural_owner(
            client,
            &owner,
            &expr,
            cancel,
            eval_timeout,
        )
        .await;
    }

    // 点击文字 / 点击文字#N：与文字探针相同——跨 frame 按得分合成连续 #N，再落到本地序号执行
    if cmd.keyword == "点击" {
        if let Some((query, global_index)) = super::scripts::parse_click_text_args(args) {
            let owner = frame_eval::locate_global_click_text_with_hot(
                client,
                &query,
                global_index,
                &hot,
                cancel,
            )
            .await?;
            let local_args = if owner.local_index <= 1 {
                query.clone()
            } else {
                format!("{}#{}", query, owner.local_index)
            };
            let mut owner_ctx = extra_ctx.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Some(obj) = owner_ctx.as_object_mut() {
                if !or_probe && !obj.contains_key("elementWaitTimeout") {
                    let secs = if hot.preferred_warm
                        && hot
                            .preferred_oopif
                            .as_deref()
                            .is_some_and(|p| p == owner.target_id)
                    {
                        frame_eval::STICKY_ELEMENT_WAIT_SECS
                    } else {
                        5.0
                    };
                    obj.insert("elementWaitTimeout".into(), serde_json::json!(secs));
                }
            }
            let expr =
                frame_eval::build_script_expr(&cmd.script, &local_args, bits_ref, Some(&owner_ctx))?;
            let eval_timeout = if or_probe {
                Duration::from_secs(8)
            } else if hot.preferred_warm
                && hot
                    .preferred_oopif
                    .as_deref()
                    .is_some_and(|p| p == owner.target_id)
            {
                frame_eval::STICKY_TRY_EVAL_TIMEOUT
            } else {
                Duration::from_secs(10)
            };
            return frame_eval::eval_on_structural_owner(
                client,
                &owner,
                &expr,
                cancel,
                eval_timeout,
            )
            .await;
        }
    }

    // 仅当 FrameTree / OOPIF / 同源子 ctx / sticky 任一存在时扇出。
    // 注意：有同源 ctx ≠ 已扫到跨域业务 iframe；发现为空且 FrameTree 有子节点时由
    // eval_across_frames 内部 Peek→Wait 补齐 OOPIF。
    let tree_has_children = client.frame_tree_has_child_frames().await;
    let oopif_peek = chrome::discover_oopif_targets_for_parent(
        client.port,
        &client.target_id,
        Some(client),
        chrome::DiscoverOopifMode::Peek,
    )
    .await;
    let has_same_origin_child = !client.child_frame_context_ids().await.is_empty();
    let use_fanout = frame_eval::should_fanout_frames(&cmd.keyword, args)
        && (tree_has_children
            || !oopif_peek.is_empty()
            || has_same_origin_child
            || hot.preferred_oopif.is_some());

    if use_fanout {
        let mut fanout_ctx = extra_ctx.clone().unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = fanout_ctx.as_object_mut() {
            obj.insert("frameFanout".into(), serde_json::json!(true));
        }
        // 热路径短探：父页与 sticky iframe 竞速，谁先命中用谁（避免 join 空等慢的一侧）
        let mut early_expr: Option<String> = None;
        if !or_probe {
            let mut short_ctx = fanout_ctx.clone();
            if let Some(obj) = short_ctx.as_object_mut() {
                obj.insert(
                    "elementWaitTimeout".into(),
                    serde_json::json!(frame_eval::STICKY_ELEMENT_WAIT_SECS),
                );
            }
            if let Ok(short_expr) =
                frame_eval::build_script_expr(&cmd.script, args, bits_ref, Some(&short_ctx))
            {
                early_expr = Some(short_expr.clone());
                let pref = hot.preferred_oopif.clone();
                let warm = hot.preferred_warm;
                let prefer_parent = hot.prefer_parent;
                let has_sticky = pref.is_some();
                // 已有子 frame / OOPIF：跳过「仅父页」5s 短探，否则目标在 iframe 时白等一轮
                let skip_parent_only_short =
                    has_same_origin_child || tree_has_children || !oopif_peek.is_empty();

                // iframe 短探用更短等待（元素已在 DOM 时立刻命中；未命中交给全量 15s）
                if skip_parent_only_short {
                    if let Some(obj) = short_ctx.as_object_mut() {
                        obj.insert("elementWaitTimeout".into(), serde_json::json!(1.2));
                    }
                    if let Ok(child_early) = frame_eval::build_script_expr(
                        &cmd.script,
                        args,
                        bits_ref,
                        Some(&short_ctx),
                    ) {
                        early_expr = Some(child_early);
                    }
                }
                // 连续主页：只打父页短探，绝不先空等 sticky
                if prefer_parent {
                    if let Ok(hit) =
                        frame_eval::try_eval_on_parent_short(client, &short_expr, cancel).await
                    {
                        return Ok(hit);
                    }
                } else if has_sticky {
                    // 刚从 iframe 出来 / 亲和 iframe：两侧竞速，父页命中可立即取消 sticky 空等
                    let parent_fut =
                        frame_eval::try_eval_on_parent_short(client, &short_expr, cancel);
                    let sticky_fut = async {
                        let p = pref.as_deref().unwrap();
                        frame_eval::try_eval_preferred_oopif(
                            client, p, &short_expr, warm, cancel,
                        )
                        .await
                    };
                    tokio::pin!(parent_fut);
                    tokio::pin!(sticky_fut);
                    tokio::select! {
                        biased;
                        Ok(hit) = &mut parent_fut => return Ok(hit),
                        Ok(hit) = &mut sticky_fut => return Ok(hit),
                        else => {}
                    }
                } else if !skip_parent_only_short {
                    if let Ok(hit) =
                        frame_eval::try_eval_on_parent_short(client, &short_expr, cancel).await
                    {
                        return Ok(hit);
                    }
                }
            }
            // 短探未中：回退扇出用缩短等待，勿再烧满 15s
            if hot.prefer_parent || hot.preferred_warm || hot.preferred_oopif.is_some() {
                if let Some(obj) = fanout_ctx.as_object_mut() {
                    obj.insert(
                        "elementWaitTimeout".into(),
                        serde_json::json!(frame_eval::WARM_FANOUT_ELEMENT_WAIT_SECS),
                    );
                }
            }
        }
        let expr =
            frame_eval::build_script_expr(&cmd.script, args, bits_ref, Some(&fanout_ctx))?;
        // 热路径已缩短 elementWait；CDP 上限仍须盖住慢 frame
        let parent_timeout = if cmd.keyword == "等待" {
            wait_script_cdp_timeout(args)
        } else if hot.prefer_parent || hot.preferred_warm || hot.preferred_oopif.is_some() {
            frame_eval::STICKY_TRY_EVAL_TIMEOUT
        } else {
            match cmd.keyword.as_str() {
                "点击" | "输入" | "按" => frame_eval::SCRIPT_WAIT_EVAL_TIMEOUT,
                _ => frame_eval::PARENT_EVAL_TIMEOUT,
            }
        };
        return frame_eval::eval_across_frames_with_hot(
            client,
            &expr,
            cancel,
            parent_timeout,
            &hot,
            early_expr.as_deref(),
        )
        .await;
    }

    let expr = frame_eval::build_script_expr(&cmd.script, args, bits_ref, extra_ctx.as_ref())?;
    // 点击/输入脚本内可轮询至 ~15s + 遮罩，须用更长 CDP 求值上限，否则会变成「CDP 超时: Runtime.evaluate」
    // 试探步仅短等，CDP 上限也收紧，避免失败后空等
    // 「等待」须覆盖参数中的秒数（旧硬编码 35s 会截断「等待xxx 200」）
    let eval_timeout = if or_probe {
        Duration::from_secs(8)
    } else {
        match cmd.keyword.as_str() {
            "点击" | "输入" | "按" => frame_eval::SCRIPT_WAIT_EVAL_TIMEOUT,
            "等待" => wait_script_cdp_timeout(args),
            _ => Duration::from_secs(30),
        }
    };
    let v = if cancel.is_some() {
        let eval = client.evaluate_in_context_with_timeout(&expr, None, eval_timeout);
        tokio::pin!(eval);
        tokio::select! {
            biased;
            _ = async {
                while !cancel.is_some_and(|f| f.load(Ordering::SeqCst)) {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
            } => return Err("已取消".into()),
            r = eval => r?,
        }
    } else {
        client
            .evaluate_in_context_with_timeout(&expr, None, eval_timeout)
            .await?
    };
    if v.is_null() {
        return Err("脚本返回 null".into());
    }
    Ok(ScriptEvalHit {
        value: v,
        target_id: client.target_id.clone(),
        is_parent: true,
        context_id: None,
    })
}

/// 将探针连续编号的单段结构化目标改写为 frame 本地序号。
/// 返回 (类型, 全局序号, args 模板，其中 `__LOCAL__` 待替换为本地序号)。
fn remap_global_structural_args(
    keyword: &str,
    args: &str,
) -> Option<(String, u32, String)> {
    match keyword {
        "点击" => {
            let (ty, global) = super::scripts::parse_single_structural_segment(args)?;
            Some((ty.clone(), global, format!("{ty}#__LOCAL__")))
        }
        "输入" => {
            // format_input_script_args → text|field
            let (text, field) = if let Some((t, f)) = args.split_once('|') {
                (t.to_string(), f.to_string())
            } else {
                super::scripts::parse_input_step_args(args)
            };
            let (ty, global) = super::scripts::parse_single_structural_segment(&field)?;
            Some((ty.clone(), global, format!("{text}|{ty}#__LOCAL__")))
        }
        _ => None,
    }
}

async fn run_feishu_bitable<R: Runtime>(
    client: &mut CdpClient,
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    _ctx: Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    // 数据读写走开放平台 API；目标表优先取浏览器当前页的 /base/{app_token}?table=...
    let _ = client.sync_page_meta().await;
    let page_url = if client.target_url.trim().is_empty() {
        None
    } else {
        Some(client.target_url.as_str())
    };

    let table = match cmd.keyword.as_str() {
        "飞书查询" => {
            let (table_name, _conditions) = crate::feishu::parse_bitable_query_args(args)?;
            crate::feishu::resolve_for_command(page_url, table_name.as_deref())?
        }
        "飞书同步" => {
            let (table_name, _source, _) = crate::feishu::parse_sync_args(args)?;
            crate::feishu::resolve_for_command(page_url, table_name.as_deref())?
        }
        _ => {
            let (table_name, fields) = crate::feishu::parse_bitable_step_args(args)?;
            if fields.is_empty() {
                return Err("至少需要提供一个字段".into());
            }
            let _ = fields;
            crate::feishu::resolve_for_command(page_url, table_name.as_deref())?
        }
    };

    match cmd.keyword.as_str() {
        "飞书新增" => feishu_do_create(&table, args).await,
        "飞书更新" => feishu_do_update(&table, args).await,
        "飞书删除" => feishu_do_delete(&table, args).await,
        "飞书查询" => feishu_do_query(&table, args, session).await,
        "飞书同步" => feishu_do_sync(&table, args, session).await,
        _ if cmd.script.contains("feishu_bitable_create") => feishu_do_create(&table, args).await,
        _ if cmd.script.contains("feishu_bitable_update") => feishu_do_update(&table, args).await,
        _ if cmd.script.contains("feishu_bitable_delete") => feishu_do_delete(&table, args).await,
        _ if cmd.script.contains("feishu_bitable_query") => feishu_do_query(&table, args, session).await,
        _ if cmd.script.contains("feishu_bitable_sync") => feishu_do_sync(&table, args, session).await,
        _ => Err(format!("暂不支持的指令: {}", cmd.keyword)),
    }
}

/// 飞书云盘上传：从当前页（或参数 文件夹=）解析 folder_token，经 Open API 上传本地附件。
async fn run_feishu_push<R: Runtime>(
    cmd: &CommandDef,
    args: &str,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    cancel: Option<&AtomicBool>,
    ctx: Option<&CommandContext<'_, R>>,
) -> Result<String, String> {
    check_cancel(cancel)?;
    match cmd.keyword.as_str() {
        "推送消息" => {
            let text = args.trim();
            if text.is_empty() {
                return Err("请写正文，例如：推送消息 任务已完成".into());
            }
            crate::feishu_push::push_text(text).await
        }
        "推送附件" => {
            let session = session.ok_or(
                "推送附件需要运行产物目录（请在快捷指令/任务中执行，并先「添加附件」到 files/）",
            )?;
            let queries = crate::feishu_push::split_attachment_queries(args);
            if queries.is_empty() {
                let path = resolve_upload_file(&session.run_dir, None)?;
                let msg = crate::feishu_push::push_file_path(&path).await?;
                return Ok(format!(
                    "{msg} | {}",
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("file")
                ));
            }
            let mut parts = Vec::new();
            for q in queries {
                check_cancel(cancel)?;
                let path = resolve_upload_file(&session.run_dir, Some(&q))?;
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string();
                let msg = crate::feishu_push::push_file_path(&path).await?;
                parts.push(format!("{name}: {msg}"));
            }
            Ok(format!("已推送 {} 个附件 | {}", parts.len(), parts.join("；")))
        }
        "推送AI" => {
            let session = session.ok_or("推送AI 需要运行产物目录")?;
            let (report, summary, answer) =
                crate::ai_analyze::analyze_run_artifacts(session, args, cancel).await?;
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let idx = step_index.unwrap_or(0);
            let file_name = if idx > 0 {
                format!("AI分析-step-{idx:02}-{stamp}.md")
            } else {
                format!("AI分析-{stamp}.md")
            };
            let art = session
                .save_text_document(&file_name, &report, step_index)
                .map_err(|e| e.to_string())?;

            let answer = answer.trim();
            if answer.is_empty() {
                return Err("AI 未生成可推送的回答".into());
            }
            // 飞书单条消息有长度限制，截断到约 4000 字
            let push_body = if answer.chars().count() > 3800 {
                let truncated: String = answer.chars().take(3800).collect();
                format!("{truncated}\n\n…（内容过长已截断，完整报告见 documents/{}）", art.name)
            } else {
                answer.to_string()
            };
            let push_msg = crate::feishu_push::push_text(&push_body).await?;

            if let Some(app) = ctx.and_then(|c| c.app) {
                let task_id = ctx.and_then(|c| c.task_id).unwrap_or("main");
                let payload = serde_json::json!({
                    "content": answer,
                    "document": art.name,
                    "taskId": task_id,
                });
                let _ = app.emit("ai-analyze-result", &payload);
            }

            Ok(format!(
                "{summary} | {push_msg} | 报告：documents/{}",
                art.name
            ))
        }
        other => Err(format!("未知推送指令：{other}")),
    }
}

async fn run_feishu_drive(
    client: &mut CdpClient,
    args: &str,
    session: Option<&Arc<RunSession>>,
) -> Result<String, String> {
    let session = session.ok_or(
        "飞书上传需要运行产物目录（请在快捷指令/任务中执行，并先「添加附件」到 files/）",
    )?;
    let (source, _button, folder_override) = parse_upload_args(args)?;
    let path = resolve_upload_file(&session.run_dir, source.as_deref())?;
    let templates_dir = crate::templates::templates_dir();
    let display_rel = if let Ok(rel) = path.strip_prefix(&session.run_dir) {
        rel.to_string_lossy().replace('\\', "/")
    } else if let Ok(rel) = path.strip_prefix(&templates_dir) {
        format!("templates/{}", rel.to_string_lossy().replace('\\', "/"))
    } else {
        path.display().to_string()
    };

    let folder_raw = if let Some(f) = folder_override {
        f
    } else {
        let _ = client.sync_page_meta().await;
        let url = client.target_url.trim();
        if url.is_empty() {
            return Err(
                "无法取得当前页地址。请先打开飞书云盘文件夹，或写 飞书上传 …,文件夹=https://…/drive/folder/…"
                    .into(),
            );
        }
        url.to_string()
    };
    let folder_token = crate::feishu::parse_drive_folder_token(&folder_raw)?;

    let uploaded =
        crate::feishu::upload_file_to_folder(&folder_token, &path).await?;

    Ok(format!(
        "已上传到云盘「{}」→ {}（{}，{} 字节）| file_token={}",
        uploaded.folder_token,
        uploaded.file_name,
        display_rel,
        uploaded.size,
        uploaded.file_token
    ))
}

async fn feishu_do_create(
    table: &crate::feishu::BitableTable,
    args: &str,
) -> Result<String, String> {
    let (_, fields) = crate::feishu::parse_bitable_step_args(args)?;
    let record_id =
        crate::feishu::create_record(&table.app_token, &table.table_id, &fields).await?;
    Ok(format!(
        "已在「{}」新增记录 {record_id}（{}）| table={}",
        table.name,
        crate::feishu::field_summary(&fields),
        table.table_id
    ))
}

async fn feishu_do_update(
    table: &crate::feishu::BitableTable,
    args: &str,
) -> Result<String, String> {
    let (_, fields) = crate::feishu::parse_bitable_step_args(args)?;
    let (record_id, created) = crate::feishu::upsert_record(
        &table.app_token,
        &table.table_id,
        &table.unique_keys,
        &fields,
    )
    .await
    .map_err(|e| {
        if table.unique_keys.is_empty() {
            format!(
                "{e}\n提示：「飞书更新」需要唯一键。可在指令中写 @项目提报台账 以使用内置唯一键（项目名称+归属月份+项目类别），或先登记该表的唯一键。"
            )
        } else {
            e
        }
    })?;
    let action = if created { "新增" } else { "更新" };
    Ok(format!(
        "已在「{}」{action}记录 {record_id}（{}）",
        table.name,
        crate::feishu::field_summary(&fields)
    ))
}

async fn feishu_do_delete(
    table: &crate::feishu::BitableTable,
    args: &str,
) -> Result<String, String> {
    let (_, fields) = crate::feishu::parse_bitable_step_args(args)?;
    let records =
        crate::feishu::search_records(&table.app_token, &table.table_id, &fields).await?;
    if records.is_empty() {
        return Err(format!(
            "未找到匹配记录（{}）",
            crate::feishu::field_summary(&fields)
        ));
    }
    let ids: Vec<String> = records.into_iter().map(|(id, _)| id).collect();
    let n = crate::feishu::delete_records(&table.app_token, &table.table_id, &ids).await?;
    Ok(format!(
        "已从「{}」删除 {n} 条记录（条件：{}）",
        table.name,
        crate::feishu::field_summary(&fields)
    ))
}

async fn feishu_do_query(
    table: &crate::feishu::BitableTable,
    args: &str,
    session: Option<&Arc<RunSession>>,
) -> Result<String, String> {
    let session = session.ok_or("飞书查询需要运行产物目录（请在快捷指令/任务中执行）")?;
    let (_, conditions) = crate::feishu::parse_bitable_query_args(args)?;
    let records = crate::feishu::search_records_with_query(
        &table.app_token,
        &table.table_id,
        &conditions,
    )
    .await?;
    let column_order =
        crate::feishu::table_field_order(&table.app_token, &table.table_id).await?;

    let md = crate::feishu::format_records_markdown(
        &table.name,
        &conditions,
        &records,
        &column_order,
    );
    let safe_name: String = table
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file_name =
        run_artifacts::stamp_document_filename(&format!("飞书查询-{safe_name}.md"));
    let art = session
        .save_text_document(&file_name, &md, None)
        .map_err(|e| e.to_string())?;

    let cond = if conditions.is_empty() {
        "全部".to_string()
    } else {
        crate::feishu::query_conditions_summary(&conditions)
    };
    Ok(format!(
        "已从「{}」查询到 {} 条记录（条件：{}）| 报告：documents/{}，可在运行产物中预览",
        table.name,
        records.len(),
        cond,
        art.name
    ))
}

async fn feishu_do_sync(
    table: &crate::feishu::BitableTable,
    args: &str,
    session: Option<&Arc<RunSession>>,
) -> Result<String, String> {
    let session = session.ok_or("飞书同步需要运行产物目录（请在快捷指令/任务中执行）")?;
    let (_, source_rel, defaults) = crate::feishu::parse_sync_args(args)?;

    let source_path = resolve_run_file_path(&session.run_dir, &source_rel)?;
    if !source_path.is_file() {
        return Err(format!("找不到同步文件: {}", source_path.display()));
    }

    let (_, mut rows) = crate::feishu::read_excel_rows(&source_path)?;
    for row in &mut rows {
        for (k, v) in &defaults {
            row.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    let sync = crate::feishu::sync_records(
        &table.app_token,
        &table.table_id,
        &table.unique_keys,
        &rows,
    )
    .await?;

    for err in sync.errors.iter().take(8) {
        eprintln!("飞书同步: {err}");
    }
    if sync.errors.len() > 8 {
        eprintln!("飞书同步: …另有 {} 条错误未列出", sync.errors.len() - 8);
    }

    Ok(format!(
        "已同步「{}」| 新增 {}，更新 {}，失败 {} | 来源: {}",
        table.name, sync.created, sync.updated, sync.failed, source_rel
    ))
}

fn resolve_run_file_path(run_dir: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, String> {
    let rel = rel.trim().replace('\\', "/");
    let path = if rel.starts_with("files/") || rel.starts_with("downloads/") {
        run_dir.join(&rel)
    } else if let Some(rest) = rel
        .strip_prefix("templates/")
        .or_else(|| rel.strip_prefix("模板/"))
    {
        crate::templates::templates_dir().join(rest)
    } else {
        run_dir.join("files").join(&rel)
    };
    Ok(path)
}

/// 解析「上传」/「飞书上传」参数：部分文件名 / 空；可选 `按钮=`、`文件夹=`
fn parse_upload_args(args: &str) -> Result<(Option<String>, String, Option<String>), String> {
    let mut source: Option<String> = None;
    let mut button = "上传".to_string();
    let mut folder: Option<String> = None;
    let raw = recipe::normalize_automation_text(args).trim().to_string();
    if raw.is_empty() {
        return Ok((None, button, None));
    }

    for part in split_upload_csv(&raw) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            if key == "来源" || key.eq_ignore_ascii_case("source") || key == "文件" {
                if !val.is_empty() {
                    source = Some(val.to_string());
                }
            } else if key == "按钮" || key.eq_ignore_ascii_case("button") {
                if !val.is_empty() {
                    button = val.to_string();
                }
            } else if key == "文件夹"
                || key == "目录"
                || key.eq_ignore_ascii_case("folder")
                || key.eq_ignore_ascii_case("folder_token")
            {
                if !val.is_empty() {
                    folder = Some(val.to_string());
                }
            } else if source.is_none() && !val.is_empty() {
                source = Some(if key.is_empty() {
                    val.to_string()
                } else {
                    part.to_string()
                });
            }
        } else if source.is_none() {
            source = Some(part.to_string());
        }
    }
    Ok((source, button, folder))
}

fn split_upload_csv(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                cur.push(n);
            } else {
                cur.push('\\');
            }
            continue;
        }
        if c == ',' || c == '，' {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(c);
    }
    out.push(cur);
    out
}

const UPLOAD_FILE_EXTS: &[&str] = &[
    "xlsx", "xls", "xlsm", "csv", "pdf", "zip", "docx", "doc", "png", "jpg", "jpeg", "txt",
];

fn is_upload_candidate_file(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    // 与产物列表一致：跳过 Office 锁文件 ~$*、隐藏文件等
    if name.is_empty()
        || name.starts_with('.')
        || name.starts_with("~$")
        || name.eq_ignore_ascii_case("Thumbs.db")
        || name.eq_ignore_ascii_case("desktop.ini")
        || name.eq_ignore_ascii_case(".DS_Store")
        || name.ends_with(".tmp-replace")
    {
        return false;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // 无扩展名也允许（少见）；有扩展名则限制常用类型
    ext.is_empty() || UPLOAD_FILE_EXTS.iter().any(|e| *e == ext)
}

fn list_run_attach_candidates(run_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for sub in ["files", "downloads"] {
        let dir = run_dir.join(sub);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if is_upload_candidate_file(&path) {
                out.push(path);
            }
        }
    }
    sort_upload_candidates(&mut out);
    out
}

/// 设置里已同步/导入的模板文件（供「上传」按名选用）
fn list_template_upload_candidates() -> Vec<std::path::PathBuf> {
    let _ = crate::templates::ensure_template_files();
    let dir = crate::templates::templates_dir();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Ok(defs) = crate::templates::list_templates() {
        for def in defs {
            let path = dir.join(&def.filename);
            if !is_upload_candidate_file(&path) {
                continue;
            }
            let key = path.to_string_lossy().to_ascii_lowercase();
            if seen.insert(key) {
                out.push(path);
            }
        }
    }
    // 目录里存在、但尚未写入 templates.json 的文件也纳入（容错）
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !is_upload_candidate_file(&path) {
                continue;
            }
            let key = path.to_string_lossy().to_ascii_lowercase();
            if seen.insert(key) {
                out.push(path);
            }
        }
    }
    sort_upload_candidates(&mut out);
    out
}

fn sort_upload_candidates(out: &mut [std::path::PathBuf]) {
    out.sort_by(|a, b| {
        let na = a.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let nb = b.file_name().and_then(|s| s.to_str()).unwrap_or("");
        na.cmp(nb).then_with(|| a.cmp(b))
    });
}

/// 上传候选：本次产物 files/、downloads/ + 本机模板目录
fn list_upload_candidates(run_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = list_run_attach_candidates(run_dir);
    let mut seen: std::collections::HashSet<String> = out
        .iter()
        .map(|p| p.to_string_lossy().to_ascii_lowercase())
        .collect();
    for path in list_template_upload_candidates() {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            out.push(path);
        }
    }
    sort_upload_candidates(&mut out);
    out
}

/// 文件名 +（若为模板）显示名，便于「上传 模板名」命中
fn upload_candidate_match_blob(path: &std::path::Path) -> String {
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let dir = crate::templates::templates_dir();
    if path.starts_with(&dir) {
        if let Ok(defs) = crate::templates::list_templates() {
            for def in defs {
                if dir.join(&def.filename) == path && !def.name.is_empty() && def.name != fname {
                    return format!("{fname} {}", def.name);
                }
            }
        }
    }
    fname
}

pub fn is_wait_download_step(step: &crate::recipe::Step) -> bool {
    args_are_wait_download(&step.keyword, &step.args)
}

fn args_are_wait_download(keyword: &str, args: &str) -> bool {
    if keyword != "等待" {
        return false;
    }
    let s = args.trim().trim_end_matches('秒').trim();
    s == "下载" || s.starts_with("下载")
}

/// 步骤执行前是否跟随浏览器活动标签。
///
/// 唯一决策入口（执行器与 `run_command` 共用）：
/// 1. 「等待下载」永不跟随（短命弹窗关后前台常乱跳）
/// 2. 「脚本」「AI」本机执行，永不跟随（避免反复把 Chrome 置前）
/// 3. 其余看 [`RunnerKind::follows_active_tab`]（默认跟随，opt-out 显式列出）
pub fn should_follow_active_tab(runner: RunnerKind, keyword: &str, args: &str) -> bool {
    if args_are_wait_download(keyword, args) {
        return false;
    }
    if matches!(keyword, "AI" | "脚本") {
        return false;
    }
    runner.follows_active_tab()
}

fn is_upload_keyword(keyword: &str) -> bool {
    matches!(keyword, "上传" | "飞书上传")
}

/// 步骤里有「上传/飞书上传」，且该步之前没有「等待下载」，产物也没有附件时，运行前应提示。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadAttachPreflight {
    pub needs_prompt: bool,
    pub message: String,
    /// 与未命中查询相近的已有文件名（供 UI 一键复制）
    pub similar_names: Vec<String>,
    pub attachment_count: usize,
    pub has_upload: bool,
    pub has_download_before_upload: bool,
}

pub fn check_upload_attach_preflight(
    content: &str,
    chat_session_id: Option<&str>,
    run_dir: Option<&str>,
    start_step_index: Option<u32>,
) -> Result<UploadAttachPreflight, String> {
    let steps = if content.trim().is_empty() {
        Vec::new()
    } else {
        crate::recipe::parse_recipe_steps(content)?
    };
    let start_i = start_step_index
        .map(|n| n.saturating_sub(1) as usize)
        .unwrap_or(0)
        .min(steps.len());
    let run_steps = &steps[start_i..];

    // 预检展开「参数」/公共参数，使「上传我的{{月份}}报表」能命中「我的9月报表」
    let recipe_params = upload_preflight_param_map(content);

    let run_path = run_dir
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            chat_session_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(crate::paths::chat_session_runs_dir)
        });
    // 预检解析用目录：无产物目录时仍可用空目录 + 本机模板候选
    let resolve_dir = run_path.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "webclaw-upload-preflight-empty-{}",
            std::process::id()
        ))
    });

    let attachment_count = run_path
        .as_ref()
        .map(|p| list_run_attach_candidates(p).len())
        .unwrap_or(0);

    // 只认「本次还会执行」的下载；已跳过的下载若未留下附件，仍应提示
    let mut seen_download = false;
    let mut has_upload = false;
    let mut has_download_before_upload = false;
    let mut upload_without_prior_download = false;
    let mut unresolved: Vec<UploadPreflightMiss> = Vec::new();
    for step in run_steps {
        if is_wait_download_step(step) {
            seen_download = true;
        }
        if is_upload_keyword(&step.keyword) {
            has_upload = true;
            if seen_download {
                has_download_before_upload = true;
            } else {
                upload_without_prior_download = true;
                let args = crate::recipe::resolve_step_args(&step.args, &recipe_params);
                // 无前置下载时：
                // - 无参数 / 仅 #N：只认产物 files/downloads（有模板也不放过）
                // - 有文件名/模板名：可匹配产物附件或本机模板
                let (source, _, _) = parse_upload_args(&args)?;
                let miss = match source.as_deref() {
                    None => {
                        if list_run_attach_candidates(&resolve_dir).len() == 1 {
                            None
                        } else {
                            Some(UploadPreflightMiss::NoName)
                        }
                    }
                    Some(q) => {
                        if q.contains("{{") {
                            Some(UploadPreflightMiss::Unexpanded {
                                query: q.to_string(),
                            })
                        } else {
                            let (needle, index) = strip_upload_match_index(q);
                            if needle.is_empty() && index > 0 {
                                if list_run_attach_candidates(&resolve_dir).len() >= index {
                                    None
                                } else {
                                    Some(UploadPreflightMiss::BadIndex { index })
                                }
                            } else if resolve_upload_file(&resolve_dir, Some(q)).is_ok() {
                                None
                            } else {
                                Some(UploadPreflightMiss::NotFound {
                                    query: if needle.is_empty() {
                                        q.to_string()
                                    } else {
                                        needle
                                    },
                                })
                            }
                        }
                    }
                };
                if let Some(m) = miss {
                    unresolved.push(m);
                }
            }
        }
    }

    let needs_prompt = upload_without_prior_download && !unresolved.is_empty();
    let (message, similar_names) = if needs_prompt {
        format_upload_preflight_message(&unresolved, &resolve_dir)
    } else {
        (String::new(), Vec::new())
    };

    Ok(UploadAttachPreflight {
        needs_prompt,
        message,
        similar_names,
        attachment_count,
        has_upload,
        has_download_before_upload,
    })
}

#[derive(Debug, Clone)]
enum UploadPreflightMiss {
    /// `上传` 未写文件名，且产物不是恰好 1 个
    NoName,
    /// `上传#N` 但产物不足
    BadIndex { index: usize },
    /// 仍含 `{{…}}`
    Unexpanded { query: String },
    /// 有查询名但未命中
    NotFound { query: String },
}

fn format_upload_preflight_message(
    misses: &[UploadPreflightMiss],
    resolve_dir: &std::path::Path,
) -> (String, Vec<String>) {
    let run_cands = list_run_attach_candidates(resolve_dir);
    let all_cands = list_upload_candidates(resolve_dir);
    let mut lines: Vec<String> = Vec::new();
    let mut similar_names: Vec<String> = Vec::new();
    lines.push("步骤含「上传」，但未匹配到可用附件。".into());

    for miss in misses {
        match miss {
            UploadPreflightMiss::NoName => {
                if run_cands.is_empty() {
                    lines.push("未指定文件名，且当前没有产物附件。".into());
                } else {
                    lines.push(format!(
                        "未指定文件名（当前有 {} 个产物附件，请写：上传 部分文件名）。",
                        run_cands.len()
                    ));
                }
            }
            UploadPreflightMiss::BadIndex { index } => {
                lines.push(format!(
                    "序号 #{index} 不可用（产物附件 {} 个）。",
                    run_cands.len()
                ));
            }
            UploadPreflightMiss::Unexpanded { query } => {
                lines.push(format!(
                    "「{query}」仍含未展开参数，请先写 参数名=值。"
                ));
            }
            UploadPreflightMiss::NotFound { query } => {
                lines.push(format!("未找到「{query}」。"));
                for name in similar_upload_names(query, &all_cands, 3) {
                    if !similar_names.iter().any(|n| n == &name) {
                        similar_names.push(name);
                    }
                }
            }
        }
    }

    if !run_cands.is_empty()
        && !misses
            .iter()
            .any(|m| matches!(m, UploadPreflightMiss::NotFound { .. }))
    {
        let names: Vec<_> = run_cands
            .iter()
            .take(5)
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(str::to_string))
            .collect();
        if !names.is_empty() {
            let more = if run_cands.len() > names.len() {
                format!(" 等{}个", run_cands.len())
            } else {
                String::new()
            };
            lines.push(format!("当前附件：{}{more}", names.join("、")));
        }
    }

    lines.push("可点「添加附件」、改文件名关键字，或在上传前加入「等待下载」。".into());
    (lines.join(""), similar_names)
}

/// 与查询「相差不大」的已有附件/模板名（供预检提示）。
fn similar_upload_names(query: &str, candidates: &[std::path::PathBuf], limit: usize) -> Vec<String> {
    let needle = normalize_upload_fuzzy(query);
    if needle.is_empty() || candidates.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(i32, String)> = Vec::new();
    for path in candidates {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let blob = upload_candidate_match_blob(path);
        let score = upload_name_similarity(&needle, &normalize_upload_fuzzy(&blob))
            .max(upload_name_similarity(
                &needle,
                &normalize_upload_fuzzy(&name),
            ));
        // 约一半以上相近，或编辑距离很小
        if score >= 55 {
            scored.push((score, name));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(limit).map(|(_, n)| n).collect()
}

fn normalize_upload_fuzzy(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-' && *c != '—')
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .trim_end_matches(".xlsx")
        .trim_end_matches(".xls")
        .trim_end_matches(".xlsm")
        .trim_end_matches(".csv")
        .trim_end_matches(".pdf")
        .trim_end_matches(".zip")
        .to_string()
}

/// 0..=100：相等 / 包含 / 编辑距离换算。
fn upload_name_similarity(a: &str, b: &str) -> i32 {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    if a == b {
        return 100;
    }
    if a.contains(b) || b.contains(a) {
        let short = a.chars().count().min(b.chars().count());
        let long = a.chars().count().max(b.chars().count());
        return ((short * 100) / long.max(1)) as i32;
    }
    let dist = levenshtein_chars(a, b);
    let long = a.chars().count().max(b.chars().count()).max(1);
    let score = 100i32.saturating_sub(((dist * 100) / long) as i32);
    // 要求至少共享一定比例字符，避免无关短名乱入
    let overlap = shared_char_ratio(a, b);
    if overlap < 40 && score < 70 {
        return 0;
    }
    score
}

fn shared_char_ratio(a: &str, b: &str) -> i32 {
    use std::collections::HashMap;
    let mut ca: HashMap<char, usize> = HashMap::new();
    for c in a.chars() {
        *ca.entry(c).or_default() += 1;
    }
    let mut shared = 0usize;
    for c in b.chars() {
        if let Some(n) = ca.get_mut(&c) {
            if *n > 0 {
                *n -= 1;
                shared += 1;
            }
        }
    }
    let long = a.chars().count().max(b.chars().count()).max(1);
    ((shared * 100) / long) as i32
}

fn levenshtein_chars(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// 预检用参数表：正文「参数」声明 + 被引用的公共参数（不强制校验其它步骤占位符）。
fn upload_preflight_param_map(content: &str) -> std::collections::HashMap<String, String> {
    let from_steps = crate::recipe::extract_recipe_params_only(content)
        .ok()
        .and_then(|v| crate::recipe::ordered_vec_to_params(&v).ok())
        .unwrap_or_default();
    crate::public_params::base_params_with_public(content, &from_steps)
}

/// 从查询末尾拆出 `#N` / `＃N`（与点击指令一致）；无序号时返回 0 表示「须唯一匹配」
fn strip_upload_match_index(query: &str) -> (String, usize) {
    let q = query.trim();
    let bytes = q.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == 0 || i == bytes.len() {
        return (q.to_string(), 0);
    }
    let hash = &q[..i];
    let digits = &q[i..];
    let hash = hash.trim_end();
    if !(hash.ends_with('#') || hash.ends_with('＃')) {
        return (q.to_string(), 0);
    }
    let needle = hash.trim_end_matches(['#', '＃']).trim_end();
    let idx: usize = digits.parse().unwrap_or(0);
    if idx == 0 {
        return (q.to_string(), 0);
    }
    (needle.to_string(), idx)
}

fn filename_contains_ci(name: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&needle.to_lowercase())
}

/// 去掉末尾常见附件扩展名（大小写不敏感），便于「上传月报」命中「月报.xlsx」。
fn strip_upload_ext_ci(name: &str) -> String {
    let lower: String = name.chars().flat_map(|c| c.to_lowercase()).collect();
    for ext in UPLOAD_FILE_EXTS {
        let suf = format!(".{ext}");
        if lower.ends_with(&suf) {
            return lower[..lower.len() - suf.len()].to_string();
        }
    }
    lower
}

/// 上传查询是否命中候选：支持不写扩展名，且忽略 .xlsx/.xls 等扩展差异。
fn upload_query_matches_candidate(path: &std::path::Path, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return true;
    }
    let blob = upload_candidate_match_blob(path);
    if filename_contains_ci(&blob, needle) {
        return true;
    }
    let needle_key = strip_upload_ext_ci(needle);
    if needle_key.is_empty() {
        return false;
    }
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let stem = strip_upload_ext_ci(fname);
    if !stem.is_empty() && stem.contains(&needle_key) {
        return true;
    }
    // 模板显示名等（blob 可能是「文件名.xlsx 显示名」）
    for part in blob.split_whitespace() {
        let key = strip_upload_ext_ci(part);
        if !key.is_empty() && key.contains(&needle_key) {
            return true;
        }
    }
    false
}

/// 路径未带扩展名或不存在时，尝试补全常见扩展名 / xls↔xlsx 别名。
fn resolve_upload_path_with_ext_fallback(
    run_dir: &std::path::Path,
    raw: &str,
) -> Result<std::path::PathBuf, String> {
    let p = resolve_run_file_path(run_dir, raw)?;
    if p.is_file() {
        return Ok(p);
    }
    let abs = std::path::PathBuf::from(raw);
    if abs.is_file() {
        return Ok(abs);
    }
    // 已有扩展名：再试 excel 同名别名（.xlsx ↔ .xls）
    let resolved = crate::rhai_runtime::excel_compat::resolve_excel_path(&p);
    if resolved.is_file() {
        return Ok(resolved);
    }
    if abs != p {
        let resolved_abs = crate::rhai_runtime::excel_compat::resolve_excel_path(&abs);
        if resolved_abs.is_file() {
            return Ok(resolved_abs);
        }
    }
    // 无扩展名：依次尝试常见后缀
    let has_ext = std::path::Path::new(raw)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.is_empty())
        .unwrap_or(false);
    if !has_ext {
        for ext in UPLOAD_FILE_EXTS {
            let try_rel = format!("{raw}.{ext}");
            if let Ok(cand) = resolve_run_file_path(run_dir, &try_rel) {
                if cand.is_file() {
                    return Ok(cand);
                }
            }
            let try_abs = std::path::PathBuf::from(format!("{raw}.{ext}"));
            if try_abs.is_file() {
                return Ok(try_abs);
            }
        }
    }
    Err(format!("文件不存在: {}", p.display()))
}

fn format_candidate_names(paths: &[std::path::PathBuf]) -> String {
    let templates_dir = crate::templates::templates_dir();
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("?");
            let tag = if p.starts_with(&templates_dir) {
                "模板"
            } else if p
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|s| s.to_str())
                == Some("downloads")
            {
                "下载"
            } else {
                "附件"
            };
            format!("  #{}\t[{tag}] {name}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析要上传的本地文件：
/// - 空：仅当 files/ + downloads/ 合计 1 个时自动选用（不含模板）
/// - 仅 `#N`：只按产物 files/downloads 序号选取（不含模板；无产物附件则失败）
/// - 部分文件名 / 模板显示名：在产物 + 模板中子串匹配；多个命中须写 `#序号`
/// - `files/…` / `downloads/…` / `templates/…` / 绝对路径：按路径解析
pub(crate) fn resolve_upload_file(
    run_dir: &std::path::Path,
    query: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let run_candidates = list_run_attach_candidates(run_dir);
    let all_candidates = list_upload_candidates(run_dir);
    let q = query.map(str::trim).filter(|s| !s.is_empty());

    let Some(raw) = q else {
        return match run_candidates.len() {
            0 => Err(
                "产物目录 files/、downloads/ 中没有可上传文件。请先「添加附件」、等待下载，或写模板名：上传 考勤汇总"
                    .into(),
            ),
            1 => Ok(run_candidates.into_iter().next().unwrap()),
            n => Err(format!(
                "有 {n} 个产物附件，请写部分文件名（可加 #序号）：上传 考勤 或 上传 月报#2\n{}",
                format_candidate_names(&run_candidates)
            )),
        };
    };

    // 显式路径（可省略扩展名）
    if raw.contains('/') || raw.contains('\\') {
        return resolve_upload_path_with_ext_fallback(run_dir, raw);
    }

    let (needle, index) = strip_upload_match_index(raw);
    if needle.is_empty() && index > 0 {
        // 仅写了 #2：只认产物 files/downloads，避免误选本机模板
        if run_candidates.is_empty() {
            return Err(format!(
                "产物目录 files/、downloads/ 中没有附件，无法使用 #{index}。请先「添加附件」，或改写模板名：上传 考勤汇总"
            ));
        }
        if index > run_candidates.len() {
            return Err(format!(
                "序号 #{index} 超出范围（产物附件共 {} 个）\n{}",
                run_candidates.len(),
                format_candidate_names(&run_candidates)
            ));
        }
        return Ok(run_candidates[index - 1].clone());
    }

    let matched: Vec<_> = all_candidates
        .iter()
        .filter(|p| upload_query_matches_candidate(p, &needle))
        .cloned()
        .collect();

    if matched.is_empty() {
        return Err(format!(
            "未找到名称包含「{needle}」的附件或模板\n可选:\n{}",
            format_candidate_names(&all_candidates)
        ));
    }
    // 多个命中时：若恰有一个「去扩展名后全等」，优先选用（避免 ~$ 锁文件等误伤；也消歧 暂估单/暂估表）
    let needle_key = strip_upload_ext_ci(&needle);
    let exact: Vec<_> = matched
        .iter()
        .filter(|p| {
            let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            strip_upload_ext_ci(fname) == needle_key
        })
        .cloned()
        .collect();
    let matched = if exact.len() == 1 && index <= 1 {
        exact
    } else {
        matched
    };

    if matched.len() == 1 {
        if index > 1 {
            return Err(format!(
                "仅匹配到 1 个文件，无法使用 #{index}：{}",
                matched[0]
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
            ));
        }
        return Ok(matched.into_iter().next().unwrap());
    }
    // 多个命中
    if index == 0 {
        return Err(format!(
            "「{needle}」匹配到 {} 个文件，请加 #序号：上传 {needle}#1\n{}",
            matched.len(),
            format_candidate_names(&matched)
        ));
    }
    if index > matched.len() {
        return Err(format!(
            "序号 #{index} 超出范围（「{needle}」共 {} 个匹配）\n{}",
            matched.len(),
            format_candidate_names(&matched)
        ));
    }
    Ok(matched[index - 1].clone())
}

async fn run_upload<R: Runtime>(
    client: &mut CdpClient,
    args: &str,
    session: Option<&Arc<RunSession>>,
    ctx: Option<CommandContext<'_, R>>,
) -> Result<String, String> {
    let cancel_flag = ctx.as_ref().and_then(|c| c.cancel.clone());
    let cancel = cancel_flag.as_deref();
    check_cancel(cancel)?;
    let session = session.ok_or(
        "上传需要运行产物目录（请在快捷指令/任务中执行，并先「添加附件」到 files/）",
    )?;
    let (source, _button, _folder) = parse_upload_args(args)?;
    let path = resolve_upload_file(&session.run_dir, source.as_deref())?;

    let templates_dir = crate::templates::templates_dir();
    let display_rel = if let Ok(rel) = path.strip_prefix(&session.run_dir) {
        rel.to_string_lossy().replace('\\', "/")
    } else if let Ok(rel) = path.strip_prefix(&templates_dir) {
        format!("templates/{}", rel.to_string_lossy().replace('\\', "/"))
    } else {
        path.display().to_string()
    };

    client.prepare_tab_for_automation().await?;
    // 上传期间禁止自动落盘，并清空已误入队的 URL
    session.set_auto_fetch_armed(false);
    let cleared = crate::browser_network::clear_download_urls_on(client.port);
    session.append_download_trace(format!(
        "上传步骤：暂停自动落盘并清空队列（丢弃 {cleared} 条）"
    ));
    let _ = client_download::set_capture_mode(client, false).await;

    // 只写入文件；点「上传 / 上传文件」由前面的点击步骤完成（执行器已全程屏蔽系统文件框）
    let detail = client
        .upload_local_files_after_open(std::slice::from_ref(&path))
        .await;

    let _ = client_download::set_capture_mode(client, true).await;
    // wait_only：保持未武装，由执行器在「等待下载」窗口再开
    // （历史上非上传菜谱会在这里恢复全程武装，已统一为 wait_only）
    let cleared2 = crate::browser_network::clear_download_urls_on(client.port);
    if cleared2 > 0 {
        session.append_download_trace(format!("上传结束后再次清空队列（丢弃 {cleared2} 条）"));
    }

    let detail = detail?;
    check_cancel(cancel)?;
    Ok(format!("{detail} | 来源: {display_rel}"))
}

/// 去掉菜谱拆分残留的尾部标点（如「等待1,」），便于识别纯秒数等待。
fn strip_wait_tail_punct(s: &str) -> &str {
    s.trim_end_matches(|c: char| {
        matches!(c, ',' | '，' | '。' | '；' | ';' | '、' | '．' | '.')
    })
    .trim()
}

/// 从「等待」参数提取超时秒数，供 CDP Runtime.evaluate 预算（须 ≥ 页内脚本轮询）。
fn wait_timeout_secs_hint(args: &str) -> u64 {
    const DEFAULT: u64 = 30;
    const DOWNLOAD_DEFAULT: u64 = 200;
    let s = strip_wait_tail_punct(args.trim().trim_end_matches('秒').trim());
    if s.is_empty() {
        return DEFAULT;
    }
    if let Ok(n) = s.parse::<f64>() {
        return ceil_secs(n);
    }
    // 去掉模式前缀后再取尾部秒数
    let rest = [
        ("下载", 2usize),
        ("加载", 2),
        ("文字", 2),
        ("选择器", 3),
        ("消失", 2),
        ("遮罩", 2),
    ]
    .into_iter()
    .find_map(|(p, _)| s.strip_prefix(p))
    .map(|r| r.trim_start_matches([':', '：']).trim())
    .unwrap_or(s);

    let is_download = s.starts_with("下载");
    let mode_default = if is_download { DOWNLOAD_DEFAULT } else { DEFAULT };

    if rest.is_empty() {
        return mode_default;
    }
    if let Ok(n) = rest.parse::<f64>() {
        return ceil_secs(n);
    }
    // 「xxx 200」空格分隔
    if let Some((body, timeout)) = rest.rsplit_once([' ', '\t']) {
        if !body.trim().is_empty() {
            if let Ok(n) = timeout.trim().parse::<f64>() {
                if n >= 1.0 {
                    return ceil_secs(n);
                }
            }
        }
    }
    // 「xxx200」/「登录30」紧挨；「按钮#2」末尾序号不当超时
    if let Some((body, digits)) = split_trailing_number(rest) {
        let body = body.trim();
        if body.ends_with('#') {
            return mode_default;
        }
        if let Ok(n) = digits.parse::<f64>() {
            if n >= 1.0 {
                return ceil_secs(n);
            }
        }
    }
    mode_default
}

fn split_trailing_number(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i > 0 && bytes[i - 1] == b'.' {
        let mut j = i - 1;
        while j > 0 && bytes[j - 1].is_ascii_digit() {
            j -= 1;
        }
        if j < i - 1 {
            i = j;
        }
    }
    if i == bytes.len() || i == 0 {
        return None;
    }
    Some((&s[..i], &s[i..]))
}

fn ceil_secs(n: f64) -> u64 {
    if !n.is_finite() || n < 1.0 {
        1
    } else {
        n.ceil() as u64
    }
}

/// CDP 求值上限 = 等待秒数 + 缓冲，避免「等待xxx 200」被硬编码 35s 截断后误进页面恢复
fn wait_script_cdp_timeout(args: &str) -> Duration {
    const BUFFER_SECS: u64 = 8;
    const MIN_SECS: u64 = 15;
    const MAX_SECS: u64 = 600;
    let secs = wait_timeout_secs_hint(args)
        .saturating_add(BUFFER_SECS)
        .clamp(MIN_SECS, MAX_SECS);
    Duration::from_secs(secs)
}

fn normalize_url(url: &str) -> String {
    // 组间逗号等若误留在参数里，导航前剥掉（勿剥 URL 路径里的 `.`）
    let url = recipe::normalize_automation_text(url)
        .trim()
        .trim_end_matches(|c: char| matches!(c, '，' | ',' | '、' | ';' | '；') || c.is_whitespace())
        .to_string();
    // 「打开浏览器访问 http://…」→ 抽出真 URL，避免 https://浏览器访问%20http://… → xn--
    let url = crate::web_systems::coerce_navigate_url(&url);
    if url.starts_with("http://") || url.starts_with("https://") {
        return url;
    }
    // 「打开云盘考勤文件夹」等：按网站账号名称解析为配置 URL，避免被当成 https://中文域名
    if let Some(resolved) = crate::web_systems::resolve_open_url_from_query(&url) {
        let resolved = resolved
            .trim()
            .trim_end_matches(|c: char| {
                matches!(c, '，' | ',' | '、' | ';' | '；') || c.is_whitespace()
            });
        if !resolved.is_empty() {
            if resolved.starts_with("http://") || resolved.starts_with("https://") {
                return resolved.to_string();
            }
            return format!("https://{resolved}");
        }
    }
    format!("https://{url}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_url_strips_browser_visit_fluff() {
        assert_eq!(
            normalize_url("浏览器访问 http://zhhw.clean-pro.cn:18082/#/"),
            "http://zhhw.clean-pro.cn:18082/#/"
        );
        assert_eq!(
            normalize_url("打开浏览器访问https://example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            normalize_url(
                "https://xn--%20http-0v7l951our7cese330b//zhhw.clean-pro.cn:18082/#/"
            ),
            "http://zhhw.clean-pro.cn:18082/#/"
        );
    }

    #[test]
    fn should_follow_active_tab_policy() {
        // 页内 JS（采集/截图/等待文字…）默认跟随
        assert!(should_follow_active_tab(
            RunnerKind::JavaScript,
            "采集",
            ""
        ));
        assert!(should_follow_active_tab(
            RunnerKind::JavaScript,
            "等待",
            "文字登录10"
        ));
        // 「等待下载」豁免
        assert!(!should_follow_active_tab(
            RunnerKind::JavaScript,
            "等待",
            "下载"
        ));
        assert!(!should_follow_active_tab(
            RunnerKind::JavaScript,
            "等待",
            "下载200"
        ));
        assert!(!should_follow_active_tab(
            RunnerKind::JavaScript,
            "AI",
            "总结异常记录"
        ));
        // 「脚本」即使 runner 误为 JavaScript 也不跟随（避免反复置前 Chrome）
        assert!(!should_follow_active_tab(
            RunnerKind::JavaScript,
            "脚本",
            "合并考勤数据"
        ));
        // 自己管标签 / 本机脚本
        assert!(!should_follow_active_tab(RunnerKind::RustTab, "页签", "#2"));
        assert!(!should_follow_active_tab(
            RunnerKind::NamedScript,
            "脚本",
            "合并考勤数据"
        ));
        // 像素坐标点击仍跟随活动标签（与普通点击同一 runner）
        assert!(should_follow_active_tab(
            RunnerKind::RustClick,
            "点击",
            "120 80"
        ));
        assert!(should_follow_active_tab(RunnerKind::RustClick, "点击", ""));
        assert!(super::super::scripts::is_pixel_click_args("120 80"));
        assert!(super::super::scripts::is_pixel_click_args(""));
        assert!(!super::super::scripts::is_pixel_click_args("登录"));
    }

    #[test]
    fn format_click_log_includes_message() {
        let v = json!({
            "message": "已点击「登 录」→ BUTTON#loginBtn.btn-primary「登 录」",
            "query": "登录",
            "score": 1.08,
            "textScore": 1.0,
            "strongCount": 2
        });
        let log = format_click_log(&v, "登 录", "登录");
        assert!(log.starts_with("已点击「登 录」→ BUTTON#loginBtn"));
        assert!(log.contains("查询: 登录"));
        assert!(log.contains("score: 1.080"));
        assert!(log.contains("候选: 2"));
    }

    #[test]
    fn parse_upload_args_path_and_button() {
        let (src, btn, folder) = parse_upload_args("考勤").unwrap();
        assert_eq!(src.as_deref(), Some("考勤"));
        assert_eq!(btn, "上传");
        assert!(folder.is_none());

        let (src, btn, folder) =
            parse_upload_args("月报#2,按钮=本地上传").unwrap();
        assert_eq!(src.as_deref(), Some("月报#2"));
        assert_eq!(btn, "本地上传");
        assert!(folder.is_none());

        let (src, btn, folder) = parse_upload_args("来源=files/b.xlsx").unwrap();
        assert_eq!(src.as_deref(), Some("files/b.xlsx"));
        assert_eq!(btn, "上传");
        assert!(folder.is_none());

        let (src, btn, folder) = parse_upload_args(
            "考勤,文件夹=https://xxx.feishu.cn/drive/folder/TOK123",
        )
        .unwrap();
        assert_eq!(src.as_deref(), Some("考勤"));
        assert_eq!(btn, "上传");
        assert_eq!(
            folder.as_deref(),
            Some("https://xxx.feishu.cn/drive/folder/TOK123")
        );

        let (src, btn, folder) = parse_upload_args("").unwrap();
        assert!(src.is_none());
        assert_eq!(btn, "上传");
        assert!(folder.is_none());
    }

    #[test]
    fn strip_upload_match_index_works() {
        assert_eq!(strip_upload_match_index("考勤"), ("考勤".into(), 0));
        assert_eq!(strip_upload_match_index("月报#2"), ("月报".into(), 2));
        assert_eq!(strip_upload_match_index("月报＃3"), ("月报".into(), 3));
        assert_eq!(strip_upload_match_index("#2"), ("".into(), 2));
    }

    #[test]
    fn upload_preflight_prompts_without_download_or_attach() {
        let r = check_upload_attach_preflight("点击上传文件，上传", None, None, None).unwrap();
        assert!(r.has_upload);
        assert!(!r.has_download_before_upload);
        assert!(r.needs_prompt);
        assert!(r.message.contains("添加附件"));
    }

    #[test]
    fn upload_preflight_ok_with_wait_download() {
        let r = check_upload_attach_preflight(
            "等待下载，点击上传文件，上传",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(r.has_upload);
        assert!(r.has_download_before_upload);
        assert!(!r.needs_prompt);
    }

    #[test]
    fn upload_preflight_ok_with_existing_attach() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-upload-preflight-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(dir.join("files").join("a.xlsx"), b"a").unwrap();
        let r = check_upload_attach_preflight(
            "上传",
            None,
            Some(dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        assert!(r.has_upload);
        assert_eq!(r.attachment_count, 1);
        assert!(!r.needs_prompt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_upload_file_partial_and_index() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-upload-resolve-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        std::fs::write(dir.join("files").join("考勤-alpha.xlsx"), b"a").unwrap();
        std::fs::write(dir.join("files").join("考勤-beta.xlsx"), b"b").unwrap();
        std::fs::write(dir.join("downloads").join("月报.xls"), b"c").unwrap();

        let only = resolve_upload_file(&dir, Some("月报")).unwrap();
        assert!(only.ends_with("月报.xls"));

        // 写了 .xlsx 也能命中实际的 .xls；不写扩展名同上
        let cross = resolve_upload_file(&dir, Some("月报.xlsx")).unwrap();
        assert!(cross.ends_with("月报.xls"));
        let by_path = resolve_upload_file(&dir, Some("files/考勤-alpha")).unwrap();
        assert!(
            by_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap()
                .contains("alpha"),
            "{by_path:?}"
        );

        let err = resolve_upload_file(&dir, Some("考勤")).unwrap_err();
        assert!(err.contains("#序号"));

        let first = resolve_upload_file(&dir, Some("考勤#1")).unwrap();
        let second = resolve_upload_file(&dir, Some("考勤#2")).unwrap();
        assert!(first
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap()
            .contains("alpha"));
        assert!(second
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap()
            .contains("beta"));

        let multi_empty = resolve_upload_file(&dir, None).unwrap_err();
        assert!(multi_empty.contains("部分文件名") || multi_empty.contains("模板名"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_run_file_path_templates_prefix() {
        let run = std::path::PathBuf::from("dummy-run");
        let p = resolve_run_file_path(&run, "templates/工时统计.xlsx").unwrap();
        assert_eq!(
            p,
            crate::templates::templates_dir().join("工时统计.xlsx")
        );
        let p2 = resolve_run_file_path(&run, "模板/a.xlsx").unwrap();
        assert_eq!(p2, crate::templates::templates_dir().join("a.xlsx"));
    }

    #[test]
    fn preflight_prompts_when_upload_args_miss_template() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-upload-preflight-miss-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let r = check_upload_attach_preflight(
            "上传 不存在的模板名xyz999",
            None,
            Some(dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        assert!(r.has_upload);
        assert_eq!(r.attachment_count, 0);
        assert!(r.needs_prompt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_ok_when_upload_args_match_template() {
        let _ = crate::templates::ensure_template_files();
        let tpl_dir = crate::templates::templates_dir();
        let file = tpl_dir.join("webclaw-preflight-tpl-match.xlsx");
        std::fs::write(&file, b"tpl").unwrap();
        let r = check_upload_attach_preflight(
            "上传 webclaw-preflight-tpl-match",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(r.has_upload);
        assert!(!r.needs_prompt, "matched template should skip attach prompt");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn preflight_prompts_upload_hash_index_without_run_attach() {
        // 上传#1：即便本机有模板，files/downloads 为空也要拦截
        let _ = crate::templates::ensure_template_files();
        let tpl_dir = crate::templates::templates_dir();
        let file = tpl_dir.join("webclaw-preflight-hash-idx.xlsx");
        std::fs::write(&file, b"tpl").unwrap();

        let dir = std::env::temp_dir().join(format!(
            "webclaw-upload-preflight-hash-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();

        let r = check_upload_attach_preflight(
            "上传#1",
            None,
            Some(dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        assert!(r.has_upload);
        assert_eq!(r.attachment_count, 0);
        assert!(r.needs_prompt);

        let err = resolve_upload_file(&dir, Some("#1")).unwrap_err();
        assert!(err.contains("没有附件") || err.contains("添加附件"));

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn upload_preflight_expands_recipe_params_in_filename() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-upload-preflight-param-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(dir.join("files").join("我的9月报表.xlsx"), b"a").unwrap();

        let r = check_upload_attach_preflight(
            "参数月份=9月，上传我的{{月份}}报表",
            None,
            Some(dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        assert!(r.has_upload);
        assert!(!r.needs_prompt, "expanded {{月份}} should match attachment");

        let r2 = check_upload_attach_preflight(
            "参数月份=9月，上传我的{{月份}}报表",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(r2.needs_prompt, "still prompt when no matching file");
        assert!(
            r2.message.contains("我的9月报表"),
            "message should name missing file: {}",
            r2.message
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_upload_ignores_office_lock_and_prefers_exact_stem() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-upload-lock-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(dir.join("files").join("【2026】收入暂估单.xlsx"), b"a").unwrap();
        std::fs::write(dir.join("files").join("【2026】收入暂估表.xls"), b"b").unwrap();
        std::fs::write(dir.join("files").join("~$【2026】收入暂估单.xlsx"), b"c").unwrap();

        assert!(
            !is_upload_candidate_file(&dir.join("files").join("~$【2026】收入暂估单.xlsx"))
        );

        let hit = resolve_upload_file(&dir, Some("【2026】收入暂估单")).unwrap();
        assert!(
            hit.file_name().and_then(|s| s.to_str()) == Some("【2026】收入暂估单.xlsx"),
            "{hit:?}"
        );

        let r = check_upload_attach_preflight(
            "上传【2026】收入暂估单",
            None,
            Some(dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        assert!(!r.needs_prompt, "should match exact file, msg={}", r.message);
        assert!(r.similar_names.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_preflight_suggests_similar_attachment_names() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-upload-preflight-similar-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(dir.join("files").join("我的9月报表.xlsx"), b"a").unwrap();
        std::fs::write(dir.join("files").join("考勤汇总.xlsx"), b"b").unwrap();

        let r = check_upload_attach_preflight(
            "上传我的9月月报",
            None,
            Some(dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        assert!(r.needs_prompt);
        assert!(r.message.contains("未找到「我的9月月报」"), "{}", r.message);
        assert!(
            r.similar_names.iter().any(|n| n.contains("我的9月报表")),
            "should suggest close name in similar_names: {:?}",
            r.similar_names
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_name_similarity_basic() {
        assert!(upload_name_similarity("我的9月报表", "我的9月报表") >= 100);
        assert!(upload_name_similarity("我的9月月报", "我的9月报表") >= 55);
        assert_eq!(upload_name_similarity("考勤", "完全无关文件名xyz"), 0);
    }

    #[test]
    fn click_coords_from_script_skips_dom_clicked() {
        let v = json!({ "clicked": true, "x": 10.0, "y": 20.0 });
        assert!(click_coords_from_script(&v).unwrap().is_none());
    }

    #[test]
    fn click_coords_from_script_returns_center() {
        let v = json!({ "clicked": false, "x": 100.5, "y": 200.25 });
        let (x, y) = click_coords_from_script(&v).unwrap().unwrap();
        assert!((x - 100.5).abs() < f64::EPSILON);
        assert!((y - 200.25).abs() < f64::EPSILON);
    }

    #[test]
    fn click_coords_from_script_requires_xy_when_not_clicked() {
        let v = json!({ "clicked": false });
        assert!(click_coords_from_script(&v).is_err());
    }

    #[test]
    fn parse_tab_args_switch_and_close() {
        assert_eq!(parse_tab_args("#2").unwrap(), TabAction::Switch(2));
        assert_eq!(parse_tab_args("2").unwrap(), TabAction::Switch(2));
        assert_eq!(parse_tab_args("切换#3").unwrap(), TabAction::Switch(3));
        assert_eq!(parse_tab_args("关闭").unwrap(), TabAction::Close(None));
        assert_eq!(parse_tab_args("关闭当前").unwrap(), TabAction::Close(None));
        assert_eq!(parse_tab_args("关闭#1").unwrap(), TabAction::Close(Some(1)));
        assert_eq!(parse_tab_args("关闭2").unwrap(), TabAction::Close(Some(2)));
        assert!(parse_tab_args("").is_err());
        assert!(parse_tab_args("#0").is_err());
    }

    #[test]
    fn parse_clipboard_args_copy_paste() {
        assert_eq!(
            parse_clipboard_args("复制=hello").unwrap(),
            ClipboardAction::CopyText("hello".into())
        );
        assert_eq!(
            parse_clipboard_args("复制 hello").unwrap(),
            ClipboardAction::CopyText("hello".into())
        );
        assert_eq!(
            parse_clipboard_args("复制").unwrap(),
            ClipboardAction::CopySelection
        );
        assert_eq!(
            parse_clipboard_args("粘贴").unwrap(),
            ClipboardAction::Paste
        );
        assert!(parse_clipboard_args("").is_err());
    }

    #[test]
    fn wait_timeout_hint_respects_user_seconds() {
        assert_eq!(wait_timeout_secs_hint("xxx 200"), 200);
        assert_eq!(wait_timeout_secs_hint("xxx200"), 200);
        assert_eq!(wait_timeout_secs_hint("文字success 200"), 200);
        assert_eq!(wait_timeout_secs_hint("登录30"), 30);
        assert_eq!(wait_timeout_secs_hint("2"), 2);
        assert_eq!(wait_timeout_secs_hint("1,"), 1);
        assert_eq!(wait_timeout_secs_hint("3，"), 3);
        assert_eq!(wait_timeout_secs_hint("2秒"), 2);
        assert_eq!(wait_timeout_secs_hint("按钮#2"), 30); // 序号不是超时
        assert_eq!(wait_timeout_secs_hint("输入框#1 15"), 15);
        assert_eq!(wait_timeout_secs_hint("下载"), 200);
        assert_eq!(wait_timeout_secs_hint("下载60"), 60);
        assert!(wait_script_cdp_timeout("xxx 200") >= Duration::from_secs(200));
        assert!(wait_script_cdp_timeout("xxx 200") < Duration::from_secs(220));
    }

    #[test]
    fn wait_pure_sleep_secs_detects_delay_only() {
        assert_eq!(wait_pure_sleep_secs("1").as_deref(), Some("1"));
        assert_eq!(wait_pure_sleep_secs("1,").as_deref(), Some("1"));
        assert_eq!(wait_pure_sleep_secs("2秒").as_deref(), Some("2"));
        assert_eq!(wait_pure_sleep_secs("").as_deref(), Some("1"));
        assert!(wait_pure_sleep_secs("下载").is_none());
        assert!(wait_pure_sleep_secs("登录").is_none());
        assert!(wait_pure_sleep_secs("文字确定10").is_none());
    }
}
