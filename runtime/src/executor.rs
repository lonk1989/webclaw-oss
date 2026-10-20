use crate::ai_repair::{self, ErrorClass, ProposeSource, RepairApply};
use crate::ark::{self, FailureAnalysisContext};
use crate::browser_context;
use crate::browser_memory;
use crate::browser_network;
use crate::browser_session;
use crate::captcha;
use crate::cdp::CdpClient;
use crate::chrome;
use crate::commands::{self, run_command};
use crate::login_verify;
use crate::page_recovery::{self, RecoveryAnchor};
use crate::recipe::{
    is_login_submit_click_target, norm_ui_contains, parse_recipe_steps, OrBranch, Step,
};
use crate::run_artifacts::{self, RunSession};
use crate::settings;
use crate::task_progress::{self, TaskProgressPayload, TaskStepRecord};
use crate::task_run::TaskControl;
use chrono::Local;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use crate::run_user_log::{
    self, failure_analysis_detail_hint, format_user_failure_analysis, format_user_footer,
    format_user_header, format_user_step_fail, format_user_step_ok, user_step_summary,
    with_step_timing,
};
use crate::ui_host::{AppHandle, Emitter, Manager, Runtime};

/// 共用调试浏览器：不要把 OA `isprint=1` 打印页记成主标签。
/// 否则第一次打印成功后 last_main 停在打印页，exit 只关那一页捕获，第二次点打印易走原生预览。
fn remember_shared_main_target(state: &crate::AppState, client: &CdpClient) {
    if client.target_url.contains("isprint=1") {
        return;
    }
    *state.last_main_target.lock() = Some(client.target_id.clone());
}

fn emit_task_progress<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    control: &TaskControl,
    task_name: &str,
    phase: &str,
    current: u32,
    total: u32,
    label: &str,
    indeterminate: bool,
    queued: bool,
) {
    let stopping = control.cancel.load(Ordering::SeqCst);
    let paused = control.pause.load(Ordering::SeqCst) && !stopping;
    let steps = control.step_records.lock().clone();
    let (run_dir, artifacts) = control
        .run_session
        .lock()
        .as_ref()
        .map(|s| (Some(s.run_dir_display()), Some(s.list_artifacts())))
        .unwrap_or((None, None));
    let payload = TaskProgressPayload {
        task_id: task_id.to_string(),
        task_name: task_name.to_string(),
        phase: phase.to_string(),
        current,
        total,
        label: label.to_string(),
        indeterminate,
        paused,
        stopping,
        queued,
        steps: Some(steps),
        summary: None,
        success: None,
        run_dir,
        artifacts,
        recipe_params: control.recipe_params_vec(),
    };
    *control.last_progress.lock() = Some(payload.clone());
    task_progress::emit_task_progress(app, task_id, payload);
}

fn rebuild_step_records_after_insert(
    control: &TaskControl,
    steps: &[Step],
    insert_at: usize,
    insert_count: usize,
) {
    let old = control.step_records.lock().clone();
    let mut new_recs = Vec::with_capacity(steps.len());
    for (idx, step) in steps.iter().enumerate() {
        let (status, detail) = if idx < insert_at {
            old.get(idx)
                .map(|r| (r.status.clone(), r.detail.clone()))
                .unwrap_or_else(|| ("pending".into(), String::new()))
        } else if idx >= insert_at + insert_count {
            old.get(idx - insert_count)
                .map(|r| (r.status.clone(), r.detail.clone()))
                .unwrap_or_else(|| ("pending".into(), String::new()))
        } else {
            ("pending".into(), String::new())
        };
        let params = control.recipe_params.lock().clone();
        new_recs.push(TaskStepRecord {
            index: (idx + 1) as u32,
            label: describe_step_resolved(step, &params),
            status,
            detail,
        });
    }
    *control.step_records.lock() = new_recs;
    *control.live_steps.lock() = steps.to_vec();
}

/// 应用暂停期间的回退 / 插入 / 跳过标记。返回是否有变更。
fn apply_pending_edits(steps: &mut Vec<Step>, i: &mut usize, control: &TaskControl) -> bool {
    let mut changed = false;

    if let Some(seek) = control.seek_to.lock().take() {
        let seek = seek.min(steps.len());
        *i = seek;
        {
            let mut recs = control.step_records.lock();
            for (idx, rec) in recs.iter_mut().enumerate() {
                if idx >= seek {
                    rec.status = "pending".into();
                    rec.detail.clear();
                }
            }
        }
        control.skip_set.lock().retain(|&x| x < seek);
        control.push_log(format!("回退到步骤 {}，将从此重新执行", seek + 1));
        changed = true;
    }

    let inserts = std::mem::take(&mut *control.insert_steps.lock());
    if !inserts.is_empty() {
        let n = inserts.len();
        let at = (*i).min(steps.len());
        steps.splice(at..at, inserts);
        rebuild_step_records_after_insert(control, steps, at, n);
        {
            let mut skips = control.skip_set.lock();
            *skips = skips
                .iter()
                .map(|&s| if s >= at { s + n } else { s })
                .collect();
        }
        control.push_log(format!("已插入 {n} 步指令（自步骤 {} 起）", at + 1));
        changed = true;
    }

    {
        let skips = control.skip_set.lock().clone();
        let mut recs = control.step_records.lock();
        for &si in &skips {
            if let Some(rec) = recs.get_mut(si) {
                if rec.status == "pending" {
                    rec.status = "skipped".into();
                    rec.detail = "已跳过".into();
                    changed = true;
                }
            }
        }
    }

    *control.next_index.lock() = *i;
    *control.live_steps.lock() = steps.clone();
    changed
}

async fn wait_if_paused<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    control: &TaskControl,
    task_name: &str,
    steps: &mut Vec<Step>,
    i: &mut usize,
    label: &str,
    download_port: u16,
    download_target: &str,
    client: Option<&CdpClient>,
    shield_file_chooser: bool,
) {
    let mut released_download = false;
    let mut released_chooser = false;
    while control.pause.load(Ordering::SeqCst) && !control.cancel.load(Ordering::SeqCst) {
        if !released_download {
            // 暂停期间允许用户在浏览器里手动下载 / 选文件
            crate::client_download::exit_automation_download_mode(
                download_port,
                Some(download_target),
            )
            .await;
            released_download = true;
            if shield_file_chooser {
                if let Some(c) = client {
                    let _ = c.set_intercept_file_chooser(false).await;
                }
                released_chooser = true;
            }
        }
        apply_pending_edits(steps, i, control);
        let total = steps.len() as u32;
        emit_task_progress(
            app,
            task_id,
            control,
            task_name,
            "paused",
            *i as u32,
            total,
            label,
            false,
            false,
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if released_download && !control.cancel.load(Ordering::SeqCst) {
        crate::client_download::enter_automation_download_mode(
            download_port,
            Some(download_target),
        )
        .await;
    }
    if released_chooser && !control.cancel.load(Ordering::SeqCst) {
        if let Some(c) = client {
            let _ = c.set_intercept_file_chooser(true).await;
        }
    }
    apply_pending_edits(steps, i, control);
}

pub async fn execute_recipe_for_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    task_id: &str,
    control: &TaskControl,
    recipe_name: Option<&str>,
    content: &str,
    chat_session_id: Option<&str>,
) -> Result<String, String> {
    execute_recipe_for_task_from(
        app,
        state,
        task_id,
        control,
        recipe_name,
        content,
        chat_session_id,
        None,
    )
    .await
}

/// `start_step_index`：1-based，从该步起执行（之前的步骤记为已跳过，用于失败后「继续执行」）
/// `initial_params`：外部预填参数（远程任务包 / CLI `--params`），与菜谱内「参数」合并
pub async fn execute_recipe_for_task_from<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    task_id: &str,
    control: &TaskControl,
    recipe_name: Option<&str>,
    content: &str,
    chat_session_id: Option<&str>,
    start_step_index: Option<u32>,
) -> Result<String, String> {
    execute_recipe_for_task_from_with_params(
        app,
        state,
        task_id,
        control,
        recipe_name,
        content,
        chat_session_id,
        start_step_index,
        &HashMap::new(),
    )
    .await
}

fn worker_is_resume() -> bool {
    std::env::var("WEBCLAW_RESUME_BROWSER")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn open_execute_session(
    task_id: &str,
    title: &str,
    chat_session_id: Option<&str>,
) -> Result<std::sync::Arc<crate::run_artifacts::RunSession>, String> {
    if crate::run_sink::worker_mode() {
        crate::run_artifacts::RunSession::new_or_resume(task_id, title, worker_is_resume())
    } else {
        crate::run_artifacts::RunSession::new(task_id, title, chat_session_id)
    }
}

pub async fn execute_recipe_for_task_from_with_params<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    task_id: &str,
    control: &TaskControl,
    recipe_name: Option<&str>,
    content: &str,
    chat_session_id: Option<&str>,
    start_step_index: Option<u32>,
    initial_params: &HashMap<String, String>,
) -> Result<String, String> {
    control.cancel.store(false, Ordering::SeqCst);
    control.pause.store(false, Ordering::SeqCst);
    *control.next_index.lock() = 0;
    *control.seek_to.lock() = None;
    control.skip_set.lock().clear();
    control.insert_steps.lock().clear();
    control.recipe_params.lock().clear();
    control.runtime_only_params.lock().clear();
    control.live_steps.lock().clear();

    let (content, params, runtime_only) =
        crate::recipe::apply_recipe_params(content, initial_params)?;
    *control.recipe_params.lock() = params;
    *control.runtime_only_params.lock() = runtime_only;
    let content = crate::web_systems::substitute_web_system_vars(&content)?;
    let mut steps = if content.trim().is_empty() {
        Vec::new()
    } else {
        parse_recipe_steps(&content)?
    };
    // 静态「参数/动态参数」已在 apply 中剥离；含 {{页签地址}} 等的步骤会保留并执行
    let title = recipe_name.unwrap_or("未命名快捷指令");

    // 仅写入/覆盖参数，无浏览器步骤
    if steps.is_empty() {
        let params_snap = control.recipe_params.lock().clone();
        if params_snap.is_empty() {
            return Err("快捷指令内容为空".into());
        }
        let detail = crate::recipe::params_to_ordered_vec(&params_snap)
            .into_iter()
            .map(|e| format!("{}={}", e.name, e.value))
            .collect::<Vec<_>>()
            .join("，");
        let session = open_execute_session(task_id, title, chat_session_id)?;
        *control.run_session.lock() = Some(session.clone());
        run_artifacts::emit_artifacts_update(app, &session);
        *control.live_steps.lock() = Vec::new();
        *control.step_records.lock() = Vec::new();
        run_user_log::log_user(
            control,
            state,
            app,
            format!("════ 开始 · 「{title}」写入参数 ════"),
        );
        run_user_log::log_user(control, state, app, format!("已写入参数：{detail}"));
        run_user_log::log_user(
            control,
            state,
            app,
            "════ 结束 · 参数已更新（无其它执行步骤） ════",
        );
        flush_run_logs_to_disk(app, control, &session);
        emit_task_progress(
            app,
            task_id,
            control,
            title,
            "done",
            0,
            0,
            &format!("已写入参数：{detail}"),
            false,
            false,
        );
        return Ok("done:ok=0,fail=0".into());
    }

    let total = steps.len() as u32;
    let start_i = start_step_index
        .map(|n| n.saturating_sub(1) as usize)
        .unwrap_or(0)
        .min(steps.len());

    let session = open_execute_session(task_id, title, chat_session_id)?;
    *control.run_session.lock() = Some(session.clone());
    if crate::run_sink::worker_mode() {
        let human_dir = session.run_dir.join("human");
        let _ = std::fs::create_dir_all(&human_dir);
        std::env::set_var("WEBCLAW_HUMAN_DIR", &human_dir);
        let _ = crate::human_bridge::FileHumanBridge {
            dir: human_dir.clone(),
        }
        .prepare();
        crate::run_sink::emit_event(crate::run_sink::RunEvent::RunStart {
            run_id: task_id.to_string(),
            title: title.to_string(),
        });
        crate::run_sink::emit_log(format!(
            "run_dir={} human_dir={} cdp_port={}",
            session.run_dir.display(),
            human_dir.display(),
            std::env::var("WEBCLAW_CDP_PORT").unwrap_or_else(|_| "19222".into())
        ));
    }
    if chat_session_id.is_some_and(|s| !s.trim().is_empty()) {
        let line = format!("产物已与对话绑定 | {}", session.run_dir_display());
        run_user_log::log_full(control, line);
    }
    run_artifacts::emit_artifacts_update(app, &session);

    *control.live_steps.lock() = steps.clone();
    let params_snap = control.recipe_params.lock().clone();
    let step_records: Vec<TaskStepRecord> = steps
        .iter()
        .enumerate()
        .map(|(i, step)| TaskStepRecord {
            index: (i + 1) as u32,
            label: describe_step_resolved(step, &params_snap),
            status: if i < start_i {
                "skipped".into()
            } else {
                "pending".into()
            },
            detail: if i < start_i {
                "此前已完成或已跳过".into()
            } else {
                String::new()
            },
        })
        .collect();
    *control.step_records.lock() = step_records;

    if start_i >= steps.len() {
        run_user_log::log_user(
            control,
            state,
            app,
            "没有剩余步骤可继续，任务已结束",
        );
        flush_run_logs_to_disk(app, control, &session);
        return Ok("done:ok=0,fail=0".into());
    }

    let header = if start_i > 0 {
        format!(
            "════ 快捷指令继续执行 ════ 「{title}」从第 {}/{} 步继续 | 产物目录: {}",
            start_i + 1,
            steps.len(),
            session.run_dir_display()
        )
    } else {
        format!(
            "════ 快捷指令执行开始 ════ 「{title}」共 {} 步 | 产物目录: {}",
            steps.len(),
            session.run_dir_display()
        )
    };
    let user_header = if start_i > 0 {
        format!(
            "════ 继续 · 「{title}」从第 {}/{} 步 ════",
            start_i + 1,
            steps.len()
        )
    } else {
        format_user_header(title, steps.len())
    };
    run_user_log::log_dual(control, state, app, header, user_header);

    let remaining = &steps[start_i..];
    let needs_browser = remaining
        .iter()
        .any(|s| commands::step_needs_browser(&s.keyword, &s.args));
    let may_launch_browser = remaining
        .iter()
        .any(|s| commands::keyword_opens_browser(&s.keyword));

    if !needs_browser {
        emit_task_progress(
            app,
            task_id,
            control,
            title,
            "step",
            0,
            total,
            "执行中（无需浏览器）…",
            total == 0,
            false,
        );
        run_user_log::log_full(control, "本段步骤无需浏览器，跳过启动");
        return run_browser_free_steps(
            app,
            state,
            task_id,
            control,
            title,
            &mut steps,
            start_i,
            &session,
        )
        .await;
    }

    emit_task_progress(
        app,
        task_id,
        control,
        title,
        "step",
        0,
        total,
        if may_launch_browser {
            "正在准备浏览器…"
        } else {
            "正在连接已有浏览器…"
        },
        total == 0,
        false,
    );

    let dl = session.run_dir.join("downloads");
    let worker_persist = crate::run_sink::worker_mode()
        || std::env::var("WEBCLAW_WORKER")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    let use_shared_debug = task_id == "main";
    // 伙伴空间 worker 无桌面，保持无头。本机 lite 默认弹出可见窗口（WEBCLAW_HEADLESS=1 可改回无头）。
    let chrome_headless = if std::env::var_os("WEBCLAW_LITE").is_some() {
        std::env::var("WEBCLAW_HEADLESS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    } else if worker_persist {
        true
    } else if use_shared_debug {
        false
    } else {
        crate::settings::background_chrome_headless()
    };

    let mut worker_chrome_reused = false;
    let debug_port = if use_shared_debug {
        let intro = format!(
            "使用共用调试浏览器 | 端口 {}（首页手动执行）",
            chrome::DEBUG_PORT
        );
        run_user_log::log_full(control, intro);

        // 仅「打开」「登录」可拉起；其余步骤只连接已运行的调试浏览器
        let ready = if may_launch_browser {
            chrome::ensure_debug_chrome_ready(&state.chrome_child, false).await
        } else {
            chrome::require_debug_chrome_ready().await
        };
        match ready {
            Ok(msg) => {
                run_user_log::log_full(control, msg);
            }
            Err(e) => {
                let line = format!("调试浏览器准备失败: {e}");
                run_user_log::log_user(control, state, app, line);
                flush_run_logs_to_disk(app, control, &session);
                return Ok("failed:chrome".into());
            }
        }
        let dl_note = format!(
            "本次产物目录 | {}",
            dl.display()
        );
        run_user_log::log_full(control, dl_note);
        *control.task_chrome.lock() = None;
        chrome::DEBUG_PORT
    } else if worker_persist {
        let resume = worker_is_resume();
        // 续跑可接上已有伙伴浏览器；新开且本段无「打开/登录」则不主动拉起
        if !may_launch_browser && !resume {
            let line =
                "本段步骤需要浏览器，但未包含「打开」或「登录」，且非继续任务。请先打开/登录后再执行。"
                    .to_string();
            run_user_log::log_user(control, state, app, line);
            flush_run_logs_to_disk(app, control, &session);
            return Ok("failed:chrome".into());
        }
        match chrome::ensure_worker_chrome(chrome_headless, resume, task_id).await {
            Ok((port, reused, msg)) => {
                worker_chrome_reused = reused;
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    if resume {
                        format!(
                            "继续任务：沿用产物目录 {}{}",
                            session.run_dir_display(),
                            if reused { " · 已接上浏览器" } else { " · 已重新打开浏览器" }
                        )
                    } else {
                        format!(
                            "新任务：已重启浏览器 · 产物目录 {}",
                            session.run_dir_display()
                        )
                    },
                );
                run_user_log::log_full(
                    control,
                    format!(
                        "{msg} | {}",
                        if chrome_headless {
                            "无头模式（--headless=new）"
                        } else {
                            "显示浏览器窗口"
                        }
                    ),
                );
                *control.task_chrome.lock() = None;
                port
            }
            Err(e) => {
                let line = format!("伙伴浏览器准备失败: {e}");
                run_user_log::log_user(control, state, app, line);
                flush_run_logs_to_disk(app, control, &session);
                return Ok("failed:chrome".into());
            }
        }
    } else {
        if !may_launch_browser {
            let line =
                "本段步骤需要浏览器，但未包含「打开」或「登录」。后台任务请先写「打开」/「登录」，或仅使用「脚本」「参数」「等待」等无浏览器步骤。"
                    .to_string();
            run_user_log::log_user(control, state, app, line);
            flush_run_logs_to_disk(app, control, &session);
            return Ok("failed:chrome".into());
        }
        let mode_note = if chrome_headless {
            "无头模式（--headless=new）"
        } else {
            "显示浏览器窗口"
        };
        run_user_log::log_full(control, format!("任务浏览器：{mode_note}"));

        let task_chrome = match chrome::ChromeInstance::spawn_for_task(task_id, &dl, chrome_headless)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                let line = format!("任务专用浏览器启动失败: {e}");
                run_user_log::log_user(control, state, app, line);
                flush_run_logs_to_disk(app, control, &session);
                return Ok("failed:chrome".into());
            }
        };
        *control.task_chrome.lock() = Some(task_chrome.clone());

        match task_chrome.ensure_ready(chrome_headless).await {
            Ok(msg) => {
                run_user_log::log_full(control, msg);
            }
            Err(e) => {
                let line = format!("浏览器/下载目录准备失败: {e}");
                run_user_log::log_user(control, state, app, line);
            }
        }
        if chrome::is_debug_port_ready_on(chrome::DEBUG_PORT).await {
            match chrome::sync_cookies_from_main_port(task_chrome.port).await {
                Ok(n) if n > 0 => {
                    run_user_log::log_full(
                        control,
                        format!("已从主调试浏览器同步 {n} 条 Cookie（登录态）"),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    run_user_log::log_full(
                        control,
                        format!("从主浏览器同步 Cookie 失败（可能需重新登录）: {e}"),
                    );
                }
            }
        }
        task_chrome.port
    };
    control.set_debug_port(debug_port);

    let t0 = std::time::Instant::now();
    let preferred_target = if use_shared_debug {
        state.last_main_target.lock().clone()
    } else {
        None
    };
    // 共享 Debug Chrome：与页面探针一致，执行前优先附着当前前台活动页签，
    // 避免临时步骤仍连在上次 last_main_target 旧标签上。
    let connect_result = async {
        if use_shared_debug {
            if let Ok(page) = chrome::find_foreground_page_on_ex(debug_port, true).await {
                match CdpClient::connect_to_any_target_on(debug_port, &page.id).await {
                    Ok(c) => return Ok((c, true)),
                    Err(e) => {
                        eprintln!(
                            "[executor] 前台页签附着失败（将回退 last_main_target）: {e}"
                        );
                    }
                }
            }
        }
        if let Some(ref tid) = preferred_target {
            if let Ok(c) = CdpClient::connect_to_target_on(debug_port, tid).await {
                return Ok((c, false));
            }
        }
        CdpClient::connect_on(debug_port)
            .await
            .map(|c| (c, false))
    }
    .await;
    let mut client = match connect_result {
        Ok((c, from_foreground)) => {
            if use_shared_debug {
                remember_shared_main_target(state, &c);
            }
            browser_context::store_snapshot(
                &state.last_browser_snapshot,
                browser_context::snapshot_from_client(&c),
            );
            let how = if from_foreground {
                "前台活动标签"
            } else {
                "已保存标签"
            };
            let line = format!(
                "CDP 已连接 | 端口 {debug_port} | {how}「{}」| {}",
                c.target_title, c.target_url
            );
            run_user_log::log_full(control, line);
            browser_session::set_active_browser(app, &state.active_browser, task_id, &c);
            let _ = browser_network::BrowserNetworkCapture::shared_for_port(debug_port).await;
            crate::client_download::enter_automation_download_mode(
                debug_port,
                Some(c.target_id.as_str()),
            )
            .await;
            // 开跑前摘掉探针 overlay，避免执行期间仍重画/连 OOPIF 把原生内存顶上去
            crate::page_probe::clear_all_probe_overlays_quiet(&c).await;
            c
        }
        Err(e) => {
            let line = format!("CDP 连接失败: {e}");
            run_user_log::log_full(control, &line);
            let total_ms = t0.elapsed().as_millis();
            let end = format!("════ 快捷指令执行结束 ════ CDP 未连接 | 总耗时 {total_ms} ms");
            run_user_log::log_user(
                control,
                state,
                app,
                format_user_footer(0, 1, total_ms),
            );
            run_user_log::log_full(control, end);
            emit_task_progress(
                app,
                task_id,
                control,
                title,
                "error",
                0,
                total,
                &format!("浏览器连接失败: {e}"),
                false,
                false,
            );
            if let Some(c) = control.task_chrome.lock().take() {
                c.shutdown();
            }
            flush_run_logs_to_disk(app, control, &session);
            return Ok("failed:cdp".into());
        }
    };

    if worker_persist {
        chrome::save_worker_last_page(task_id,&client.target_url, &client.target_title);
        let resume = std::env::var("WEBCLAW_RESUME_BROWSER")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let on_http = client.target_url.starts_with("http://")
            || client.target_url.starts_with("https://");
        if resume && (!worker_chrome_reused || !on_http) {
            if let Some(url) = chrome::load_worker_last_page(task_id) {
                if url != client.target_url {
                    match client.navigate(&url).await {
                        Ok(()) => {
                            let _ = client.wait_after_navigation(20).await;
                            let _ = client.sync_page_meta().await;
                            chrome::save_worker_last_page(
                                task_id,
                                &client.target_url,
                                &client.target_title,
                            );
                            run_user_log::log_user(
                                control,
                                state,
                                app,
                                format!(
                                    "已恢复上次页面 | 「{}」| {}",
                                    client.target_title, client.target_url
                                ),
                            );
                        }
                        Err(e) => {
                            run_user_log::log_user(
                                control,
                                state,
                                app,
                                format!("恢复上次页面失败: {e}"),
                            );
                        }
                    }
                }
            }
        }
    }

    // 任务期间自动截获并落盘；仅在「等待下载」及其前一步武装（含/不含上传均如此）
    let has_upload_step = steps
        .iter()
        .any(|s| s.keyword == "上传" || s.keyword == "飞书上传");
    session.configure_auto_fetch_for_recipe(has_upload_step);
    crate::client_download::set_active_download_trace(&session);
    let auto_download = crate::client_download::spawn_auto_download_pump(
        app.clone(),
        task_id.to_string(),
        debug_port,
        session.clone(),
        state.active_browser.clone(),
        control.cancel.clone(),
        control.pause.clone(),
        Some(control.clone()),
    );
    let memory_heartbeat = if std::env::var_os("WEBCLAW_LITE").is_some()
        && std::env::var_os("WEBCLAW_VERBOSE").is_none()
    {
        None
    } else {
        let hb = browser_memory::spawn_memory_heartbeat(
            debug_port,
            task_id.to_string(),
            state.active_browser.clone(),
            control.cancel.clone(),
            control.clone(),
        );
        run_user_log::log_full(
            control,
            format!(
                "已启用浏览器内存定时采样（每 {}s：Perf.getMetrics；每 {}min：UA内存 breakdown）",
                browser_memory::MEMORY_HEARTBEAT_INTERVAL_SECS,
                browser_memory::MEMORY_HEARTBEAT_INTERVAL_SECS
                    * browser_memory::MEMORY_UA_BREAKDOWN_EVERY_TICKS
                    / 60,
            ),
        );
        Some(hb)
    };
    let arm_note = if has_upload_step {
        "已启用下载捕获与打印拦截（window.print→PDF）；自动落盘仅在「等待下载」及其前一步武装（防误抓上传，并覆盖点击瞬时下完）。详情见 executions/download-capture.log"
    } else {
        "已启用下载捕获与打印拦截（window.print→PDF）；自动落盘仅在「等待下载」及其前一步武装（覆盖点击瞬时下完，空闲不扫 CDP）。详情见 executions/download-capture.log"
    };
    run_user_log::log_full(control, arm_note);

    let mut ok_count = 0u32;
    let mut fail_count = 0u32;
    let mut last_anchor: Option<RecoveryAnchor> = None;
    let mut focus_ctx = commands::FocusContext::default();
    let mut login_form = captcha::LoginFormSnapshot::default();
    // 上一步点击可能打开新标签但尚未切换成功时，下一步前重试
    let mut pending_popup_follow: Option<(String, HashSet<String>, bool)> = None;
    // 括号试探组：试探整段成功 → 跳过备选；试探任一步失败 → 不重试，改走备选
    let mut last_chrome_ws: Option<u64> = None;
    let mut or_try_ok: HashMap<u32, bool> = HashMap::new();
    let mut or_try_failed: HashMap<u32, bool> = HashMap::new();
    let mut ai_calls_used = 0u32;
    // 含「上传」时全程屏蔽系统文件框（对照任务级自动下载捕获）
    let shield_file_chooser = steps.iter().any(|s| s.keyword == "上传");
    if shield_file_chooser {
        match client.set_intercept_file_chooser(true).await {
            Ok(()) => {
                run_user_log::log_full(
                    control,
                    "已屏蔽系统文件选择框（本单含「上传」，执行期间生效，结束后取消）",
                );
            }
            Err(e) => {
                run_user_log::log_full(control, format!("屏蔽系统文件选择框失败: {e}"));
            }
        }
    }

    log_browser_memory(
        control,
        debug_port,
        Some(&client),
        "任务开始",
        &mut last_chrome_ws,
    )
    .await;

    let mut i = start_i;
    while i < steps.len() {
        if control.cancel.load(Ordering::SeqCst) {
            run_user_log::log_user(control, state, app, "已停止：中止后续步骤");
            break;
        }

        *control.next_index.lock() = i;
        wait_if_paused(
            app,
            task_id,
            control,
            title,
            &mut steps,
            &mut i,
            "已暂停：可插入指令，或点击步骤回退/跳过",
            client.port,
            &client.target_id,
            Some(&client),
            shield_file_chooser,
        )
        .await;

        if control.cancel.load(Ordering::SeqCst) {
            break;
        }
        if i >= steps.len() {
            break;
        }

        if control.skip_set.lock().remove(&i) {
            let idx = i + 1;
            let total_now = steps.len() as u32;
            let params_now = control.recipe_params.lock().clone();
            let step_desc = describe_step_resolved(&steps[i], &params_now);
            if let Some(rec) = control.step_records.lock().get_mut(i) {
                rec.status = "skipped".into();
                rec.detail = "已跳过".into();
                rec.label = step_desc.clone();
            }
            run_user_log::log_user(
                control,
                state,
                app,
                format!("── 步骤 {idx}/{total_now} ── 已跳过：{step_desc}"),
            );
            emit_task_progress(
                app,
                task_id,
                control,
                title,
                "step",
                idx as u32,
                total_now,
                &format!("已跳过：{step_desc}"),
                false,
                false,
            );
            i += 1;
            continue;
        }

        // 括号试探组：按试探成败跳过对应分支
        if let Some(og) = steps[i].or_group.clone() {
            let skip_reason = match og.branch {
                OrBranch::Try if or_try_failed.get(&og.id) == Some(&true) => {
                    Some("试探已失败，跳过其余试探步")
                }
                OrBranch::Fallback if or_try_ok.get(&og.id) == Some(&true) => {
                    Some("试探已成功，跳过备选")
                }
                _ => None,
            };
            if let Some(reason) = skip_reason {
                let idx = i + 1;
                let total_now = steps.len() as u32;
                let params_now = control.recipe_params.lock().clone();
                let step_desc = describe_step_resolved(&steps[i], &params_now);
                if let Some(rec) = control.step_records.lock().get_mut(i) {
                    rec.status = "skipped".into();
                    rec.detail = reason.into();
                    rec.label = step_desc.clone();
                }
                // 试探跳过不刷用户日志（仅记全量日志，避免啰嗦）
                run_user_log::log_full(
                    control,
                    format!("步骤 {idx}/{total_now} {reason}：{step_desc}"),
                );
                emit_task_progress(
                    app,
                    task_id,
                    control,
                    title,
                    "step",
                    idx as u32,
                    total_now,
                    &format!("{reason}：{step_desc}"),
                    false,
                    false,
                );
                i += 1;
                continue;
            }
        }

        let step_tmpl = steps[i].clone();
        let params_now = control.recipe_params.lock().clone();
        // 展开 {{页签地址}} / {{页签标题}} / {{剪切板}} 等内置动态占位（及参数值中的嵌套引用）
        let _ = client.sync_page_meta().await;
        let clipboard = commands::read_system_clipboard();
        let runtime = crate::recipe::RuntimeParamCtx {
            tab_url: &client.target_url,
            tab_title: &client.target_title,
            clipboard: &clipboard,
        };
        let resolved_args = crate::recipe::resolve_step_args_ex(
            &step_tmpl.args,
            &params_now,
            Some(&runtime),
        );
        let mut step = Step {
            keyword: step_tmpl.keyword.clone(),
            args: resolved_args,
            or_group: step_tmpl.or_group.clone(),
        };
        let total = steps.len() as u32;
        let idx = i + 1;
        let mut step_desc = describe_step(&step);

        // 自动落盘仅在「等待下载」及其前一步武装，避免点击瞬时下完却尚未武装
        if session.is_auto_fetch_wait_only() {
            let next_is_wait_dl = steps
                .get(i + 1)
                .is_some_and(commands::is_wait_download_step);
            let cur_is_wait_dl = commands::is_wait_download_step(&step);
            let should_arm = cur_is_wait_dl || next_is_wait_dl;
            let was_armed = session.is_auto_fetch_armed();
            if should_arm && !was_armed {
                // 新窗口：清掉上一轮迟到/未确认落盘，否则下一轮「等待下载」会 8ms 误判成功
                session.begin_wait_download_window();
                session.set_auto_fetch_armed(true);
                let why = if cur_is_wait_dl {
                    "当前为等待下载"
                } else {
                    "下一步为等待下载"
                };
                let line = format!("下载截获 | 已武装自动落盘（{why}）");
                session.append_download_trace(&line);
                run_user_log::log_full(control, &line);
                let _ = browser_network::arm_ephemeral_download_capture(debug_port).await;
                // 在「点击导出」执行前装好钩子/deny，避免瞬时下载发生在泵醒来之前
                let _ = crate::client_download::ensure_automation_capture(&client).await;
            } else {
                session.set_auto_fetch_armed(should_arm);
                if !should_arm && was_armed {
                    let line = "下载截获 | 已解除自动落盘武装";
                    session.append_download_trace(line);
                    run_user_log::log_full(control, line);
                }
            }
        }

        if step.keyword == "输入" {
            login_form.absorb_input_step(&step.args);
        }
        if step.keyword == "登录" {
            if let Ok(system) = crate::web_systems::find_web_system_by_query(&step.args) {
                login_form.phone = Some(system.username.clone());
                login_form.password = Some(system.password.clone());
            }
        }

        if let Some(rec) = control.step_records.lock().get_mut(i) {
            rec.status = "running".into();
            rec.label = step_desc.clone();
        }

        let head = format!("── 步骤 {idx}/{} ── {step_desc}", steps.len());
        run_user_log::log_user(control, state, app, head);
        log_browser_memory(
            control,
            debug_port,
            Some(&client),
            &format!("步骤 {idx} 前"),
            &mut last_chrome_ws,
        )
        .await;
        emit_task_progress(
            app,
            task_id,
            control,
            title,
            "step",
            idx as u32,
            total,
            &step_desc,
            false,
            false,
        );

        if let Some((ref parent, ref pages_before, may_open)) = pending_popup_follow.clone() {
            match client
                .try_follow_opener_child_tab_cancel(
                    parent,
                    Some(pages_before),
                    may_open,
                    Some(&control.cancel),
                )
                .await
            {
                Ok(true) => {
                    pending_popup_follow = None;
                    if use_shared_debug {
                        remember_shared_main_target(state, &client);
                    }
                    browser_context::store_snapshot(
                        &state.last_browser_snapshot,
                        browser_context::snapshot_from_client(&client),
                    );
                    browser_session::set_active_browser(
                        app,
                        &state.active_browser,
                        task_id,
                        &client,
                    );
                    let line = format!(
                        "步骤 {idx} 已切换到子标签 | {}",
                        client.target_url
                    );
                    run_user_log::log_full(control, line);
                }
                Ok(false) => {
                    if !client
                        .popup_follow_pending(parent, Some(pages_before))
                        .await
                        .unwrap_or(false)
                    {
                        pending_popup_follow = None;
                    }
                }
                Err(e) => {
                    let line = format!("步骤 {idx} 子标签切换: {e}");
                    run_user_log::log_full(control, line);
                }
            }
        }

        let t_step = std::time::Instant::now();
        let max_reloads = if std::env::var_os("WEBCLAW_LITE").is_some() {
            0
        } else if page_recovery::is_recoverable(&step) {
            page_recovery::MAX_ATTEMPTS.saturating_sub(1)
        } else {
            0
        };
        #[allow(unused_assignments)]
        let mut result: Result<String, String> = Err("未执行".into());
        let mut reloads_used = 0u32;
        let mut browser_restarts_used = 0u32;
        let mut ai_rounds = 0u32;
        let mut ai_repaired = false;
        let mut pending_ai_restart: Option<usize> = None;
        let mut last_repair_plan: Option<ark::RepairPlan> = None;
        let mut repair_notes: Vec<String> = Vec::new();
        let cache_step = step.clone();
        let mut last_fail_err = String::new();
        let mut parent_target = String::new();
        let mut pages_before_click: Option<HashSet<String>> = None;
        let mut do_reload = false;
        let mut probe_dump_at_fail: Option<String> = None;
        let mut probe_dump_after_recovery: Option<String> = None;
        let mut probe_page_at_fail: Option<crate::page_probe::ProbePageResult> = None;
        let mut probe_page_after_recovery: Option<crate::page_probe::ProbePageResult> = None;
        const MAX_BROWSER_RESTARTS_PER_STEP: u32 = 1;

        loop {
            if control.cancel.load(Ordering::SeqCst) {
                result = Err("已取消".into());
                break;
            }
            if do_reload {
                let recovery_anchor =
                    page_recovery::recovery_anchor_for_step(&step, last_anchor.as_ref());
                let anchor = recovery_anchor.as_ref();
                let attempt_n = reloads_used + 1;
                let recover_note = match anchor {
                    Some(RecoveryAnchor::Text(t)) => {
                        format!(
                            "步骤 {idx} 失败，自动刷新并重试（第 {attempt_n}/{} 次）| 等待锚点「{t}」",
                            max_reloads
                        )
                    }
                    Some(RecoveryAnchor::Structural(t)) => {
                        format!(
                            "步骤 {idx} 失败，自动刷新并重试（第 {attempt_n}/{} 次）| 等待锚点「{t}」",
                            max_reloads
                        )
                    }
                    Some(RecoveryAnchor::Selector(s)) => {
                        format!(
                            "步骤 {idx} 失败，自动刷新并重试（第 {attempt_n}/{} 次）| 等待选择器「{s}」",
                            max_reloads
                        )
                    }
                    None => format!(
                        "步骤 {idx} 失败，自动刷新并重试（第 {attempt_n}/{} 次）",
                        max_reloads
                    ),
                };
                run_user_log::log_full(control, with_step_timing(recover_note, t_step));
                log_browser_memory(
                    control,
                    debug_port,
                    Some(&client),
                    &format!("步骤 {idx} 刷新恢复前"),
                    &mut last_chrome_ws,
                )
                .await;

                match page_recovery::recover_page(
                    &mut client,
                    &session,
                    idx as u32,
                    attempt_n,
                    anchor,
                    Some(&control.cancel),
                )
                .await
                {
                    Ok(()) => {
                        run_user_log::log_full(
                            control,
                            with_step_timing(format!("步骤 {idx} 页面已刷新并就绪"), t_step),
                        );
                        reloads_used += 1;
                        // 刷新恢复后再扫一次，对照首次失败时的序号/class
                        if let Some((dump, page)) = capture_probe_phase_to_log(
                            control,
                            &client,
                            "刷新重试后",
                        )
                        .await
                        {
                            probe_dump_after_recovery = Some(dump);
                            probe_page_after_recovery = Some(page);
                        }
                    }
                    Err(e) => {
                        run_user_log::log_user(
                            control,
                            state,
                            app,
                            with_step_timing(
                                format!(
                                    "步骤 {idx} 自动恢复失败（{attempt_n}/{max_reloads}）: {e}"
                                ),
                                t_step,
                            ),
                        );
                        result = Err(e);
                        break;
                    }
                }
                do_reload = false;
            }

            // 跟随用户当前活动页签（策略见 should_follow_active_tab / RunnerKind::follows_active_tab）
            let follow = commands::get_by_keyword(&step.keyword)
                .ok()
                .flatten()
                .map(|c| commands::should_follow_active_tab(c.runner, &step.keyword, &step.args))
                .unwrap_or_else(|| {
                    // 未注册关键词时仍跟，避免卡在旧标签；「等待下载」仍豁免
                    commands::should_follow_active_tab(
                        commands::RunnerKind::JavaScript,
                        &step.keyword,
                        &step.args,
                    )
                });
            if follow {
                match client.sync_to_active_tab().await {
                    Ok(true) => {
                        if use_shared_debug {
                            remember_shared_main_target(state, &client);
                        }
                        browser_context::store_snapshot(
                            &state.last_browser_snapshot,
                            browser_context::snapshot_from_client(&client),
                        );
                        browser_session::set_active_browser(
                            app,
                            &state.active_browser,
                            task_id,
                            &client,
                        );
                        let line = format!(
                            "步骤 {idx} 已跟随活动标签 | 「{}」| {}",
                            client.target_title, client.target_url
                        );
                        run_user_log::log_full(control, line);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        let line = format!("步骤 {idx} 跟随活动标签: {e}");
                        run_user_log::log_full(control, line);
                    }
                }
            }

            let _ = client.ensure_attached().await;
            if shield_file_chooser {
                let _ = client.set_intercept_file_chooser(true).await;
            }

            if matches!(step.keyword.as_str(), "点击" | "打开" | "登录") {
                let _ = browser_network::BrowserNetworkCapture::shared_for_port(debug_port).await;
                browser_network::set_watch_opener_on(debug_port, &client.target_id).await;
                parent_target = client.target_id.clone();
                pages_before_click = browser_network::snapshot_page_ids_on(debug_port)
                    .await
                    .ok();
            }

            result = execute_step(
                app,
                state,
                task_id,
                control,
                Some(&mut client),
                &step,
                Some(&session),
                Some(idx as u32),
                Some(&control.cancel),
                &mut focus_ctx,
            )
            .await;

            if result.is_ok() {
                break;
            }
            let cancelled = control.cancel.load(Ordering::SeqCst)
                || result
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.contains("已取消") || e.contains("已停止"));
            if cancelled {
                result = Err("已取消".into());
                break;
            }

            let err = result.as_ref().err().cloned().unwrap_or_default();
            let class = ai_repair::classify_error(&err);
            if class == ErrorClass::Cancelled {
                result = Err("已取消".into());
                break;
            }

            // 等待超时 / 未命中：在 AI / 刷新前落一条带本步已耗时的中间日志，便于对照是否等够
            let miss_kind = if err.contains("等待超时")
                || (err.contains("等待") && err.contains("仍未找到"))
                || err.contains("实际轮询")
            {
                "等待超时"
            } else if err.contains("未找到") {
                "未命中"
            } else {
                "本次失败"
            };
            run_user_log::log_full(
                control,
                with_step_timing(format!("步骤 {idx} {miss_kind} | {err}"), t_step),
            );

            // 首次失败瞬间立刻扫探针（必须在刷新/AI 修复之前），供对照与 AI 使用
            // 断言失败不做页面修复 / AI 分析，跳过探针采集
            if step.keyword != "断言"
                && settings::ai_enhance_enabled()
                && probe_dump_at_fail.is_none()
                && !ai_repair::needs_browser_restart(&err)
                && chrome::is_debug_port_ready_on(debug_port).await
            {
                if let Some((dump, page)) =
                    capture_probe_phase_to_log(control, &client, "首次失败时").await
                {
                    probe_dump_at_fail = Some(dump);
                    probe_page_at_fail = Some(page);
                }
            }

            let ai_on = settings::ai_enhance_enabled();
            let can_ai = step.keyword != "断言"
                && ai_on
                && ai_repair::is_repairable_step(&step)
                && ai_rounds < ai_repair::MAX_AI_ROUNDS_PER_STEP
                && ai_calls_used < ai_repair::MAX_AI_CALLS_PER_RUN;

            // 浏览器进程已挂死/OOM：刷新无效，强制重启再让 AI 介入
            // 断言失败不自动重启浏览器重试
            let port_down = !chrome::is_debug_port_ready_on(debug_port).await;
            let need_browser_restart = step.keyword != "断言"
                && class == ErrorClass::DeadPage
                && (ai_repair::needs_browser_restart(&err) || port_down)
                && browser_restarts_used < MAX_BROWSER_RESTARTS_PER_STEP
                && !control.cancel.load(Ordering::SeqCst);

            if need_browser_restart {
                browser_restarts_used += 1;
                last_fail_err = err.clone();
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    with_step_timing(
                        format!(
                            "检测到浏览器无响应（可能内存不足 OOM）。正在强制重启浏览器并尝试恢复第 {idx} 步…"
                        ),
                        t_step,
                    ),
                );
                match restart_browser_after_crash(
                    app,
                    state,
                    control,
                    &mut client,
                    debug_port,
                    use_shared_debug,
                    chrome_headless,
                    task_id,
                )
                .await
                {
                    Ok(()) => {
                        repair_notes.push(format!(
                            "浏览器已强制重启（端口 {debug_port}）；原错误: {err}"
                        ));
                        if can_ai {
                            let oom_err = format!(
                                "浏览器崩溃或无响应（疑似 OOM），已强制重启。原错误: {err}"
                            );
                            last_fail_err = oom_err.clone();
                            ai_rounds += 1;
                            run_user_log::log_user(
                                control,
                                state,
                                app,
                                with_step_timing(
                                    format!(
                                        "AI 正在分析浏览器崩溃后的恢复方案（第 {idx} 步）…"
                                    ),
                                    t_step,
                                ),
                            );
                            let proposed = ai_repair::propose_repair(
                                &mut client,
                                control,
                                &steps,
                                i,
                                &step,
                                &step_desc,
                                &oom_err,
                                true,
                                &repair_notes,
                                probe_dump_at_fail.as_deref(),
                                probe_dump_after_recovery.as_deref(),
                                probe_page_after_recovery
                                    .as_ref()
                                    .or(probe_page_at_fail.as_ref()),
                            )
                            .await;
                            match proposed {
                                Ok(out) => {
                                    if matches!(out.source, ProposeSource::Model) {
                                        ai_calls_used += 1;
                                    }
                                    let summary = ai_repair::summarize_plan(&out.plan);
                                    match ai_repair::apply_repair_plan(
                                        &out.plan,
                                        &mut steps,
                                        i,
                                        control,
                                    ) {
                                        RepairApply::RetryCurrent => {
                                            ai_repaired = true;
                                            last_repair_plan = Some(out.plan.clone());
                                            repair_notes.push(summary.clone());
                                            ai_repair::emit_live_recipe(app, task_id, &steps);
                                            run_user_log::log_user(
                                                control,
                                                state,
                                                app,
                                                with_step_timing(
                                                    format!("AI 已{summary}（崩溃恢复）"),
                                                    t_step,
                                                ),
                                            );
                                            let tmpl = steps[i].clone();
                                            let params_now = control.recipe_params.lock().clone();
                                            let _ = client.sync_page_meta().await;
                                            let clipboard = commands::read_system_clipboard();
                                            let runtime = crate::recipe::RuntimeParamCtx {
                                                tab_url: &client.target_url,
                                                tab_title: &client.target_title,
                                                clipboard: &clipboard,
                                            };
                                            let resolved_args =
                                                crate::recipe::resolve_step_args_ex(
                                                    &tmpl.args,
                                                    &params_now,
                                                    Some(&runtime),
                                                );
                                            step = Step {
                                                keyword: tmpl.keyword,
                                                args: resolved_args,
                                                or_group: tmpl.or_group,
                                            };
                                            step_desc = describe_step(&step);
                                        }
                                        RepairApply::RestartFrom(new_i) => {
                                            ai_repaired = true;
                                            last_repair_plan = Some(out.plan);
                                            ai_repair::emit_live_recipe(app, task_id, &steps);
                                            run_user_log::log_user(
                                                control,
                                                state,
                                                app,
                                                with_step_timing(
                                                    format!("AI 已{summary}（崩溃恢复）"),
                                                    t_step,
                                                ),
                                            );
                                            pending_ai_restart = Some(new_i);
                                            break;
                                        }
                                        RepairApply::Skip { detail } => {
                                            ai_repaired = true;
                                            last_repair_plan = Some(out.plan);
                                            run_user_log::log_user(
                                                control,
                                                state,
                                                app,
                                                with_step_timing(
                                                    format!("AI 已跳过第 {idx} 步（{detail}）"),
                                                    t_step,
                                                ),
                                            );
                                            result = Ok(detail);
                                            break;
                                        }
                                        RepairApply::ReloadThenRetry => {
                                            ai_repaired = true;
                                            last_repair_plan = Some(out.plan);
                                            repair_notes.push(summary);
                                            // 刚重启过，直接重试当前步
                                        }
                                        RepairApply::NeedHuman { reason }
                                        | RepairApply::Noop { reason } => {
                                            repair_notes.push(reason.clone());
                                            run_user_log::log_user(
                                                control,
                                                state,
                                                app,
                                                with_step_timing(
                                                    format!(
                                                        "AI 未能自动给出崩溃恢复补丁：{reason}。将直接重试当前步；若需登录请手工完成或点「继续」。"
                                                    ),
                                                    t_step,
                                                ),
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    if e.contains("已取消") {
                                        result = Err("已取消".into());
                                        break;
                                    }
                                    ai_calls_used += 1;
                                    repair_notes.push(format!("AI 崩溃恢复失败: {e}"));
                                    run_user_log::log_full(
                                        control,
                                        with_step_timing(
                                            format!("AI 崩溃恢复失败: {e}"),
                                            t_step,
                                        ),
                                    );
                                }
                            }
                        }
                        // 重启后重试当前步（可能已插入登录等）
                        continue;
                    }
                    Err(e) => {
                        run_user_log::log_user(
                            control,
                            state,
                            app,
                            format!(
                                "浏览器重启失败: {e}。请点击「重启调试浏览器」或「显示浏览器」后，再点「继续当前步骤」。"
                            ),
                        );
                        result = Err(format!("浏览器无响应且重启失败: {e}（原错误: {err}）"));
                        break;
                    }
                }
            }

            if class == ErrorClass::DeadPage && reloads_used < max_reloads {
                // 端口已死时不要浪费刷新预算
                if chrome::is_debug_port_ready_on(debug_port).await {
                    do_reload = true;
                    continue;
                }
            }

            if can_ai {
                last_fail_err = err.clone();
                ai_rounds += 1;
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    with_step_timing(
                        format!("AI 正在修复第 {idx} 步（第 {ai_rounds} 轮）…"),
                        t_step,
                    ),
                );
                log_browser_memory(
                    control,
                    debug_port,
                    Some(&client),
                    &format!("步骤 {idx} AI修复前"),
                    &mut last_chrome_ws,
                )
                .await;
                let skip_cache = ai_rounds > 1;
                let reuse_probe = probe_page_after_recovery
                    .as_ref()
                    .or(probe_page_at_fail.as_ref());
                let proposed = if skip_cache {
                    // 第二轮跳过缓存，避免反复套用同一失败补丁
                    let (ctx, png) = ai_repair::gather_repair_context(
                        &mut client,
                        control,
                        &steps,
                        i,
                        &step_desc,
                        &err,
                        reloads_used > 0,
                        &repair_notes,
                        probe_dump_at_fail.as_deref(),
                        probe_dump_after_recovery.as_deref(),
                        reuse_probe,
                    )
                    .await;
                    if control.cancel.load(Ordering::SeqCst) {
                        result = Err("已取消".into());
                        break;
                    }
                    ai_calls_used += 1;
                    tokio::time::timeout(
                        std::time::Duration::from_secs(45),
                        ark::propose_step_repair(
                            &ctx,
                            png.as_deref(),
                            Some(control.cancel.clone()),
                        ),
                    )
                    .await
                    .map_err(|_| "AI 修复超时".to_string())
                    .and_then(|r| r)
                    .map(|plan| ai_repair::ProposeOutcome {
                        plan,
                        source: ProposeSource::Model,
                    })
                } else {
                    match ai_repair::propose_repair(
                        &mut client,
                        control,
                        &steps,
                        i,
                        &step,
                        &step_desc,
                        &err,
                        reloads_used > 0,
                        &repair_notes,
                        probe_dump_at_fail.as_deref(),
                        probe_dump_after_recovery.as_deref(),
                        reuse_probe,
                    )
                    .await
                    {
                        Ok(out) => {
                            if matches!(out.source, ProposeSource::Model) {
                                ai_calls_used += 1;
                            }
                            Ok(out)
                        }
                        Err(e) => Err(e),
                    }
                };

                match proposed {
                    Ok(out) => {
                        let summary = ai_repair::summarize_plan(&out.plan);
                        match ai_repair::apply_repair_plan(&out.plan, &mut steps, i, control) {
                            RepairApply::NeedHuman { reason } => {
                                run_user_log::log_user(
                                    control,
                                    state,
                                    app,
                                    with_step_timing(
                                        format!("AI 无法自动修复：{reason}"),
                                        t_step,
                                    ),
                                );
                            }
                            RepairApply::Noop { reason } => {
                                let msg = if reason.trim().is_empty() {
                                    "AI 无法自动修复：计划无效".into()
                                } else {
                                    format!("AI 无法自动修复：{reason}")
                                };
                                run_user_log::log_user(
                                    control,
                                    state,
                                    app,
                                    with_step_timing(msg, t_step),
                                );
                            }
                            RepairApply::Skip { detail } => {
                                ai_repaired = true;
                                last_repair_plan = Some(out.plan);
                                run_user_log::log_user(
                                    control,
                                    state,
                                    app,
                                    with_step_timing(
                                        format!("AI 已跳过第 {idx} 步（{detail}）"),
                                        t_step,
                                    ),
                                );
                                result = Ok(detail);
                                break;
                            }
                            RepairApply::RestartFrom(new_i) => {
                                ai_repaired = true;
                                last_repair_plan = Some(out.plan);
                                ai_repair::emit_live_recipe(app, task_id, &steps);
                                run_user_log::log_user(
                                    control,
                                    state,
                                    app,
                                    with_step_timing(format!("AI 已{summary}"), t_step),
                                );
                                pending_ai_restart = Some(new_i);
                                break;
                            }
                            RepairApply::RetryCurrent => {
                                ai_repaired = true;
                                last_repair_plan = Some(out.plan.clone());
                                repair_notes.push(summary.clone());
                                ai_repair::emit_live_recipe(app, task_id, &steps);
                                run_user_log::log_user(
                                    control,
                                    state,
                                    app,
                                    with_step_timing(format!("AI 已{summary}"), t_step),
                                );
                                let tmpl = steps[i].clone();
                                let params_now = control.recipe_params.lock().clone();
                                let _ = client.sync_page_meta().await;
                                let clipboard = commands::read_system_clipboard();
                                let runtime = crate::recipe::RuntimeParamCtx {
                                    tab_url: &client.target_url,
                                    tab_title: &client.target_title,
                                    clipboard: &clipboard,
                                };
                                let resolved_args = crate::recipe::resolve_step_args_ex(
                                    &tmpl.args,
                                    &params_now,
                                    Some(&runtime),
                                );
                                step = Step {
                                    keyword: tmpl.keyword,
                                    args: resolved_args,
                                    or_group: tmpl.or_group,
                                };
                                step_desc = describe_step(&step);
                                continue;
                            }
                            RepairApply::ReloadThenRetry => {
                                ai_repaired = true;
                                last_repair_plan = Some(out.plan);
                                repair_notes.push(summary.clone());
                                run_user_log::log_user(
                                    control,
                                    state,
                                    app,
                                    with_step_timing(format!("AI 已{summary}"), t_step),
                                );
                                if reloads_used < max_reloads.max(1) {
                                    do_reload = true;
                                    continue;
                                }
                            }
                        }
                    }
                    Err(e) if e.contains("已取消") => {
                        result = Err("已取消".into());
                        break;
                    }
                    Err(e) => {
                        run_user_log::log_user(
                            control,
                            state,
                            app,
                            with_step_timing(format!("AI 无法自动修复：{e}"), t_step),
                        );
                    }
                }
            }

            if page_recovery::is_recoverable(&step)
                && reloads_used < max_reloads
                && !page_recovery::is_unrecoverable_error(&err)
            {
                do_reload = true;
                continue;
            }
            break;
        }

        if let Some(new_i) = pending_ai_restart {
            i = new_i;
            continue;
        }

        let step_ms = t_step.elapsed().as_millis();
        let step_ok = result.is_ok();
        let mut fail_err: Option<String> = None;

        match result {
            Ok(detail) => {
                let _ = client.sync_page_meta().await;
                if use_shared_debug {
                    remember_shared_main_target(state, &client);
                }
                if worker_persist {
                    chrome::save_worker_last_page(task_id,&client.target_url, &client.target_title);
                }
                browser_context::store_snapshot(
                    &state.last_browser_snapshot,
                    browser_context::snapshot_from_client(&client),
                );
                browser_session::set_active_browser(app, &state.active_browser, task_id, &client);
                run_artifacts::emit_artifacts_update(app, &session);
                ok_count += 1;
                let full_line = format!(
                    "步骤 {idx} 成功 | {detail} | @ {} · 耗时 {step_ms} ms",
                    run_user_log::clock_stamp()
                );
                let user_line = format_user_step_ok(
                    idx as u32,
                    total,
                    &step_desc,
                    &detail,
                    reloads_used > 0,
                    ai_repaired,
                );
                run_user_log::log_dual(control, state, app, full_line, user_line);
                if let Some(rec) = control.step_records.lock().get_mut(i) {
                    rec.status = "ok".into();
                    rec.detail = user_step_summary(&detail);
                    if reloads_used > 0 {
                        rec.detail = format!("{} · 已自动恢复", rec.detail);
                    }
                    if ai_repaired {
                        rec.detail = format!("{} · 已 AI 修复", rec.detail);
                    }
                }
                if let Some(plan) = last_repair_plan.as_ref() {
                    let host = browser_context::hostname_from_url(&client.target_url);
                    let key = ai_repair::cache_fingerprint(&host, &cache_step, &last_fail_err);
                    ai_repair::remember_successful_plan(&key, plan);
                }
                if let Some(anchor) = page_recovery::anchor_after_success(&step) {
                    last_anchor = Some(anchor);
                }
                // 试探分支最后一步成功 → 整组试探成功，后续备选将被跳过
                if let Some(og) = &step.or_group {
                    if og.branch == OrBranch::Try {
                        let more_try = steps.get(i + 1).is_some_and(|next| {
                            next.or_group.as_ref().is_some_and(|g| {
                                g.id == og.id && g.branch == OrBranch::Try
                            })
                        });
                        if !more_try {
                            or_try_ok.insert(og.id, true);
                            run_user_log::log_full(
                                control,
                                format!("步骤 {idx} 试探成功，跳过同组备选分支"),
                            );
                        }
                    }
                }
            }
            Err(e) => {
                // 试探失败：不中断；有备选则改走备选，无备选则继续后续步骤
                if step.is_or_try() {
                    let gid = step.or_group.as_ref().map(|g| g.id);
                    if let Some(id) = gid {
                        or_try_failed.insert(id, true);
                    }
                    let has_fallback = gid.is_some_and(|id| {
                        steps.iter().any(|s| {
                            s.or_group.as_ref().is_some_and(|g| {
                                g.id == id && g.branch == OrBranch::Fallback
                            })
                        })
                    });
                    let (full_tail, user_tail, detail) = if has_fallback {
                        (
                            "→ 改走备选",
                            "试探未命中，改走备选",
                            format!("试探未命中，改走备选 | {e}"),
                        )
                    } else {
                        (
                            "→ 无备选，继续",
                            "试探未命中，已跳过",
                            format!("试探未命中（无备选）| {e}"),
                        )
                    };
                    let full_line = format!(
                        "步骤 {idx} 试探未命中 | @ {} · 耗时 {step_ms} ms | {e} {full_tail}",
                        run_user_log::clock_stamp()
                    );
                    let user_line = format!("步骤 {idx}/{total} {user_tail}");
                    run_user_log::log_dual(control, state, app, full_line, user_line);
                    if let Some(rec) = control.step_records.lock().get_mut(i) {
                        rec.status = "skipped".into();
                        rec.detail = detail;
                    }
                    emit_task_progress(
                        app,
                        task_id,
                        control,
                        title,
                        "step",
                        idx as u32,
                        total,
                        &step_desc,
                        false,
                        false,
                    );
                    i += 1;
                    continue;
                }

                fail_count += 1;
                let full_line = format!(
                    "步骤 {idx} 失败 | @ {} · 耗时 {step_ms} ms | 错误: {e}",
                    run_user_log::clock_stamp()
                );
                let user_line = format_user_step_fail(idx as u32, total, &step_desc, &e);
                run_user_log::log_dual(control, state, app, full_line, user_line);
                if let Some(rec) = control.step_records.lock().get_mut(i) {
                    rec.status = "fail".into();
                    rec.detail = e.clone();
                }
                let _ = session.sync_files();
                run_artifacts::emit_artifacts_update(app, &session);
                fail_err = Some(e);
            }
        }
        emit_task_progress(
            app,
            task_id,
            control,
            title,
            "step",
            idx as u32,
            total,
            &step_desc,
            false,
            false,
        );

        if control.cancel.load(Ordering::SeqCst) {
            break;
        }

        // 任一步失败即中断，避免在错误状态下继续执行后续步骤
        if !step_ok {
            let fail_msg = fail_err.as_deref().unwrap_or("");
            let is_assert_step = step.keyword == "断言";
            if is_assert_step {
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    format!("第 {idx} 步断言未通过，已中断后续步骤（不触发页面修复 / AI 分析）。"),
                );
            } else if crate::run_sink::worker_mode() {
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    format!("第 {idx} 步失败，已停止后续步骤。"),
                );
            } else if ai_repair::needs_browser_restart(fail_msg)
                || !chrome::is_debug_port_ready_on(debug_port).await
            {
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    format!(
                        "第 {idx} 步失败：浏览器可能已因内存不足崩溃。可点击「重启调试浏览器」后「继续当前步骤」；若开启 AI 增强，系统会尝试分析原因。"
                    ),
                );
            } else {
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    format!(
                        "第 {idx} 步失败，已暂停后续步骤。请在浏览器中手工完成该步骤后，点击「继续当前步骤」或「继续下一步骤」。"
                    ),
                );
            }
            maybe_analyze_step_failure(
                app,
                state,
                control,
                &mut client,
                &session,
                i,
                idx as u32,
                &step_desc,
                fail_msg,
                reloads_used > 0 || browser_restarts_used > 0,
                probe_dump_at_fail.as_deref(),
                probe_dump_after_recovery.as_deref(),
                probe_page_after_recovery
                    .as_ref()
                    .or(probe_page_at_fail.as_ref()),
                is_assert_step,
            )
            .await;
            break;
        }

        if step.keyword == "打开" || step.keyword == "点击" || step.keyword == "打开标签" || step.keyword == "登录" {
            let is_pixel_click = step.keyword == "点击"
                && crate::commands::scripts::is_pixel_click_args(&step.args);
            let nav_secs = if is_pixel_click {
                // 坐标点空白关弹层：不整页导航，短等即可，避免 8s 空等期间前台页签漂移
                if step.is_or_try() { 1 } else { 2 }
            } else if step.keyword == "点击"
                && (is_login_submit_click_target(&step.args)
                    || norm_ui_contains(&step.args, "提交"))
            {
                20
            } else if step.keyword == "登录" {
                20
            } else if step.is_or_try() {
                // 试探命中后页面通常已就绪，短等即可
                3
            } else {
                8
            };
            let cancel_ref = control.cancel.as_ref();
            if let Err(e) = client
                .wait_after_navigation_cancel(nav_secs, Some(cancel_ref))
                .await
            {
                let line = format!("步骤 {idx} 导航等待: {e}");
                run_user_log::log_full(control, line);
                if e.contains("已取消") || control.cancel.load(Ordering::SeqCst) {
                    break;
                }
            }
            if control.cancel.load(Ordering::SeqCst) {
                break;
            }
            let _ = client.sync_page_meta().await;
            browser_session::set_active_browser(app, &state.active_browser, task_id, &client);
            // 已武装等待下载时：父页勿跑完整 ensure（打印预览会卡住 CDP evaluate 十余秒）。
            // 子页靠 pause+waitForDebugger 在 resume 前注入 print 钩子。
            if session.is_auto_fetch_armed() {
                let _ = crate::print_pdf::dismiss_native_print_ui(client.port).await;
            } else {
                let _ = crate::client_download::ensure_automation_capture(&client).await;
            }

            if matches!(step.keyword.as_str(), "点击" | "打开" | "登录") {
                let may_open_tab = client.click_may_open_tab;
                match client
                    .try_follow_opener_child_tab_cancel(
                        &parent_target,
                        pages_before_click.as_ref(),
                        may_open_tab,
                        Some(cancel_ref),
                    )
                    .await
                {
                    Ok(true) => {
                        pending_popup_follow = None;
                        if use_shared_debug {
                            remember_shared_main_target(state, &client);
                        }
                        browser_context::store_snapshot(
                            &state.last_browser_snapshot,
                            browser_context::snapshot_from_client(&client),
                        );
                        browser_session::set_active_browser(
                            app,
                            &state.active_browser,
                            task_id,
                            &client,
                        );
                        let line = format!(
                            "步骤 {idx} 已切换到子标签 | {}",
                            client.target_url
                        );
                        run_user_log::log_full(control, line);
                    }
                    Ok(false) => {
                        if client
                            .popup_follow_pending(&parent_target, pages_before_click.as_ref())
                            .await
                            .unwrap_or(false)
                            || may_open_tab
                        {
                            if let Some(before) = pages_before_click.clone() {
                                pending_popup_follow =
                                    Some((parent_target.clone(), before, may_open_tab));
                            }
                        } else {
                            pending_popup_follow = None;
                        }
                    }
                    Err(e) => {
                        let line = format!("步骤 {idx} 子标签切换: {e}");
                        run_user_log::log_full(control, line);
                    }
                }
                client.click_may_open_tab = false;
                browser_network::clear_watch_opener_on(debug_port);
            }
            let _ = client.prepare_tab_for_automation().await;
        }

        let is_login_step = step.keyword == "登录";
        // 仅「点击登录」才走验证码/登录校验；「点击密码登录」等不算
        let is_login_click =
            step.keyword == "点击" && is_login_submit_click_target(&step.args);
        // 仅在真正提交登录后再处理验证码；「输入密码」只记凭据，不弹窗
        let login_scenario = is_login_step || is_login_click;
        // 「登录」指令在已登录直达业务页时会置位；勿再跑验证码/登录成败校验
        let login_already_in = is_login_step && client.skip_next_captcha;

        if login_scenario {
            if client.skip_next_captcha {
                client.skip_next_captcha = false;
            } else {
                let app_c = app.clone();
                let waiters = state.captcha_waiters.clone();
                let tid = task_id.to_string();
                let push = |line: String| {
                    run_user_log::log_user(control, state, app, line);
                };
                if let Err(e) = captcha::resolve_if_present(
                    Some(&app_c),
                    &waiters,
                    &tid,
                    &mut client,
                    &control.cancel,
                    true,
                    &login_form,
                    push,
                )
                .await
                {
                    if control.cancel.load(Ordering::SeqCst) {
                        run_user_log::log_user(control, state, app, "已停止：中止后续步骤");
                        break;
                    }
                    let err = if e.contains("已取消") {
                        e
                    } else {
                        format!("验证码未通过，登录中断：{e}")
                    };
                    mark_login_interrupted(
                        control,
                        state,
                        app,
                        i,
                        idx,
                        total,
                        &step_desc,
                        &err,
                        &mut ok_count,
                        &mut fail_count,
                        step_ok,
                    );
                    maybe_analyze_step_failure(
                        app,
                        state,
                        control,
                        &mut client,
                        &session,
                        i,
                        idx as u32,
                        &step_desc,
                        &err,
                        false,
                        None,
                        None,
                        None,
                        false,
                    )
                    .await;
                    break;
                }
            }
        }

        // 「登录」/「点击登录」：提交后校验是否真的进入系统；失败则中断后续步骤
        // 已处于登录态并直达业务页时跳过校验，避免误报阻断
        if login_scenario && step_ok && !control.cancel.load(Ordering::SeqCst) {
            if login_already_in {
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    "已处于登录态，跳过登录校验，继续后续步骤".to_string(),
                );
            } else {
            match login_verify::await_login_outcome(&mut client, Some(&control.cancel)).await {
                Ok(note) => {
                    run_user_log::log_user(control, state, app, note.clone());
                    if let Some(rec) = control.step_records.lock().get_mut(i) {
                        if rec.status == "ok" && !note.is_empty() {
                            let base = user_step_summary(&rec.detail);
                            rec.detail = if base.is_empty() {
                                note
                            } else {
                                format!("{base} · {note}")
                            };
                        }
                    }
                }
                Err(e) => {
                    if e.contains("已取消") || control.cancel.load(Ordering::SeqCst) {
                        run_user_log::log_user(control, state, app, "已停止：中止后续步骤");
                        break;
                    }
                    mark_login_interrupted(
                        control,
                        state,
                        app,
                        i,
                        idx,
                        total,
                        &step_desc,
                        &e,
                        &mut ok_count,
                        &mut fail_count,
                        true,
                    );
                    maybe_analyze_step_failure(
                        app,
                        state,
                        control,
                        &mut client,
                        &session,
                        i,
                        idx as u32,
                        &step_desc,
                        &e,
                        false,
                        None,
                        None,
                        None,
                        false,
                    )
                    .await;
                    break;
                }
            }
            }
        }

        i += 1;
    }

    // 先停后台泵，再恢复本任务端口的下载策略 / 文件框屏蔽（须在关闭任务浏览器之前）
    if let Some(memory_heartbeat) = memory_heartbeat {
        memory_heartbeat.stop();
    }
    auto_download.stop();
    crate::client_download::clear_active_download_trace();

    let cancelled = control.cancel.load(Ordering::SeqCst);
    let browser_alive = chrome::is_debug_port_ready_on(debug_port).await;
    // 正常结束：CDP 收尾。用户停止：保留浏览器，跳过可能卡住的收尾。
    // 浏览器已死（OOM 等）：清理任务侧句柄，不在此处再杀（进程已不在）。
    if !cancelled && browser_alive {
        let preferred = state
            .last_main_target
            .lock()
            .clone()
            .or_else(|| Some(client.target_id.clone()));
        if shield_file_chooser {
            let _ = client.set_intercept_file_chooser(false).await;
            run_user_log::log_full(control, "已取消屏蔽系统文件选择框");
        }
        crate::client_download::exit_automation_download_mode(
            debug_port,
            preferred.as_deref().or(Some(client.target_id.as_str())),
        )
        .await;

        log_browser_memory(
            control,
            debug_port,
            Some(&client),
            "任务结束前",
            &mut last_chrome_ws,
        )
        .await;

        if let Some(c) = control.task_chrome.lock().take() {
            if chrome_headless {
                c.shutdown();
                let line = format!("任务专用浏览器已关闭 | 端口 {}", c.port);
                run_user_log::log_full(control, line);
            } else {
                // 设置关闭「后台隐藏浏览器」：任务完成后保留可见窗口，便于核对页面
                let line = format!(
                    "任务浏览器保持打开（未开启「后台隐藏浏览器」）| 端口 {}",
                    c.port
                );
                run_user_log::log_full(control, line);
            }
        } else if worker_persist {
            chrome::save_worker_last_page(task_id,&client.target_url, &client.target_title);
            run_user_log::log_user(
                control,
                state,
                app,
                "伙伴浏览器保持运行，供继续任务衔接",
            );
        }

        browser_session::release_active_browser(
            app,
            &state.active_browser,
            preferred.as_deref(),
        )
        .await;
    } else if cancelled {
        run_user_log::log_full(
            control,
            "已跳过完整浏览器收尾（任务已停止，浏览器保持运行）",
        );
        let preferred = state
            .last_main_target
            .lock()
            .clone()
            .or_else(|| Some(client.target_id.clone()));
        // 恢复本地下载（走 Browser WS / 新 Page 会话，不沿用已取消的 CDP）
        crate::client_download::exit_automation_download_mode(
            debug_port,
            preferred.as_deref(),
        )
        .await;
        if shield_file_chooser {
            if let Ok(c) = crate::client_download::connect_page_for_cleanup(
                debug_port,
                preferred.as_deref(),
            )
            .await
            {
                let _ = c.set_intercept_file_chooser(false).await;
                run_user_log::log_full(control, "已取消屏蔽系统文件选择框");
            }
        }
        // 任务专用 Chrome：任务结束仍关闭，避免并行任务泄漏；共用 9222 / 伙伴续跑浏览器不杀
        if let Some(c) = control.task_chrome.lock().take() {
            c.shutdown();
            let line = format!("任务专用浏览器已关闭 | 端口 {}", c.port);
            run_user_log::log_full(control, line);
        } else if worker_persist {
            chrome::save_worker_last_page(task_id,&client.target_url, &client.target_title);
            run_user_log::log_full(control, "伙伴浏览器保持运行，供继续任务衔接");
        }
        browser_session::release_active_browser(
            app,
            &state.active_browser,
            preferred.as_deref(),
        )
        .await;
    } else {
        run_user_log::log_full(control, "已跳过浏览器收尾（调试端口不可达）");
        if let Some(c) = control.task_chrome.lock().take() {
            c.force_kill();
        }
        let preferred = state.last_main_target.lock().clone();
        browser_session::release_active_browser(
            app,
            &state.active_browser,
            preferred.as_deref(),
        )
        .await;
    }

    let total_ms = t0.elapsed().as_millis();
    if let Some(summary) = session.assert_report_user_summary() {
        run_user_log::log_user(control, state, app, summary);
    }
    let end = format!(
        "════ 快捷指令执行结束 ════ 成功 {ok_count} 步，失败 {fail_count} 步 | 总耗时 {total_ms} ms | 产物: {}",
        session.run_dir_display()
    );
    run_user_log::log_dual(
        control,
        state,
        app,
        end,
        format_user_footer(ok_count, fail_count, total_ms),
    );
    flush_run_logs_to_disk(app, control, &session);
    // 允许再次打开探针（开跑期间抑制的 overlay 绘制）
    crate::page_probe::suppress_probe_overlays(false);

    if crate::run_sink::worker_mode() {
        let err = if fail_count > 0 {
            Some(format!("失败 {fail_count} 步"))
        } else if cancelled {
            Some("已取消".into())
        } else {
            None
        };
        crate::run_sink::emit_event(crate::run_sink::RunEvent::Done {
            ok: ok_count,
            fail: fail_count,
            error: err,
            run_dir: Some(session.run_dir.display().to_string()),
        });
    }

    if cancelled {
        Ok(format!("stopped:ok={ok_count},fail={fail_count}"))
    } else {
        Ok(format!("done:ok={ok_count},fail={fail_count}"))
    }
}

/// 将完整执行日志落盘到内部目录：`executions/run-log.txt`（不对用户展示）
fn flush_run_logs_to_disk<R: Runtime>(
    app: &AppHandle<R>,
    control: &TaskControl,
    session: &std::sync::Arc<RunSession>,
) {
    let logs: Vec<String> = control.logs.lock().clone();
    if logs.is_empty() {
        return;
    }
    match session.write_run_summary_log(&logs) {
        Ok(()) => {
            run_artifacts::emit_artifacts_update(app, session);
        }
        Err(e) => {
            run_user_log::log_full(control, format!("写入执行日志失败: {e}"));
        }
    }
}

/// 采样 Chrome 进程树 + 当前页 JS/DOM，仅写入完整执行日志（不刷用户对话区）。
async fn log_browser_memory(
    control: &TaskControl,
    debug_port: u16,
    client: Option<&CdpClient>,
    phase: &str,
    prev_ws: &mut Option<u64>,
) {
    let snap = browser_memory::sample(debug_port, client).await;
    let line = snap.format_line(phase, *prev_ws);
    if let Some(ws) = snap.working_set_bytes() {
        *prev_ws = Some(ws);
    }
    run_user_log::log_full(control, line);
}

/// OOM/卡死后强制重启浏览器并重新附着 CDP。
async fn restart_browser_after_crash<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    control: &TaskControl,
    client: &mut CdpClient,
    debug_port: u16,
    use_shared_debug: bool,
    chrome_headless: bool,
    task_id: &str,
) -> Result<(), String> {
    if use_shared_debug {
        *state.last_main_target.lock() = None;
        *state.active_browser.lock() = None;
        let msg =
            chrome::force_restart_debug_chrome(&state.chrome_child, false).await?;
        run_user_log::log_full(control, msg);
    } else if crate::run_sink::worker_mode() {
        let (_port, msg) = chrome::restart_worker_chrome(chrome_headless, task_id).await?;
        run_user_log::log_full(control, msg);
    } else {
        let chrome = control.task_chrome.lock().clone();
        if let Some(chrome) = chrome {
            chrome
                .restart_after_crash(chrome_headless)
                .await
                .map_err(|e| format!("任务浏览器重启失败: {e}"))?;
            run_user_log::log_full(
                control,
                format!("任务专用浏览器已重启 | 端口 {}", chrome.port),
            );
        } else {
            chrome::kill_listeners_on_port(debug_port);
            return Err(format!("调试端口 {debug_port} 无浏览器实例可重启"));
        }
    }

    control.set_debug_port(debug_port);
    let mut last_err = String::from("未知错误");
    for attempts in 1..=6u32 {
        match CdpClient::connect_on(debug_port).await {
            Ok(c) => {
                *client = c;
                last_err.clear();
                break;
            }
            Err(e) => {
                last_err = e;
                if attempts == 6 {
                    return Err(format!("重启后无法连接 CDP: {last_err}"));
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    }
    browser_session::set_active_browser(app, &state.active_browser, task_id, client);
    if use_shared_debug {
        remember_shared_main_target(state, client);
    }
    if crate::run_sink::worker_mode() {
        if let Some(url) = chrome::load_worker_last_page(task_id) {
            if url != client.target_url {
                let _ = client.navigate(&url).await;
                let _ = client.wait_after_navigation(20).await;
                let _ = client.sync_page_meta().await;
            }
        }
        chrome::save_worker_last_page(task_id,&client.target_url, &client.target_title);
    }
    run_user_log::log_user(
        control,
        state,
        app,
        format!(
            "浏览器已重启并重新连接 | 「{}」| {}（登录态可能已丢失）",
            client.target_title, client.target_url
        ),
    );
    Ok(())
}

/// 登录场景校验失败：把已记为成功的步骤改记失败，并写用户可读日志（随后应 break）
fn mark_login_interrupted<R: Runtime>(
    control: &TaskControl,
    state: &crate::AppState,
    app: &AppHandle<R>,
    step_index: usize,
    idx: usize,
    total: u32,
    step_desc: &str,
    err: &str,
    ok_count: &mut u32,
    fail_count: &mut u32,
    was_marked_ok: bool,
) {
    if was_marked_ok {
        if *ok_count > 0 {
            *ok_count -= 1;
        }
        *fail_count += 1;
    }

    if let Some(rec) = control.step_records.lock().get_mut(step_index) {
        rec.status = "fail".into();
        rec.detail = err.to_string();
    }

    let full_line = format!("步骤 {idx} 登录校验失败 | 错误: {err}");
    let user_line = format_user_step_fail(idx as u32, total, step_desc, err);
    run_user_log::log_dual(control, state, app, full_line, user_line);
    run_user_log::log_user(
        control,
        state,
        app,
        "登录未通过，已中断后续步骤。请到「设置 → 网站账号」核对系统地址与账号密码后重试；也可手工登录后点击「继续当前步骤」或「继续下一步骤」。",
    );
}

/// 扫描某一阶段探针并写入 run-log（完整日志，不推对话区）
async fn capture_probe_phase_to_log(
    control: &TaskControl,
    client: &CdpClient,
    phase: &str,
) -> Option<(String, crate::page_probe::ProbePageResult)> {
    if control.cancel.load(Ordering::SeqCst) {
        return None;
    }
    if !chrome::is_debug_port_ready_on(client.port).await {
        run_user_log::log_full(
            control,
            format!("探针快照 | {phase} | 跳过：浏览器已无响应"),
        );
        return None;
    }
    let scanned = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        crate::page_probe::capture_probe_phase_dump(client, phase),
    )
    .await;
    crate::page_probe::clear_probe_residue(client).await;
    match scanned {
        Ok(Ok((dump, page))) => {
            for line in dump.lines() {
                if !line.is_empty() {
                    run_user_log::log_full(control, line);
                }
            }
            Some((dump, page))
        }
        Ok(Err(e)) => {
            run_user_log::log_full(control, format!("探针快照 | {phase} | 扫描失败: {e}"));
            None
        }
        Err(_) => {
            run_user_log::log_full(control, format!("探针快照 | {phase} | 扫描超时"));
            None
        }
    }
}

/// 步骤失败后：截图落盘 +（若开启 AI 增强）调用方舟分析原因
async fn maybe_analyze_step_failure<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    control: &TaskControl,
    client: &mut CdpClient,
    session: &RunSession,
    step_index: usize,
    idx: u32,
    step_desc: &str,
    err: &str,
    recovery_attempted: bool,
    probe_dump_at_fail: Option<&str>,
    probe_dump_after_recovery: Option<&str>,
    reuse_probe: Option<&crate::page_probe::ProbePageResult>,
    // 断言失败等场景：仍可落盘截图，但不做 AI 失败分析
    skip_ai_analysis: bool,
) {
    if control.cancel.load(Ordering::SeqCst) {
        return;
    }

    let browser_dead = ai_repair::needs_browser_restart(err)
        || !chrome::is_debug_port_ready_on(client.port).await;

    // 失败截图尽量落盘，供人工查看（与 AI 开关无关）
    if !browser_dead && client.ensure_attached().await.is_ok() {
        match client.capture_screenshot_png().await {
            Ok(png) => {
                let label = format!("failure-step-{idx}");
                match session.save_step_screenshot(&png, idx, &label) {
                    Ok(_) => run_artifacts::emit_artifacts_update(app, session),
                    Err(e) => run_user_log::log_full(
                        control,
                        format!("失败截图保存失败: {e}"),
                    ),
                }
            }
            Err(e) => {
                run_user_log::log_full(control, format!("失败截图跳过: {e}"));
            }
        }
    } else if browser_dead {
        run_user_log::log_full(control, "失败截图跳过：浏览器已无响应");
    }

    if skip_ai_analysis || !settings::ai_enhance_enabled() {
        return;
    }

    // 若循环内未采到快照（如登录即时中断），此处补扫「最终中断时」
    let mut final_dump: Option<String> = None;
    let mut final_page: Option<crate::page_probe::ProbePageResult> = None;
    let need_final_scan = !browser_dead
        && !control.cancel.load(Ordering::SeqCst)
        && probe_dump_at_fail.is_none()
        && probe_dump_after_recovery.is_none();
    if need_final_scan {
        if let Some((dump, page)) =
            capture_probe_phase_to_log(control, client, "最终中断时").await
        {
            final_dump = Some(dump);
            final_page = Some(page);
        }
    }

    let dump_at_fail = probe_dump_at_fail
        .map(str::to_string)
        .or_else(|| final_dump.clone());
    let dump_after = probe_dump_after_recovery.map(str::to_string);

    if control.cancel.load(Ordering::SeqCst) {
        return;
    }

    let browser = if browser_dead {
        None
    } else {
        let _ = client.sync_page_meta().await;
        let snap = browser_context::snapshot_from_client(client);
        browser_context::store_snapshot(&state.last_browser_snapshot, snap.clone());
        Some(snap)
    };

    let recent_logs: Vec<String> = {
        let logs = control.logs.lock();
        let n = logs.len();
        let start = n.saturating_sub(30);
        logs[start..].to_vec()
    };

    let probe_for_summary = reuse_probe.or(final_page.as_ref());
    let probe_summary = probe_for_summary
        .map(|p| ai_repair::format_probe_summary(&p.items, 40))
        .filter(|s| !s.is_empty());

    let analysis_error = if browser_dead {
        format!(
            "{err}\n（诊断提示：浏览器进程已无响应/调试端口不可达，高度疑似 Chrome OOM 或崩溃；页面截图与探针不可用，请结合执行日志中的内存采样分析。）"
        )
    } else {
        err.to_string()
    };

    let ctx = FailureAnalysisContext {
        step_label: step_desc.to_string(),
        error: analysis_error,
        recovery_attempted,
        browser,
        recent_logs,
        probe_summary,
        probe_dump_at_fail: dump_at_fail,
        probe_dump_after_recovery: dump_after,
    };

    run_user_log::log_user(control, state, app, "正在用 AI 分析失败原因…");

    let stream_id = format!(
        "failure-s{idx}-{}",
        chrono::Local::now().format("%H%M%S%3f")
    );
    // 通知前端挂接思考流式气泡（与 ai-stream-chunk / ai-stream-done 配套）
    let _ = app.emit(
        "ai-failure-analysis-start",
        serde_json::json!({
            "streamId": stream_id,
            "stepIndex": idx,
            "stepDesc": step_desc,
        }),
    );
    // 给前端一点时间挂接流式气泡，避免漏掉首段 reasoning
    tokio::time::sleep(Duration::from_millis(200)).await;

    let cancel = control.cancel.clone();
    let result = match tokio::time::timeout(
        Duration::from_secs(90),
        ark::analyze_step_failure_stream(app, &stream_id, &ctx, cancel),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            let _ = app.emit(
                "ai-stream-done",
                ark::AiStreamDonePayload {
                    stream_id: stream_id.clone(),
                    content: String::new(),
                    reasoning: None,
                    aborted: control.cancel.load(Ordering::SeqCst),
                    error: Some(e.clone()),
                    usage: None,
                },
            );
            if e.contains("已取消") || control.cancel.load(Ordering::SeqCst) {
                return;
            }
            run_user_log::log_user(
                control,
                state,
                app,
                format!("AI 失败分析跳过：{e}"),
            );
            return;
        }
        Err(_) => {
            let _ = app.emit(
                "ai-stream-done",
                ark::AiStreamDonePayload {
                    stream_id: stream_id.clone(),
                    content: String::new(),
                    reasoning: None,
                    aborted: false,
                    error: Some("AI 失败分析超时".into()),
                    usage: None,
                },
            );
            run_user_log::log_user(control, state, app, "AI 失败分析超时，已跳过");
            return;
        }
    };

    if control.cancel.load(Ordering::SeqCst) {
        return;
    }

    let analysis = result.content;
    let usage_line = result.usage.as_ref().map(ark::format_token_usage);
    let user_line = format_user_failure_analysis(&analysis, usage_line.as_deref());
    let full_usage = usage_line
        .as_deref()
        .map(|u| format!(" | {u}"))
        .unwrap_or_default();
    run_user_log::log_dual(
        control,
        state,
        app,
        format!("步骤 {idx} AI 失败分析{full_usage} | {analysis}"),
        user_line,
    );

    let usage_md = usage_line
        .as_deref()
        .map(|u| format!("\n\n**{u}**\n"))
        .unwrap_or_default();
    let md = format!(
        "# 步骤 {idx} 失败分析\n\n**步骤:** {step_desc}\n\n**错误:** {err}\n\n{analysis}{usage_md}"
    );
    let fname = format!("failure-analysis-step-{idx}.md");
    match session.save_text_document(&fname, &md, Some(idx)) {
        Ok(_) => run_artifacts::emit_artifacts_update(app, session),
        Err(e) => run_user_log::log_full(control, format!("写入失败分析文档失败: {e}")),
    }

    let hint = failure_analysis_detail_hint(&analysis);
    if !hint.is_empty() {
        if let Some(rec) = control.step_records.lock().get_mut(step_index) {
            if rec.detail.is_empty() {
                rec.detail = format!("AI: {hint}");
            } else {
                rec.detail = format!("{} | AI: {hint}", rec.detail);
            }
        }
    }
}

pub fn parse_exec_result(result: &str) -> (bool, String) {
    if result.starts_with("failed:") {
        return (false, "浏览器或环境未就绪，任务未能执行".into());
    }
    if result.starts_with("stopped:") {
        return (false, format!("任务已中止（{result}）"));
    }
    if let Some(rest) = result.strip_prefix("done:") {
        let fail = rest
            .split(',')
            .find_map(|p| p.strip_prefix("fail="))
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0);
        let ok = rest
            .split(',')
            .find_map(|p| p.strip_prefix("ok="))
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0);
        let report = rest
            .split(',')
            .find_map(|p| p.strip_prefix("report="))
            .unwrap_or("skip");
        let mut summary = format!("成功 {ok} 步，失败 {fail} 步");
        if report == "fail" {
            summary.push_str("；汇报生成失败");
        } else if report == "ok" {
            summary.push_str("；已生成汇报");
        }
        return (fail == 0, summary);
    }
    (true, result.to_string())
}

pub fn format_task_logs_for_history(
    control: &TaskControl,
    task_name: &str,
    path: Option<&str>,
) -> String {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut out = format!("════ 后台任务 · 「{task_name}」 ════\n时间: {ts}\n");
    if let Some(p) = path {
        out.push_str(&format!("文件: {p}\n"));
    }
    if let Some(session) = control.run_session.lock().as_ref() {
        out.push_str(&format!("产物目录: {}\n", session.run_dir_display()));
        let arts = session.list_artifacts();
        if !arts.is_empty() {
            out.push_str("产物:\n");
            for art in arts {
                out.push_str(&format!("  · [{}] {}\n", art.kind, art.name));
            }
        }
    }

    // 只保留一套步骤结果（来自 step_records），避免与执行日志重复
    let steps = control.step_records.lock().clone();
    let mut ok = 0u32;
    let mut fail = 0u32;
    if !steps.is_empty() {
        out.push_str("\n── 步骤 ──\n");
        for s in &steps {
            let status = match s.status.as_str() {
                "ok" => {
                    ok += 1;
                    "成功"
                }
                "fail" => {
                    fail += 1;
                    "失败"
                }
                "skipped" => "跳过",
                "running" => "进行中",
                other => other,
            };
            let label = s.label.lines().next().unwrap_or("").trim();
            let mut line = format!("步骤 {}/{} {} · {}", s.index, steps.len(), status, label);
            let detail = s.detail.trim();
            if !detail.is_empty() {
                let short = detail.lines().next().unwrap_or(detail).trim();
                if !short.is_empty() && short != label {
                    line.push_str(" · ");
                    line.push_str(short);
                }
            }
            out.push_str(&line);
            out.push('\n');
        }
    }

    let logs = control.logs.lock().clone();
    let joined = logs.join("\n");

    // AI 分析正文（与主界面一致，单独抽出）
    if let Some(ai) = extract_ai_analyze_from_logs(&joined) {
        out.push_str("\n⟦AI分析⟧\n");
        out.push_str(&ai);
        out.push_str("\n⟦/AI分析⟧\n");
    }

    // 人机/扫码等补充提示（不重复步骤行）
    let mut notes = Vec::new();
    let mut seen_notes = std::collections::HashSet::new();
    for raw in &logs {
        for t in raw.lines().map(str::trim).filter(|s| !s.is_empty()) {
            if !is_history_side_note(t) {
                continue;
            }
            if seen_notes.insert(t.to_string()) {
                notes.push(t.to_string());
            }
        }
    }
    if !notes.is_empty() {
        out.push_str("\n── 提示 ──\n");
        for n in notes {
            out.push_str(&n);
            out.push('\n');
        }
    }

    if let Some(footer) = logs.iter().rev().find_map(|l| {
        l.lines()
            .map(str::trim)
            .find(|t| t.starts_with("════ 完成") || t.starts_with("════ 结束"))
            .map(|s| s.to_string())
    }) {
        out.push('\n');
        out.push_str(&footer);
        out.push('\n');
    } else if !steps.is_empty() {
        out.push('\n');
        if fail > 0 {
            out.push_str(&format!("════ 结束 · 成功 {ok} 步，失败 {fail} 步 ════\n"));
        } else {
            out.push_str(&format!("════ 完成 · 成功 {ok} 步 ════\n"));
        }
    }
    out
}

fn extract_ai_analyze_from_logs(text: &str) -> Option<String> {
    const START: &str = "⟦AI分析⟧";
    const END: &str = "⟦/AI分析⟧";
    let i = text.find(START)?;
    let rest = &text[i + START.len()..];
    let body = match rest.find(END) {
        Some(j) => &rest[..j],
        None => rest,
    };
    let body = body.trim();
    if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

/// 步骤结果之外的少量用户提示（登录/人机等），不含步骤行与技术日志
fn is_history_side_note(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty()
        || t.starts_with('═')
        || t.starts_with("──")
        || t.starts_with("步骤 ")
        || t.starts_with("正在执行")
        || t.starts_with("⟦")
        || t.contains("浏览器内存")
        || t.contains("下载截获")
        || t.contains("下载捕获")
        || t.contains("内存定时采样")
        || t.contains("产物已与对话绑定")
        || t.contains("端口 ")
    {
        return false;
    }
    t.starts_with("飞书")
        || t.starts_with("人机验证")
        || t.starts_with("检测到")
        || t.starts_with("验证码")
        || t.starts_with("滑块")
        || t.starts_with("已停止")
        || t.starts_with("已写入参数")
        || t.starts_with("AI 失败分析")
        || t.starts_with("AI 正在修复")
        || t.starts_with("AI 已")
        || t.starts_with("AI 无法")
}

fn mask_input_args_for_log(args: &str) -> String {
    let (text, field) = commands::scripts::parse_input_step_args(args);
    let masked = if text.chars().count() <= 2 {
        "*".repeat(text.chars().count())
    } else {
        format!("{}***", text.chars().next().unwrap_or('*'))
    };
    if field.is_empty() {
        masked
    } else if text.is_empty() {
        format!("{field}=")
    } else {
        format!("{field}={masked}")
    }
}

fn describe_step(step: &Step) -> String {
    let base = if step.keyword == "输入" {
        format!("{} {}", step.keyword, mask_input_args_for_log(&step.args))
    } else {
        format!("{} {}", step.keyword, step.args)
    };
    match step.or_group.as_ref().map(|g| g.branch) {
        Some(OrBranch::Try) => format!("试探·{base}"),
        Some(OrBranch::Fallback) => format!("备选·{base}"),
        None => base,
    }
}

fn describe_step_resolved(step: &Step, params: &HashMap<String, String>) -> String {
    let args = crate::recipe::resolve_step_args(&step.args, params);
    describe_step(&Step {
        keyword: step.keyword.clone(),
        args,
        or_group: step.or_group.clone(),
    })
}

/// 仅「脚本 / 参数 / 等待」等无浏览器步骤：不启动、不连接 Chrome。
async fn run_browser_free_steps<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    task_id: &str,
    control: &TaskControl,
    title: &str,
    steps: &mut Vec<Step>,
    start_i: usize,
    session: &Arc<RunSession>,
) -> Result<String, String> {
    let t0 = std::time::Instant::now();
    let mut ok_count = 0u32;
    let mut fail_count = 0u32;
    let mut focus_ctx = commands::FocusContext::default();
    let mut i = start_i;

    while i < steps.len() {
        if control.cancel.load(Ordering::SeqCst) {
            run_user_log::log_user(control, state, app, "已停止：中止后续步骤");
            break;
        }

        *control.next_index.lock() = i;
        wait_if_paused(
            app,
            task_id,
            control,
            title,
            steps,
            &mut i,
            "已暂停：可插入指令，或点击步骤回退/跳过",
            0,
            "",
            None,
            false,
        )
        .await;

        if control.cancel.load(Ordering::SeqCst) {
            break;
        }
        if i >= steps.len() {
            break;
        }

        if control.skip_set.lock().remove(&i) {
            let idx = i + 1;
            let total_now = steps.len() as u32;
            let params_now = control.recipe_params.lock().clone();
            let step_desc = describe_step_resolved(&steps[i], &params_now);
            if let Some(rec) = control.step_records.lock().get_mut(i) {
                rec.status = "skipped".into();
                rec.detail = "已跳过".into();
                rec.label = step_desc.clone();
            }
            run_user_log::log_user(
                control,
                state,
                app,
                format!("── 步骤 {idx}/{total_now} ── 已跳过：{step_desc}"),
            );
            emit_task_progress(
                app,
                task_id,
                control,
                title,
                "step",
                idx as u32,
                total_now,
                &format!("已跳过：{step_desc}"),
                false,
                false,
            );
            i += 1;
            continue;
        }

        let step_tmpl = steps[i].clone();
        let params_now = control.recipe_params.lock().clone();
        let clipboard = commands::read_system_clipboard();
        let runtime = crate::recipe::RuntimeParamCtx {
            tab_url: "",
            tab_title: "",
            clipboard: &clipboard,
        };
        let resolved_args =
            crate::recipe::resolve_step_args_ex(&step_tmpl.args, &params_now, Some(&runtime));
        let step = Step {
            keyword: step_tmpl.keyword.clone(),
            args: resolved_args,
            or_group: step_tmpl.or_group.clone(),
        };
        let total = steps.len() as u32;
        let idx = (i + 1) as u32;
        let step_desc = describe_step(&step);

        emit_task_progress(
            app,
            task_id,
            control,
            title,
            "step",
            idx,
            total,
            &step_desc,
            false,
            false,
        );
        run_user_log::log_user(
            control,
            state,
            app,
            format!("── 步骤 {idx}/{total} ── {step_desc}"),
        );

        let result = execute_step(
            app,
            state,
            task_id,
            control,
            None,
            &step,
            Some(session),
            Some(idx),
            Some(&control.cancel),
            &mut focus_ctx,
        )
        .await;

        match result {
            Ok(msg) => {
                ok_count += 1;
                let detail = user_step_summary(&msg);
                if let Some(rec) = control.step_records.lock().get_mut(i) {
                    rec.status = "ok".into();
                    rec.detail = detail.clone();
                    rec.label = step_desc.clone();
                }
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    format_user_step_ok(idx, total, &step_desc, &detail, false, false),
                );
            }
            Err(e) => {
                fail_count += 1;
                if let Some(rec) = control.step_records.lock().get_mut(i) {
                    rec.status = "fail".into();
                    rec.detail = e.clone();
                    rec.label = step_desc.clone();
                }
                run_user_log::log_user(
                    control,
                    state,
                    app,
                    format_user_step_fail(idx, total, &step_desc, &e),
                );
                break;
            }
        }
        i += 1;
    }

    let cancelled = control.cancel.load(Ordering::SeqCst);
    let total_ms = t0.elapsed().as_millis();
    if let Some(summary) = session.assert_report_user_summary() {
        run_user_log::log_user(control, state, app, summary);
    }
    let end = format!(
        "════ 快捷指令执行结束 ════ 成功 {ok_count} 步，失败 {fail_count} 步 | 总耗时 {total_ms} ms | 产物: {}",
        session.run_dir_display()
    );
    run_user_log::log_dual(
        control,
        state,
        app,
        end,
        format_user_footer(ok_count, fail_count, total_ms),
    );
    flush_run_logs_to_disk(app, control, session);

    if crate::run_sink::worker_mode() {
        let err = if fail_count > 0 {
            Some(format!("失败 {fail_count} 步"))
        } else if cancelled {
            Some("已取消".into())
        } else {
            None
        };
        crate::run_sink::emit_event(crate::run_sink::RunEvent::Done {
            ok: ok_count,
            fail: fail_count,
            error: err,
            run_dir: Some(session.run_dir.display().to_string()),
        });
    }

    emit_task_progress(
        app,
        task_id,
        control,
        title,
        if cancelled {
            "stopped"
        } else if fail_count > 0 {
            "error"
        } else {
            "done"
        },
        ok_count + fail_count,
        steps.len() as u32,
        &format_user_footer(ok_count, fail_count, total_ms),
        true,
        false,
    );

    if cancelled {
        Ok(format!("stopped:ok={ok_count},fail={fail_count}"))
    } else {
        Ok(format!("done:ok={ok_count},fail={fail_count}"))
    }
}

async fn execute_step<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    task_id: &str,
    control: &TaskControl,
    client: Option<&mut CdpClient>,
    step: &Step,
    session: Option<&Arc<RunSession>>,
    step_index: Option<u32>,
    cancel: Option<&Arc<std::sync::atomic::AtomicBool>>,
    focus_ctx: &mut commands::FocusContext,
) -> Result<String, String> {
    // 「参数」/「动态参数」：写入/覆盖任务参数表（args 已在循环中展开 {{页签地址}} 等）
    if step.keyword == "参数" || step.keyword == "动态参数" {
        let (name, value) = crate::recipe::parse_param_assignment(&step.args)?;
        control.recipe_params.lock().insert(name.clone(), value.clone());
        if step.keyword == "动态参数" {
            control.runtime_only_params.lock().insert(name.clone());
        } else {
            control.runtime_only_params.lock().remove(&name);
        }
        control.refresh_pending_step_labels();
        let preview = if value.chars().count() > 80 {
            format!("{}…", value.chars().take(80).collect::<String>())
        } else {
            value
        };
        let kind = if step.keyword == "动态参数" {
            "动态参数"
        } else {
            "参数"
        };
        return Ok(format!("已设置{kind} {name}={preview}"));
    }

    let cmd = commands::get_by_keyword(&step.keyword)?
        .ok_or_else(|| format!("未注册指令「{}」", step.keyword))?;
    // 仅主窗口对话区实时推送 Rhai print；worker / 远程经 run_sink 下发
    let on_print = if task_id == "main" {
        let app = app.clone();
        let control = control.clone();
        Some(std::sync::Arc::new(move |line: &str| {
            if let Some(st) = app.try_state::<crate::AppState>() {
                run_user_log::log_user(&control, st.inner(), &app, line);
            } else {
                control.push_log(line);
            }
        }) as crate::rhai_runtime::RhaiPrintSink)
    } else if crate::run_sink::worker_mode() {
        let control = control.clone();
        Some(std::sync::Arc::new(move |line: &str| {
            let t = line.trim();
            if !t.is_empty() {
                control.push_log(t);
                crate::run_sink::emit_log(t);
            }
        }) as crate::rhai_runtime::RhaiPrintSink)
    } else {
        None
    };
    run_command(
        client,
        &cmd,
        &step.args,
        session,
        step_index,
        Some(commands::CommandContext {
            cancel: cancel.cloned(),
            app: Some(app),
            task_id: Some(task_id),
            focus: focus_ctx,
            or_probe: step.is_or_try(),
            on_print,
        }),
    )
    .await
}

pub async fn execute_recipe_content<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    recipe_name: Option<&str>,
    content: &str,
    chat_session_id: Option<&str>,
) -> Result<String, String> {
    execute_recipe_content_from(app, state, recipe_name, content, chat_session_id, None).await
}

pub async fn execute_recipe_content_from<R: Runtime>(
    app: &AppHandle<R>,
    state: &crate::AppState,
    recipe_name: Option<&str>,
    content: &str,
    chat_session_id: Option<&str>,
    start_step_index: Option<u32>,
) -> Result<String, String> {
    // AI 增强等前置阶段若已点停止，勿清空 cancel 后继续跑步骤
    if state.cancel_flag.load(Ordering::SeqCst) {
        return Ok("stopped:已取消".into());
    }
    state.cancel_flag.store(false, Ordering::SeqCst);
    crate::cdp::clear_user_cancelled_on_live();
    let control = TaskControl {
        cancel: state.cancel_flag.clone(),
        ..TaskControl::new()
    };
    // 供主窗口「暂停/继续」通过 task_pause_cmd / task_resume_cmd 操作
    state.task_registry.insert("main", control.clone());
    let result = execute_recipe_for_task_from(
        app,
        state,
        "main",
        &control,
        recipe_name,
        content,
        chat_session_id,
        start_step_index,
    )
    .await;
    state.task_registry.remove("main");
    #[cfg(feature = "desktop")]
    crate::scheduler::notify_execution_idle(app);
    result
}

#[cfg(test)]
mod tests {
    use super::parse_exec_result;

    #[test]
    fn parse_done_all_ok() {
        let (ok, s) = parse_exec_result("done:ok=3,fail=0");
        assert!(ok);
        assert!(s.contains("成功 3"));
    }

    #[test]
    fn parse_done_with_fail() {
        let (ok, _) = parse_exec_result("done:ok=2,fail=1");
        assert!(!ok);
    }
}
