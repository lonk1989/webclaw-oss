use tauri::{Emitter, Manager, State, WindowEvent};
use serde::Serialize;

/// 文档预览窗待打开的 Markdown 路径（避免把 Windows 路径塞进 App URL 查询串）。
static PENDING_MARKDOWN_PATH: Mutex<Option<String>> = Mutex::new(None);

#[derive(Clone, Serialize)]
struct LogPayload {
    lines: Vec<String>,
}

fn emit_logs<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AppState) {
    let lines = state.logs.lock().clone();
    let _ = app.emit("log-update", LogPayload { lines });
}

pub(crate) fn emit_logs_public<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AppState) {
    emit_logs(app, state);
}

async fn ensure_chrome(state: &AppState, app: &tauri::AppHandle) -> Result<(), String> {
    let msg = chrome::ensure_debug_chrome_ready(&state.chrome_child, false).await?;
    state.push_log(&msg);
    emit_logs(app, state);
    Ok(())
}

#[tauri::command]
async fn launch_debug_chrome(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if chrome::is_debug_port_ready().await {
        let pages = chrome::list_page_targets().await.unwrap_or_default();
        state.push_log(format!(
            "调试端口已就绪 | {} 个标签页 | 端口 {}",
            pages.len(),
            chrome::DEBUG_PORT
        ));
        if let Some(p) = chrome::pick_page_target(&pages) {
            state.push_log(format!("当前附着标签: {} | {}", p.title, p.url));
        }
        emit_logs(&app, &state);
        return Ok("already_ready".into());
    }

    state.push_log("正在启动 Debug Chrome…");
    emit_logs(&app, &state);

    let msg = chrome::ensure_debug_chrome_ready(&state.chrome_child, false).await?;
    state.push_log(&msg);
    state.push_log(format!(
        "配置目录 {} | 快捷指令目录 {}",
        chrome::debug_user_data_dir().display(),
        paths::app_data_dir().display()
    ));
    if let Ok(pages) = chrome::list_page_targets().await {
        if let Some(p) = chrome::pick_page_target(&pages) {
            state.push_log(format!("已附着标签: {} | {}", p.title, p.url));
        }
    }
    emit_logs(&app, &state);
    Ok("launched".into())
}

/// 代理 MES API 请求（WebView 直连有 CORS 限制；登录加密体由前端 zhj-login.js 生成）。
#[tauri::command]
async fn zhj_auth_login(
    method: Option<String>,
    url: String,
    headers: HashMap<String, String>,
    body: Option<String>,
) -> Result<serde_json::Value, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("请求地址为空".into());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("非法请求地址".into());
    }

    let method_str = method
        .as_deref()
        .unwrap_or("POST")
        .trim()
        .to_ascii_uppercase();
    let method = match method_str.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        other => return Err(format!("不支持的 HTTP 方法: {other}")),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut req = client.request(method.clone(), url);
    // GET/HEAD 禁止带 body，否则部分 nginx 直接 400
    if !matches!(method, reqwest::Method::GET | reqwest::Method::HEAD) {
        if let Some(body) = body {
            req = req.body(body);
        }
    }

    // 按小写去重，避免同时传 Authorization + authorization 导致 nginx 400
    let mut merged: HashMap<String, String> = HashMap::new();
    for (key, value) in &headers {
        let k = key.trim();
        if k.is_empty() {
            continue;
        }
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let cleaned = value
            .chars()
            .filter(|c| *c != '\r' && *c != '\n')
            .collect::<String>()
            .trim()
            .to_string();
        merged.insert(k.to_ascii_lowercase(), cleaned);
    }
    for (k, v) in merged {
        req = req.header(k, v);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;

    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(mut v) => {
            normalize_zhj_api_json(&mut v);
            Ok(v)
        }
        Err(_) => {
            let preview: String = text.chars().take(200).collect();
            Err(format!(
                "响应异常 HTTP {}：{}",
                status.as_u16(),
                if preview.is_empty() {
                    "(空响应)".into()
                } else {
                    preview
                }
            ))
        }
    }
}

/// 将 API 响应中的 long 型 ID 转为字符串，避免 JS Number 精度丢失。
fn normalize_zhj_api_json(value: &mut serde_json::Value) {
    const ID_KEYS: &[&str] = &[
        "ossId",
        "templateId",
        "scriptId",
        "skillId",
        "createBy",
        "companyId",
        "systemId",
        "buddyId",
    ];
    match value {
        serde_json::Value::Object(map) => {
            for &key in ID_KEYS {
                if let Some(v) = map.get(key) {
                    let s = id_value_to_string(v);
                    map.insert(key.to_string(), s);
                }
            }
            for v in map.values_mut() {
                normalize_zhj_api_json(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                normalize_zhj_api_json(v);
            }
        }
        _ => {}
    }
}

fn id_value_to_string(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                serde_json::Value::String(u.to_string())
            } else if let Some(i) = n.as_i64() {
                serde_json::Value::String(i.to_string())
            } else {
                v.clone()
            }
        }
        serde_json::Value::String(s) => serde_json::Value::String(s.clone()),
        _ => v.clone(),
    }
}

#[tauri::command]
async fn run_visual_query(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    query: String,
) -> Result<String, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err("请输入查询内容".into());
    }

    ensure_chrome(&state, &app).await?;
    state.push_log(format!("── 单步点击 ── 「{query}」"));
    emit_logs(&app, &state);

    let content = format!("点击{query}");
    executor::execute_recipe_content(&app, &state, Some("单步点击"), &content, None).await
}

#[tauri::command]
fn list_recipes_cmd() -> Result<Vec<recipe::RecipeSummary>, String> {
    recipe::list_recipes()
}

#[tauri::command]
fn get_recipe_cmd(id: String) -> Result<Option<recipe::Recipe>, String> {
    recipe::get_recipe(&id)
}

#[tauri::command]
fn save_recipe_cmd(
    id: Option<String>,
    name: String,
    content: String,
) -> Result<recipe::Recipe, String> {
    recipe::save_recipe(id, name, content)
}

#[tauri::command]
fn delete_recipe_cmd(id: String) -> Result<(), String> {
    recipe::delete_recipe(&id)
}

#[tauri::command]
fn list_scheduled_jobs_cmd() -> Result<Vec<scheduler::ScheduledJobSummary>, String> {
    scheduler::list_scheduled_jobs()
}

#[tauri::command]
fn list_expired_scheduled_jobs_cmd() -> Result<Vec<scheduler::ScheduledJobSummary>, String> {
    scheduler::list_expired_jobs()
}

#[tauri::command]
async fn resolve_expired_scheduled_jobs_cmd(
    app: tauri::AppHandle,
    run_ids: Vec<String>,
) -> Result<(), String> {
    scheduler::resolve_expired_jobs_and_enqueue(app, run_ids).await
}

#[tauri::command]
fn get_scheduled_job_cmd(id: String) -> Result<Option<scheduler::ScheduledJob>, String> {
    scheduler::get_scheduled_job(&id)
}

#[tauri::command]
fn save_scheduled_job_cmd(
    app: tauri::AppHandle,
    input: scheduler::SaveScheduledJobInput,
) -> Result<scheduler::ScheduledJob, String> {
    let is_new = input
        .id
        .as_ref()
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);
    let job = scheduler::save_scheduled_job(input)?;
    schedule_floater::notify_jobs_changed(&app);
    // 新建任务时自动打开悬浮窗（建窗已切主线程，失败不拖垮保存）
    if is_new {
        if let Err(e) = schedule_floater::set_enabled(&app, true) {
            eprintln!("[schedule-floater] 新建任务后打开悬浮窗失败: {e}");
        }
    }
    Ok(job)
}

#[tauri::command]
fn delete_scheduled_job_cmd(app: tauri::AppHandle, id: String) -> Result<(), String> {
    scheduler::delete_scheduled_job(&id)?;
    schedule_floater::notify_jobs_changed(&app);
    Ok(())
}

#[tauri::command]
fn toggle_scheduled_job_cmd(
    app: tauri::AppHandle,
    id: String,
    enabled: bool,
) -> Result<scheduler::ScheduledJob, String> {
    let job = scheduler::toggle_scheduled_job(&id, enabled)?;
    schedule_floater::notify_jobs_changed(&app);
    Ok(job)
}

#[tauri::command]
async fn run_scheduled_job_now_cmd(
    app: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    scheduler::enqueue_job_now(app, id).await
}

#[tauri::command]
fn preview_schedule_next_run_cmd(
    schedule: scheduler::ScheduleSpec,
) -> Result<String, String> {
    let next = scheduler::compute_next_run_at(&schedule, chrono::Utc::now())?;
    Ok(next.to_rfc3339())
}

#[tauri::command]
fn set_schedule_floater_enabled_cmd(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    schedule_floater::set_enabled(&app, enabled)
}

#[tauri::command]
fn show_schedule_floater_cmd(app: tauri::AppHandle) -> Result<(), String> {
    schedule_floater::create_or_show(&app)
}

#[tauri::command]
fn hide_schedule_floater_cmd(app: tauri::AppHandle) -> Result<(), String> {
    schedule_floater::hide(&app);
    Ok(())
}

#[tauri::command]
fn set_schedule_floater_collapsed_cmd(
    app: tauri::AppHandle,
    collapsed: bool,
) -> Result<(), String> {
    schedule_floater::set_collapsed(&app, collapsed)
}

#[tauri::command]
fn set_schedule_floater_popup_space_cmd(
    app: tauri::AppHandle,
    mode: String,
) -> Result<(), String> {
    schedule_floater::set_popup_space(&app, mode.trim())
}

#[tauri::command]
fn quit_app_cmd(app: tauri::AppHandle) -> Result<(), String> {
    tray::quit_app(&app);
    Ok(())
}

#[tauri::command]
fn show_main_ui_cmd(app: tauri::AppHandle) -> Result<(), String> {
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        launch::show_main_ui(&app2);
    })
    .map_err(|e| format!("打开主界面失败: {e}"))?;
    Ok(())
}

/// 将云端技能列表写入本地镜像，供「技能名称」解析展开
#[tauri::command]
fn sync_skills_cache_cmd(skills: Vec<recipe::SkillCacheEntry>) -> Result<(), String> {
    recipe::replace_skills_cache(skills)
}

#[tauri::command]
fn set_current_user_cmd(username: Option<String>) -> Result<serde_json::Value, String> {
    let key = paths::set_current_user(username.as_deref());
    Ok(serde_json::json!({
        "userKey": key,
        "appData": paths::app_data_dir().to_string_lossy(),
    }))
}

#[tauri::command]
fn get_current_user_cmd() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "userKey": paths::current_user_key(),
        "appData": paths::app_data_dir().to_string_lossy(),
        "appRoot": paths::app_root_dir().to_string_lossy(),
    }))
}

#[tauri::command]
fn list_web_systems_cmd() -> Result<Vec<web_systems::WebSystemSummary>, String> {
    web_systems::list_web_systems()
}

#[tauri::command]
fn sync_remote_web_systems_cmd(
    systems: Vec<web_systems::RemoteWebSystemInput>,
) -> Result<Vec<web_systems::WebSystemSummary>, String> {
    web_systems::sync_remote_web_systems(systems)
}

#[tauri::command]
fn get_web_system_cmd(id: String) -> Result<Option<web_systems::WebSystem>, String> {
    let mut item = web_systems::get_web_system(&id)?;
    // 密码仅内存/登录使用；前端用 has_password + 掩码展示
    if let Some(ref mut s) = item {
        s.has_password = !s.password.is_empty();
        s.password.clear();
    }
    Ok(item)
}

#[tauri::command]
fn save_web_system_cmd(
    id: Option<String>,
    name: String,
    kind: web_systems::WebSystemKind,
    url: String,
    username: String,
    password: String,
    remark: Option<String>,
) -> Result<web_systems::WebSystem, String> {
    web_systems::save_web_system(
        id,
        name,
        kind,
        url,
        username,
        password,
        remark.unwrap_or_default(),
    )
}

#[tauri::command]
fn local_file_size_cmd(path: String) -> Result<u64, String> {
    let p = std::path::PathBuf::from(path.trim());
    if !p.is_file() {
        return Err(format!("文件不存在: {}", p.display()));
    }
    Ok(std::fs::metadata(&p)
        .map_err(|e| format!("读取文件信息失败: {e}"))?
        .len())
}

#[tauri::command]
fn delete_web_system_cmd(id: String) -> Result<(), String> {
    web_systems::delete_web_system(&id)
}

#[tauri::command]
fn resolve_open_web_target_cmd(query: String) -> Result<Option<String>, String> {
    Ok(web_systems::resolve_open_url_from_query(&query))
}

#[tauri::command]
async fn run_recipe_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    chat_session_id: Option<String>,
) -> Result<String, String> {
    let recipe = recipe::get_recipe(&id)?
        .ok_or_else(|| "快捷指令不存在".to_string())?;
    // 浏览器仅由「打开」「登录」拉起；纯脚本/参数等由执行器按需跳过
    executor::execute_recipe_content(
        &app,
        &state,
        Some(&recipe.name),
        &recipe.content,
        chat_session_id.as_deref(),
    )
    .await
}

#[tauri::command]
async fn run_recipe_content_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: String,
    content: String,
    chat_session_id: Option<String>,
    start_step_index: Option<u32>,
) -> Result<String, String> {
    // 浏览器仅由「打开」「登录」拉起；纯脚本/参数等由执行器按需跳过
    let name = if name.trim().is_empty() {
        "临时快捷指令".to_string()
    } else {
        name
    };
    executor::execute_recipe_content_from(
        &app,
        &state,
        Some(&name),
        &content,
        chat_session_id.as_deref(),
        start_step_index,
    )
    .await
}

/// 运行前：含「上传」且无下载步骤、无附件时，提示用户先添加附件。
#[tauri::command]
fn check_upload_attach_preflight_cmd(
    content: String,
    chat_session_id: Option<String>,
    run_dir: Option<String>,
    start_step_index: Option<u32>,
) -> Result<commands::UploadAttachPreflight, String> {
    commands::check_upload_attach_preflight(
        &content,
        chat_session_id.as_deref(),
        run_dir.as_deref(),
        start_step_index,
    )
}

#[tauri::command]
async fn ai_assist_recipe(
    state: State<'_, AppState>,
    prompt: String,
    existing_content: Option<String>,
    chat_transcript: Option<String>,
) -> Result<ark::RecipeAssistResult, String> {
    if state.cancel_flag.load(Ordering::SeqCst) {
        return Err("已取消".into());
    }
    let preferred = state.last_main_target.lock().clone();
    let browser = browser_context::refresh_browser_snapshot(
        &state.last_browser_snapshot,
        chrome::DEBUG_PORT,
        preferred.as_deref(),
    )
    .await;
    if state.cancel_flag.load(Ordering::SeqCst) {
        return Err("已取消".into());
    }
    let ctx = ark::RecipeAiContext {
        browser,
        chat_transcript,
    };
    ark::generate_recipe_content(
        &prompt,
        existing_content.as_deref(),
        Some(&ctx),
        Some(state.cancel_flag.clone()),
    )
    .await
}

#[tauri::command]
fn clear_execution_cancel_cmd(state: State<'_, AppState>) {
    state.cancel_flag.store(false, Ordering::SeqCst);
    cdp::clear_user_cancelled_on_live();
}

#[tauri::command]
async fn get_browser_tab_snapshot_cmd(
    state: State<'_, AppState>,
) -> Result<Option<browser_context::BrowserTabSnapshot>, String> {
    let preferred = state.last_main_target.lock().clone();
    Ok(
        browser_context::refresh_browser_snapshot(
            &state.last_browser_snapshot,
            chrome::DEBUG_PORT,
            preferred.as_deref(),
        )
        .await,
    )
}

#[tauri::command]
fn get_settings_cmd() -> Result<settings::AppSettings, String> {
    settings::load_settings()
}

#[tauri::command]
fn detect_chrome_executable_cmd() -> Option<String> {
    chrome::find_chrome_exe().map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
async fn pick_chrome_executable_cmd() -> Result<Option<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(|| {
        let mut dialog = rfd::FileDialog::new().set_title("选择 Chrome / Edge 浏览器");
        #[cfg(target_os = "windows")]
        {
            dialog = dialog.add_filter("可执行文件", &["exe"]);
        }
        #[cfg(target_os = "macos")]
        {
            dialog = dialog.add_filter("应用程序", &["app"]);
        }
        dialog.pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(picked.and_then(|p| {
        chrome::resolve_browser_binary(&p)
            .or_else(|| p.is_file().then_some(p))
            .map(|bin| bin.to_string_lossy().into_owned())
    }))
}

#[tauri::command]
fn save_settings_cmd(
    app: tauri::AppHandle,
    settings: settings::AppSettings,
) -> Result<settings::AppSettings, String> {
    let prev_floater = crate::settings::load_settings()
        .map(|s| s.schedule_floater_enabled)
        .unwrap_or(false);
    settings::save_settings(&settings)?;
    let loaded = settings::load_settings()?;
    // 仅在悬浮窗开关真正变化时操作窗口，避免每次改设置都触发建窗
    if loaded.schedule_floater_enabled != prev_floater {
        schedule_floater::apply_from_settings(&app);
    }
    Ok(loaded)
}

#[tauri::command]
fn dispatch_remote_job_cmd(
    title: String,
    content: String,
    params: Option<std::collections::HashMap<String, String>>,
) -> Result<crate::remote_job::DispatchResult, String> {
    crate::remote_job::dispatch_remote_job(&title, &content, params.unwrap_or_default())
}

#[tauri::command]
async fn remote_execute_cmd(
    app: tauri::AppHandle,
    title: String,
    content: String,
    params: Option<std::collections::HashMap<String, String>>,
) -> Result<crate::run_relay_desktop::RemoteExecuteResult, String> {
    crate::run_relay_desktop::remote_execute(
        app,
        title,
        content,
        params.unwrap_or_default(),
    )
    .await
}

#[tauri::command]
async fn create_pair_code_cmd() -> Result<crate::run_relay_desktop::PairCodeResult, String> {
    crate::run_relay_desktop::create_pair_code().await
}

#[tauri::command]
async fn finish_pending_remote_execute_cmd(
    app: tauri::AppHandle,
) -> Result<Option<crate::run_relay_desktop::RemoteExecuteResult>, String> {
    crate::run_relay_desktop::finish_pending_remote_execute(app).await
}

#[tauri::command]
fn run_relay_status_cmd() -> crate::run_relay_desktop::RunRelayStatus {
    crate::run_relay_desktop::status_snapshot()
}

#[tauri::command]
async fn run_relay_connect_cmd(
    app: tauri::AppHandle,
) -> Result<crate::run_relay_desktop::RunRelayStatus, String> {
    crate::run_relay_desktop::ensure_connected(app);
    Ok(crate::run_relay_desktop::status_snapshot())
}

#[tauri::command]
fn run_relay_human_reply_cmd(
    job_id: String,
    run_token: String,
    text: Option<String>,
    ok: Option<bool>,
    cancel: Option<bool>,
) -> Result<(), String> {
    crate::run_relay_desktop::human_reply(job_id, run_token, text, ok, cancel)
}

#[tauri::command]
async fn run_relay_cancel_job_cmd(
    app: tauri::AppHandle,
    job_id: Option<String>,
    run_token: Option<String>,
) -> Result<(), String> {
    crate::run_relay_desktop::cancel_host_job(app, job_id, run_token).await
}

#[tauri::command]
fn export_accounts_pack_cmd() -> Result<crate::remote_job::ExportAccountsResult, String> {
    crate::remote_job::export_accounts_pack()
}

#[tauri::command]
fn export_host_pair_cmd() -> Result<crate::remote_job::ExportHostPairResult, String> {
    crate::remote_job::export_host_pair()
}

#[tauri::command]
fn export_companion_skill_cmd() -> Result<crate::remote_job::ExportSkillResult, String> {
    crate::remote_job::export_companion_skill()
}

#[tauri::command]
fn export_remote_secret_cmd() -> Result<String, String> {
    let dest = paths::app_data_dir()
        .join("remote-jobs")
        .join("webclaw-remote-secret");
    crate::remote_job::export_secret_copy(&dest)?;
    Ok(dest.display().to_string())
}

#[tauri::command]
fn reset_remote_secret_upload_cmd() -> Result<(), String> {
    let marker = paths::app_data_dir().join(".remote-secret-exported");
    let _ = std::fs::remove_file(marker);
    Ok(())
}

#[tauri::command]
async fn ai_summarize_chat_cmd(chat_transcript: String) -> Result<String, String> {
    ark::summarize_chat_to_recipe(&chat_transcript).await
}

#[tauri::command]
fn list_commands_cmd() -> Result<Vec<commands::CommandDef>, String> {
    commands::list_commands()
}

#[tauri::command]
fn get_command_cmd(id: String) -> Result<Option<commands::CommandDef>, String> {
    commands::get_command(&id)
}

#[tauri::command]
fn save_command_cmd(cmd: commands::CommandDef) -> Result<commands::CommandDef, String> {
    commands::save_command(cmd)
}

#[tauri::command]
fn delete_command_cmd(id: String) -> Result<(), String> {
    commands::delete_command(&id)
}

#[tauri::command]
fn reset_command_script_cmd(id: String) -> Result<commands::CommandDef, String> {
    commands::reset_builtin_script(&id)
}

#[tauri::command]
fn list_scripts_cmd() -> Result<Vec<scripts::ScriptDef>, String> {
    scripts::list_scripts()
}

#[tauri::command]
fn get_script_cmd(id: String) -> Result<Option<scripts::ScriptDef>, String> {
    scripts::get_script(&id)
}

#[tauri::command]
fn save_script_cmd(script: scripts::ScriptDef) -> Result<scripts::ScriptDef, String> {
    scripts::save_script(script)
}

#[tauri::command]
fn delete_script_cmd(id: String) -> Result<(), String> {
    scripts::delete_script(&id)
}

#[tauri::command]
fn reset_script_cmd(id: String) -> Result<scripts::ScriptDef, String> {
    scripts::reset_builtin_script(&id)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ScriptDocPath {
    path: String,
    title: String,
}

/// 将内置业务脚本文档写出到用户目录，供 Markdown 预览打开
#[tauri::command]
fn resolve_script_doc_cmd(doc: String) -> Result<ScriptDocPath, String> {
    let (filename, title, content) = match doc.trim() {
        "guide" => (
            "业务脚本编写指南.md",
            "业务脚本编写指南",
            scripts::builtin::SCRIPT_DOC_GUIDE,
        ),
        "ai_prompt" | "ai-prompt" | "prompt" => (
            "业务脚本-AI生成提示词.md",
            "业务脚本 AI 提示词",
            scripts::builtin::SCRIPT_DOC_AI_PROMPT,
        ),
        _ => return Err("未知文档：请使用 guide 或 ai_prompt".into()),
    };
    let dir = paths::app_data_dir().join("docs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建文档目录失败: {e}"))?;
    let path = dir.join(filename);
    std::fs::write(&path, content).map_err(|e| format!("写入文档失败: {e}"))?;
    Ok(ScriptDocPath {
        path: path.to_string_lossy().into_owned(),
        title: title.into(),
    })
}

fn require_assert_feature() -> Result<(), String> {
    if crate::settings::assert_feature_enabled() {
        Ok(())
    } else {
        Err(
            "断言功能未开启。请在「设置 → 帮助」连续点击版本号 6 次开启，或在「通用」中打开断言能力。"
                .into(),
        )
    }
}

#[tauri::command]
fn list_assert_scripts_cmd() -> Result<Vec<crate::assert_scripts::AssertScriptDef>, String> {
    require_assert_feature()?;
    crate::assert_scripts::list_assert_scripts()
}

#[tauri::command]
fn get_assert_script_cmd(
    id: String,
) -> Result<Option<crate::assert_scripts::AssertScriptDef>, String> {
    require_assert_feature()?;
    crate::assert_scripts::get_assert_script(&id)
}

#[tauri::command]
fn save_assert_script_cmd(
    script: crate::assert_scripts::AssertScriptDef,
) -> Result<crate::assert_scripts::AssertScriptDef, String> {
    require_assert_feature()?;
    crate::assert_scripts::save_assert_script(script)
}

#[tauri::command]
fn delete_assert_script_cmd(id: String) -> Result<(), String> {
    require_assert_feature()?;
    crate::assert_scripts::delete_assert_script(&id)
}

#[tauri::command]
fn reset_assert_script_cmd(id: String) -> Result<crate::assert_scripts::AssertScriptDef, String> {
    require_assert_feature()?;
    crate::assert_scripts::reset_builtin_assert_script(&id)
}

#[tauri::command]
fn list_templates_cmd() -> Result<Vec<templates::TemplateDef>, String> {
    templates::list_templates()
}

#[tauri::command]
fn get_template_cmd(id: String) -> Result<Option<templates::TemplateDef>, String> {
    templates::get_by_id(&id)
}

#[tauri::command]
fn save_template_cmd(template: templates::TemplateDef) -> Result<templates::TemplateDef, String> {
    templates::save_template(template)
}

#[tauri::command]
fn delete_template_cmd(id: String) -> Result<(), String> {
    templates::delete_template(&id)
}

#[tauri::command]
fn reset_builtin_template_cmd(id: String) -> Result<templates::TemplateDef, String> {
    templates::reset_builtin_template(&id)
}

#[tauri::command]
async fn import_template_file_cmd(
    window: tauri::Window,
    source_path: Option<String>,
    name: Option<String>,
    description: Option<String>,
) -> Result<templates::TemplateDef, String> {
    let path = if let Some(p) = source_path {
        std::path::PathBuf::from(p)
    } else {
        pick_template_file(Some(window), "选择模板文件").await?
    };
    templates::import_template_file(&path, name, description)
}

#[tauri::command]
async fn replace_template_file_cmd(
    window: tauri::Window,
    id: String,
    source_path: Option<String>,
) -> Result<templates::TemplateDef, String> {
    let path = if let Some(p) = source_path {
        std::path::PathBuf::from(p)
    } else {
        pick_template_file(Some(window), "选择新的模板文件").await?
    };
    templates::replace_template_file(&id, &path)
}

async fn pick_template_file(
    window: Option<tauri::Window>,
    title: &str,
) -> Result<std::path::PathBuf, String> {
    let title = title.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title(&title);
        if let Some(win) = window {
            dialog = dialog.set_parent(&win);
        }
        dialog.pick_file()
    })
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "已取消".to_string())
}

#[tauri::command]
fn open_templates_dir_cmd() -> Result<(), String> {
    let dir = templates::templates_dir();
    templates::ensure_template_files()?;
    open_path_in_shell(&dir)
}

#[tauri::command]
fn sync_remote_scripts_cmd(
    scripts: Vec<scripts::RemoteScriptInput>,
) -> Result<Vec<scripts::ScriptDef>, String> {
    scripts::sync_remote_scripts(scripts)
}

#[tauri::command]
fn sync_remote_templates_cmd(
    templates: Vec<templates::RemoteTemplateInput>,
) -> Result<Vec<templates::TemplateDef>, String> {
    templates::sync_remote_templates(templates)
}

#[tauri::command]
async fn pick_template_file_path_cmd(window: tauri::Window) -> Result<String, String> {
    let path = pick_template_file(Some(window), "选择模板文件").await?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn zhj_upload_file_cmd(
    url: String,
    headers: HashMap<String, String>,
    file_path: String,
) -> Result<serde_json::Value, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("请求地址为空".into());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("非法请求地址".into());
    }
    let path = std::path::PathBuf::from(file_path.trim());
    if !path.is_file() {
        return Err(format!("文件不存在: {}", path.display()));
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str("application/octet-stream")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new().part("file", part);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let mut req = client.post(url).multipart(form);
    let mut merged: HashMap<String, String> = HashMap::new();
    for (key, value) in &headers {
        let k = key.trim();
        if k.is_empty() {
            continue;
        }
        if k.eq_ignore_ascii_case("host")
            || k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("content-type")
        {
            continue;
        }
        let cleaned = value
            .chars()
            .filter(|c| *c != '\r' && *c != '\n')
            .collect::<String>()
            .trim()
            .to_string();
        merged.insert(k.to_ascii_lowercase(), cleaned);
    }
    for (k, v) in merged {
        req = req.header(k, v);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("上传失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(mut v) => {
            normalize_zhj_api_json(&mut v);
            Ok(v)
        }
        Err(_) => {
            let preview: String = text.chars().take(200).collect();
            Err(format!(
                "响应异常 HTTP {}：{}",
                status.as_u16(),
                if preview.is_empty() {
                    status.canonical_reason().unwrap_or("未知错误").to_string()
                } else {
                    preview
                }
            ))
        }
    }
}

#[tauri::command]
fn stage_template_file_cmd(source_path: String, preferred_name: String) -> Result<String, String> {
    templates::stage_template_file(
        std::path::Path::new(source_path.trim()),
        preferred_name.trim(),
    )
    .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
async fn zhj_download_file_cmd(
    url: String,
    headers: HashMap<String, String>,
    dest_path: String,
) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("请求地址为空".into());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("非法请求地址".into());
    }
    let dest = std::path::PathBuf::from(dest_path.trim());
    if dest.as_os_str().is_empty() {
        return Err("目标路径为空".into());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let mut req = client.get(url);
    let mut merged: HashMap<String, String> = HashMap::new();
    for (key, value) in &headers {
        let k = key.trim();
        if k.is_empty() {
            continue;
        }
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let cleaned = value
            .chars()
            .filter(|c| *c != '\r' && *c != '\n')
            .collect::<String>()
            .trim()
            .to_string();
        merged.insert(k.to_ascii_lowercase(), cleaned);
    }
    for (k, v) in merged {
        req = req.header(k, v);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败 HTTP {}", resp.status().as_u16()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
    Ok(dest.to_string_lossy().into_owned())
}

#[tauri::command]
async fn ai_assist_command_script(
    command_name: String,
    keyword: String,
    runner: String,
    prompt: String,
    existing_script: Option<String>,
) -> Result<String, String> {
    ark::generate_command_script(
        &command_name,
        &keyword,
        &runner,
        &prompt,
        existing_script.as_deref(),
    )
    .await
}

fn register_ai_stream_cancel(state: &AppState, stream_id: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    state
        .ai_stream_cancels
        .lock()
        .insert(stream_id.to_string(), flag.clone());
    flag
}

#[tauri::command]
fn ai_stream_abort(state: State<'_, AppState>, stream_id: String) -> Result<(), String> {
    if let Some(flag) = state.ai_stream_cancels.lock().get(&stream_id) {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
async fn ai_stream_recipe_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    stream_id: String,
    prompt: String,
    existing_content: Option<String>,
    chat_transcript: Option<String>,
) -> Result<(), String> {
    let preferred = state.last_main_target.lock().clone();
    let browser = browser_context::refresh_browser_snapshot(
        &state.last_browser_snapshot,
        chrome::DEBUG_PORT,
        preferred.as_deref(),
    )
    .await;
    let ctx = ark::RecipeAiContext {
        browser,
        chat_transcript,
    };
    let body = ark::build_recipe_chat_body(
        &prompt,
        existing_content.as_deref(),
        Some(&ctx),
    )?;
    let cancel = register_ai_stream_cancel(&state, &stream_id);
    let cancels = state.ai_stream_cancels.clone();
    let sid = stream_id.clone();
    let app2 = app.clone();

    tokio::spawn(async move {
        let result = ark::stream_chat_completion(&app2, &sid, body, cancel.clone()).await;
        let aborted = cancel.load(Ordering::SeqCst);
        cancels.lock().remove(&sid);

        match result {
            Ok(streamed) => {
                let content = ark::clean_recipe_output(&streamed.content);
                let _ = app2.emit(
                    "ai-stream-done",
                    ark::AiStreamDonePayload {
                        stream_id: sid,
                        content,
                        reasoning: Some(streamed.reasoning).filter(|s| !s.trim().is_empty()),
                        aborted,
                        error: None,
                        usage: streamed.usage,
                    },
                );
            }
            Err(e) => {
                let _ = app2.emit(
                    "ai-stream-done",
                    ark::AiStreamDonePayload {
                        stream_id: sid,
                        content: String::new(),
                        reasoning: None,
                        aborted,
                        error: Some(e),
                        usage: None,
                    },
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn ai_stream_command_script_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    stream_id: String,
    command_name: String,
    keyword: String,
    runner: String,
    prompt: String,
    existing_script: Option<String>,
) -> Result<(), String> {
    let body = ark::build_script_chat_body(
        &command_name,
        &keyword,
        &runner,
        &prompt,
        existing_script.as_deref(),
    )?;
    let cancel = register_ai_stream_cancel(&state, &stream_id);
    let cancels = state.ai_stream_cancels.clone();
    let sid = stream_id.clone();
    let app2 = app.clone();

    tokio::spawn(async move {
        let result = ark::stream_chat_completion(&app2, &sid, body, cancel.clone()).await;
        let aborted = cancel.load(Ordering::SeqCst);
        cancels.lock().remove(&sid);

        match result {
            Ok(streamed) => {
                let content = ark::clean_script_output(&streamed.content);
                let _ = app2.emit(
                    "ai-stream-done",
                    ark::AiStreamDonePayload {
                        stream_id: sid,
                        content,
                        reasoning: Some(streamed.reasoning).filter(|s| !s.trim().is_empty()),
                        aborted,
                        error: None,
                        usage: streamed.usage,
                    },
                );
            }
            Err(e) => {
                let _ = app2.emit(
                    "ai-stream-done",
                    ark::AiStreamDonePayload {
                        stream_id: sid,
                        content: String::new(),
                        reasoning: None,
                        aborted,
                        error: Some(e),
                        usage: None,
                    },
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
fn get_app_paths() -> Result<serde_json::Value, String> {
    paths::ensure_user_scope_initialized();
    Ok(serde_json::json!({
        "app_root": paths::app_root_dir().to_string_lossy(),
        "user_key": paths::current_user_key(),
        "app_data": paths::app_data_dir().to_string_lossy(),
        "ocr_models": paths::ocr_models_dir().to_string_lossy(),
        "classify_models": paths::classify_models_dir().to_string_lossy(),
        "wechat_img_cache": paths::wechat_img_cache_dir().to_string_lossy(),
        "recipes_file": paths::recipes_file().to_string_lossy(),
        "commands_file": paths::commands_file().to_string_lossy(),
        "templates_dir": paths::templates_dir().to_string_lossy(),
        "templates_file": paths::templates_meta_file().to_string_lossy(),
        "settings_file": paths::settings_file().to_string_lossy(),
        "web_systems_file": paths::web_systems_file().to_string_lossy(),
        "chrome_profile": chrome::debug_user_data_dir().to_string_lossy(),
        "runs_dir": paths::runs_dir().to_string_lossy(),
        "chat_dir": chat_history::chat_dir().to_string_lossy(),
    }))
}

#[tauri::command]
fn local_cache_status_cmd() -> Result<serde_json::Value, String> {
    let status = rhai_runtime::local_ocr::local_cache_status()?;
    serde_json::to_value(status).map_err(|e| e.to_string())
}

/// 打开系统模型缓存目录（含 ocr-models / classify-models）
#[tauri::command]
fn local_cache_open_system_dir_cmd() -> Result<(), String> {
    let _ = paths::ocr_models_dir();
    let _ = paths::classify_models_dir();
    let dir = paths::app_root_dir();
    let _ = std::fs::create_dir_all(&dir);
    open_path_in_shell(&dir)
}

#[tauri::command]
fn local_cache_ensure_ocr_cmd(app: tauri::AppHandle) -> Result<(), String> {
    let app2 = app.clone();
    rhai_runtime::local_ocr::spawn_ensure_ocr_models(move |progress| {
        let _ = app2.emit("ocr-cache-progress", &progress);
        if let Ok(status) = rhai_runtime::local_ocr::local_cache_status() {
            let _ = app2.emit("local-cache-status", &status);
        }
    });
    Ok(())
}

#[tauri::command]
fn local_cache_clear_ocr_cmd() -> Result<serde_json::Value, String> {
    rhai_runtime::local_ocr::clear_ocr_models()?;
    let status = rhai_runtime::local_ocr::local_cache_status()?;
    serde_json::to_value(status).map_err(|e| e.to_string())
}

#[tauri::command]
fn local_cache_clear_runtime_cmd() -> Result<serde_json::Value, String> {
    rhai_runtime::local_ocr::clear_user_app_data()?;
    let status = rhai_runtime::local_ocr::local_cache_status()?;
    serde_json::to_value(status).map_err(|e| e.to_string())
}

#[tauri::command]
fn local_cache_clear_other_users_cmd() -> Result<serde_json::Value, String> {
    rhai_runtime::local_ocr::clear_other_users_app_data()?;
    let status = rhai_runtime::local_ocr::local_cache_status()?;
    serde_json::to_value(status).map_err(|e| e.to_string())
}

#[tauri::command]
fn load_chat_history_cmd() -> Result<Vec<serde_json::Value>, String> {
    chat_history::load_all_sessions()
}

#[tauri::command]
fn save_chat_session_cmd(session: serde_json::Value) -> Result<(), String> {
    chat_history::save_session(session)
}

#[tauri::command]
fn save_chat_history_cmd(sessions: Vec<serde_json::Value>) -> Result<(), String> {
    chat_history::save_all_sessions(sessions)
}

#[tauri::command]
fn delete_chat_sessions_cmd(ids: Vec<String>) -> Result<(), String> {
    chat_history::delete_sessions(&ids)
}

#[tauri::command]
fn read_chat_run_log_cmd(run_dir: String) -> Result<String, String> {
    chat_history::read_run_log(&run_dir)
}

#[tauri::command]
fn recover_chat_sessions_from_runs_cmd() -> Result<Vec<serde_json::Value>, String> {
    chat_history::recover_sessions_from_runs()
}

#[tauri::command]
fn get_app_version_cmd(app: tauri::AppHandle) -> String {
    let _ = app;
    updater::current_app_version()
}

#[tauri::command]
async fn check_for_update_cmd() -> Result<updater::UpdateCheckResult, String> {
    updater::check_for_update().await
}

#[tauri::command]
async fn install_update_cmd(app: tauri::AppHandle) -> Result<String, String> {
    updater::download_and_install(&app).await
}

#[tauri::command]
async fn check_update_cmd(app: tauri::AppHandle) -> Result<updater::CheckUpdateResult, String> {
    let _ = app;
    let v = updater::current_app_version();
    let settings = settings::load_settings().unwrap_or_default();
    let base = settings::resolve_update_base_url(&settings);
    updater::check_update(&v, &base).await
}

#[tauri::command]
async fn apply_update_cmd(app: tauri::AppHandle) -> Result<(), String> {
    let settings = settings::load_settings().unwrap_or_default();
    let base = settings::resolve_update_base_url(&settings);
    let check = updater::check_update(&updater::current_app_version(), &base).await?;
    if !check.update_available {
        return Err("没有可用更新".into());
    }
    let latest = check
        .latest
        .ok_or_else(|| "缺少更新元数据".to_string())?;
    let path = updater::download_update(&app, &base, &latest).await?;
    updater::spawn_replace_and_exit(&path, &app)
}

#[tauri::command]
fn open_run_artifacts_dir_cmd(run_dir: String) -> Result<(), String> {
    let path = std::path::PathBuf::from(&run_dir);
    if !path.is_dir() {
        return Err("产物目录不存在".into());
    }
    open_path_in_shell(&path)
}

/// 手动添加附件到当前运行的 files/（脚本可与 downloads/ 互通查找）
///
/// - 后台任务：须先暂停，传 `taskId`
/// - 主窗口：传 `runDir` 和/或 `chatSessionId`（无产物目录时会按对话创建）
#[tauri::command]
async fn upload_run_attachments_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    window: tauri::Window,
    task_id: Option<String>,
    run_dir: Option<String>,
    chat_session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let (session, paused_ctrl) =
        resolve_upload_session(&state, task_id.as_deref(), run_dir.as_deref(), chat_session_id.as_deref())?;

    let paths = pick_run_attachment_files(Some(window)).await?;
    if paths.is_empty() {
        return Err("已取消".into());
    }

    let mut added = Vec::new();
    for path in &paths {
        let art = session
            .ingest_attachment_path(path, None, None)
            .map_err(|e| format!("添加附件失败 {}: {e}", path.display()))?;
        added.push(art.name.clone());
    }

    if let Some(ctrl) = paused_ctrl.as_ref() {
        let names = added.join("、");
        ctrl.push_log(format!("已添加附件（{}）: {names}", added.len()));
        run_artifacts::emit_artifacts_update(&app, &session);
        // 顺便刷新进度窗产物列表
        if let Some(snap) = ctrl.last_progress.lock().clone() {
            let mut snap = snap;
            snap.run_dir = Some(session.run_dir_display());
            snap.artifacts = Some(session.list_artifacts());
            task_progress::emit_task_progress(&app, &session.task_id, snap);
        }
    } else {
        run_artifacts::emit_artifacts_update(&app, &session);
    }

    Ok(serde_json::json!({
        "runDir": session.run_dir_display(),
        "taskId": session.task_id,
        "added": added,
        "artifacts": session.list_artifacts(),
    }))
}

/// 删除运行产物文件（真实删盘，支持批量）。路径必须位于产物目录内。
#[tauri::command]
fn delete_run_artifacts_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    task_id: Option<String>,
    run_dir: Option<String>,
    chat_session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    if paths.iter().all(|p| p.trim().is_empty()) {
        return Err("未选择要删除的文件".into());
    }
    let (session, paused_ctrl) =
        resolve_upload_session(&state, task_id.as_deref(), run_dir.as_deref(), chat_session_id.as_deref())?;

    let (deleted, errors, artifacts) = session.delete_artifacts(&paths)?;

    if let Some(ctrl) = paused_ctrl.as_ref() {
        let names = deleted.join("、");
        ctrl.push_log(format!("已删除产物（{}）: {names}", deleted.len()));
        run_artifacts::emit_artifacts_update(&app, &session);
        if let Some(snap) = ctrl.last_progress.lock().clone() {
            let mut snap = snap;
            snap.run_dir = Some(session.run_dir_display());
            snap.artifacts = Some(artifacts.clone());
            task_progress::emit_task_progress(&app, &session.task_id, snap);
        }
    } else {
        run_artifacts::emit_artifacts_update(&app, &session);
    }

    Ok(serde_json::json!({
        "runDir": session.run_dir_display(),
        "taskId": session.task_id,
        "deleted": deleted,
        "errors": errors,
        "artifacts": artifacts,
    }))
}

fn resolve_upload_session(
    state: &AppState,
    task_id: Option<&str>,
    run_dir: Option<&str>,
    chat_session_id: Option<&str>,
) -> Result<(std::sync::Arc<run_artifacts::RunSession>, Option<TaskControl>), String> {
    let tid = task_id.map(str::trim).filter(|s| !s.is_empty());
    let dir = run_dir.map(str::trim).filter(|s| !s.is_empty());
    let chat_id = chat_session_id.map(str::trim).filter(|s| !s.is_empty());

    // 后台任务：必须暂停
    if let Some(tid) = tid.filter(|s| *s != "main") {
        let ctrl = state
            .task_registry
            .get(tid)
            .ok_or_else(|| "任务不存在或已结束".to_string())?;
        ctrl.require_paused()?;
        let session = ctrl
            .run_session
            .lock()
            .clone()
            .ok_or_else(|| "运行目录尚未就绪，请稍后再试".to_string())?;
        return Ok((session, Some(ctrl)));
    }

    // 主窗口 / 已有产物目录
    if let Some(dir) = dir {
        let path = std::path::PathBuf::from(dir);
        if !path.is_dir() {
            return Err("产物目录不存在".into());
        }
        // 若后台任务正绑着同一目录且在跑，仍要求暂停
        let mut hit: Option<(std::sync::Arc<run_artifacts::RunSession>, TaskControl)> = None;
        state.task_registry.for_each_active(|_id, ctrl| {
            if hit.is_some() {
                return;
            }
            if let Some(s) = ctrl.run_session.lock().as_ref() {
                if s.run_dir == path {
                    hit = Some((s.clone(), ctrl.clone()));
                }
            }
        });
        if let Some((session, ctrl)) = hit {
            if session.task_id != "main" {
                ctrl.require_paused()?;
                return Ok((session, Some(ctrl)));
            }
            return Ok((session, None));
        }
        let session = std::sync::Arc::new(run_artifacts::RunSession::open_for_upload(
            &path,
            tid.unwrap_or("main"),
            "手动附件",
        )?);
        return Ok((session, None));
    }

    // 尚无产物目录：按对话 ID 创建 sessions/<id>/
    if let Some(sid) = chat_id {
        let path = run_artifacts::RunSession::ensure_chat_session_dir(sid)?;
        let session = std::sync::Arc::new(run_artifacts::RunSession::open_for_upload(
            &path,
            "main",
            "手动附件",
        )?);
        return Ok((session, None));
    }

    Err("请先选择对话或开始一次运行".into())
}

async fn pick_run_attachment_files(
    window: Option<tauri::Window>,
) -> Result<Vec<std::path::PathBuf>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new()
            .set_title("选择要添加到 files 的附件")
            .add_filter(
                "表格与常用附件",
                &["xlsx", "xls", "csv", "xlsm", "pdf", "zip", "png", "jpg", "jpeg"],
            )
            .add_filter("所有文件", &["*"]);
        if let Some(win) = window {
            dialog = dialog.set_parent(&win);
        }
        dialog.pick_files().unwrap_or_default()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_artifact_file_cmd(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("文件不存在: {path}"));
    }
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "md" || ext == "markdown" {
        // Windows 上在同步命令里创建 Webview 会 WebView2 死锁；改由前端主窗模态预览。
        return emit_markdown_preview(&app, &p);
    }
    open_file_with_default_app(&p)
}

#[tauri::command]
fn open_path_externally_cmd(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return Err("路径不存在".into());
    }
    if p.is_dir() {
        return open_path_in_shell(&p);
    }
    open_file_with_default_app(&p)
}

/// 将预览 HTML 写入临时文件并用默认浏览器打开（.html 关联）。
#[tauri::command]
fn open_html_in_browser_cmd(title: String, body_html: String) -> Result<(), String> {
    if body_html.len() > 12_000_000 {
        return Err("内容过大，无法在浏览器中打开".into());
    }
    let title = title.trim();
    let title = if title.is_empty() { "文档预览" } else { title };
    let safe_title = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>{safe_title}</title>
<style>
  :root {{
    --brand: #006f3f;
    --brand-soft: #e8f5ec;
    --text: #1a3328;
    --muted: #5a7a68;
    --border: #d7e3db;
    --bg: #f4f6f5;
    --card: #ffffff;
    --code-bg: #f0f3f1;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0;
    font-family: "Segoe UI", "Microsoft YaHei", "PingFang SC", system-ui, sans-serif;
    color: var(--text);
    background: var(--bg);
    line-height: 1.65;
  }}
  main {{
    max-width: 960px;
    margin: 0 auto;
    padding: 28px 20px 48px;
  }}
  .md-doc {{
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 22px 24px 28px;
    box-shadow: 0 2px 8px rgba(26, 51, 40, 0.06);
  }}
  h1 {{ font-size: 1.4rem; margin: 0 0 12px; color: var(--brand); }}
  h2 {{ font-size: 1.12rem; margin: 22px 0 10px; }}
  h3 {{ font-size: 1rem; margin: 18px 0 8px; }}
  p, ul, blockquote {{ margin: 0 0 12px; }}
  ul {{ padding-left: 1.3em; }}
  blockquote {{
    border-left: 3px solid var(--brand);
    padding: 8px 12px;
    background: var(--brand-soft);
    color: var(--muted);
    border-radius: 0 8px 8px 0;
  }}
  hr {{ border: none; border-top: 1px solid var(--border); margin: 18px 0; }}
  code {{
    font-family: "Cascadia Code", Consolas, monospace;
    font-size: 0.88em;
    background: var(--code-bg);
    padding: 1px 5px;
    border-radius: 4px;
  }}
  .md-code-block {{
    margin: 12px 0;
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
    background: var(--code-bg);
  }}
  .md-code-toolbar {{
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 6px 10px;
    font-size: 0.75rem;
    color: var(--muted);
    border-bottom: 1px solid var(--border);
    background: #e8ece9;
  }}
  .md-code-block pre {{
    margin: 0;
    padding: 12px 14px;
    overflow: auto;
    font-family: "Cascadia Code", Consolas, monospace;
    font-size: 0.86rem;
    white-space: pre;
  }}
  .md-code-block pre code {{ background: transparent; padding: 0; }}
  .md-table-block, .md-table-wrap {{
    overflow-x: auto;
    margin: 14px 0 8px;
    border: 1px solid var(--border);
    border-radius: 10px;
  }}
  table {{
    border-collapse: collapse;
    width: max-content;
    min-width: 100%;
    font-size: 0.86rem;
  }}
  th, td {{
    border-bottom: 1px solid var(--border);
    padding: 9px 12px;
    text-align: left;
    vertical-align: top;
    max-width: 320px;
    word-break: break-word;
  }}
  th {{ background: var(--brand-soft); font-weight: 650; white-space: nowrap; }}
  .md-section {{ margin: 8px 0 14px; }}
  .md-section-head {{ display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }}
  .md-section-head h2, .md-section-head h3 {{ margin: 0; flex: 1; }}
  button {{ display: none !important; }}
</style>
</head>
<body>
<main><div class="md-doc">{body_html}</div></main>
</body>
</html>"#
    );
    write_temp_html_and_open(&html)
}

/// 将完整 HTML 文档写入临时文件并用默认浏览器打开（保留原样式，如操作手册）。
#[tauri::command]
fn open_html_document_in_browser_cmd(html: String) -> Result<(), String> {
    if html.len() > 12_000_000 {
        return Err("内容过大，无法在浏览器中打开".into());
    }
    let trimmed = html.trim();
    if trimmed.is_empty() {
        return Err("没有可打开的内容".into());
    }
    write_temp_html_and_open(trimmed)
}

fn write_temp_html_and_open(html: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join("webclaw-md-preview");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let file = dir.join(format!(
        "preview-{}.html",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::write(&file, html.as_bytes()).map_err(|e| format!("写入预览失败: {e}"))?;
    open_file_with_default_app(&file)
}

#[tauri::command]
fn read_text_file_cmd(path: String) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("文件不存在: {path}"));
    }
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    if meta.len() > 8_000_000 {
        return Err("文件过大（超过 8MB），请用系统程序打开".into());
    }
    std::fs::read_to_string(&p).map_err(|e| format!("读取失败: {e}"))
}

#[tauri::command]
fn take_pending_markdown_path_cmd() -> Option<String> {
    PENDING_MARKDOWN_PATH.lock().clone()
}

#[tauri::command]
fn close_markdown_viewer_cmd(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("md-viewer") {
        let _ = win.close();
    }
    Ok(())
}

fn emit_markdown_preview(app: &tauri::AppHandle, path: &std::path::Path) -> Result<(), String> {
    let path_str = path.to_string_lossy().to_string();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("文档预览")
        .to_string();
    *PENDING_MARKDOWN_PATH.lock() = Some(path_str.clone());
    let payload = serde_json::json!({
        "path": path_str,
        "name": name,
    });
    // 主窗模态预览；不创建独立 Webview（Windows 同步命令创建窗口会 WebView2 死锁）
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
        let _ = main.emit("open-markdown-preview", &payload);
    }
    let _ = app.emit("open-markdown-preview", &payload);
    for (label, win) in app.webview_windows() {
        if label.starts_with("task-") {
            let _ = win.emit("open-markdown-preview", &payload);
        }
    }
    Ok(())
}

fn open_file_with_default_app(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.to_string_lossy()])
            .creation_flags(DETACHED_PROCESS)
            .spawn()
            .map_err(|e| format!("无法用默认程序打开文件: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn open_path_in_shell(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        let abs = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        // explorer 对 / 路径不友好，统一成 Windows 分隔符
        let abs_s = abs.to_string_lossy().replace('/', "\\");
        std::process::Command::new("explorer")
            .arg(&abs_s)
            .creation_flags(DETACHED_PROCESS)
            .spawn()
            .map_err(|e| format!("无法打开目录: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_logs(state: State<'_, AppState>) -> Vec<String> {
    state.logs.lock().clone()
}

#[tauri::command]
fn clear_logs(app: tauri::AppHandle, state: State<'_, AppState>) {
    state.logs.lock().clear();
    state.push_log("日志已清空");
    emit_logs(&app, &state);
}

#[tauri::command]
async fn get_active_browser_cmd(
    state: State<'_, AppState>,
) -> Result<browser_session::BrowserSessionUpdate, String> {
    let preferred = state.last_main_target.lock().clone();
    Ok(browser_session::BrowserSessionUpdate {
        session: browser_session::resolve_browser_session(
            &state.active_browser,
            preferred.as_deref(),
        )
        .await,
    })
}

#[tauri::command]
fn captcha_submit_cmd(state: State<'_, AppState>, prompt_id: String, code: String) -> Result<(), String> {
    let tx = state.captcha_waiters.lock().remove(&prompt_id);
    if let Some(tx) = tx {
        let _ = tx.send(captcha::CaptchaResponse::Submitted(code));
        Ok(())
    } else {
        Err("验证码请求已过期或已处理".into())
    }
}

#[tauri::command]
fn captcha_cancel_cmd(state: State<'_, AppState>, prompt_id: String) -> Result<(), String> {
    let tx = state.captcha_waiters.lock().remove(&prompt_id);
    if let Some(tx) = tx {
        let _ = tx.send(captcha::CaptchaResponse::Cancelled);
        Ok(())
    } else {
        Err("验证码请求已过期或已处理".into())
    }
}

#[tauri::command]
async fn show_browser_cmd(state: State<'_, AppState>) -> Result<String, String> {
    // 确保调试 Chrome 在跑；用户点「显示浏览器」时可能刚重启过浏览器
    let _ = chrome::ensure_debug_chrome_ready(&state.chrome_child, false).await?;
    let preferred = state.last_main_target.lock().clone();
    let session = browser_session::resolve_browser_session(
        &state.active_browser,
        preferred.as_deref(),
    )
    .await
    .ok_or_else(|| "请先启动 Debug Chrome 并打开目标页".to_string())?;

    // 优先附着当前仍存活的标签；缓存的 target 可能已关闭
    let client = match cdp::CdpClient::connect_to_target_on(session.port, &session.target_id).await
    {
        Ok(c) => c,
        Err(_) => cdp::CdpClient::connect_on(session.port).await.map_err(|e| {
            if e.to_ascii_lowercase().contains("closed connection") {
                "浏览器连接已断开，请关闭调试 Chrome 后重新打开，再点「显示浏览器」".into()
            } else {
                e
            }
        })?,
    };
    client.activate_tab().await.map_err(|e| {
        if e.to_ascii_lowercase().contains("closed connection")
            || e.contains("Browser CDP 连接已断开")
        {
            "浏览器连接已断开，请关闭调试 Chrome 后重新打开，再点「显示浏览器」".into()
        } else {
            e
        }
    })?;
    Ok(format!(
        "已置前 | 端口 {} | 标签「{}」",
        client.port,
        if client.target_title.is_empty() {
            session.target_title.as_str()
        } else {
            client.target_title.as_str()
        }
    ))
}

#[tauri::command]
fn stop_execution_cmd(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.cancel_flag.store(true, Ordering::SeqCst);
    // 仅取消任务并打断挂起 CDP；不杀浏览器（OOM 由执行器自动重启）
    state.task_registry.stop_all();
    cdp::abort_all_pending("已取消");
    state.task_registry.for_each_active(|task_id, ctrl| {
        task_progress::emit_task_control_feedback(&app, task_id, ctrl);
    });
    state.push_log(
        "收到停止请求：已中止当前步骤（浏览器保持运行）…".to_string(),
    );
    emit_logs(&app, &state);
    Ok(())
}

#[tauri::command]
fn task_pause_cmd(app: tauri::AppHandle, state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    state.task_registry.pause_task(&task_id)?;
    if let Some(ctrl) = state.task_registry.get(&task_id) {
        task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
    }
    Ok(())
}

#[tauri::command]
fn task_resume_cmd(app: tauri::AppHandle, state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    state.task_registry.resume_task(&task_id)?;
    if let Some(ctrl) = state.task_registry.get(&task_id) {
        task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
    }
    Ok(())
}

#[tauri::command]
fn task_stop_cmd(app: tauri::AppHandle, state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    let port = state
        .task_registry
        .get(&task_id)
        .and_then(|c| c.debug_port.lock().clone());
    state.task_registry.stop_task(&task_id)?;
    if let Some(port) = port {
        cdp::abort_pending_on_port(port, "已取消");
    } else {
        cdp::abort_all_pending("已取消");
    }
    if let Some(ctrl) = state.task_registry.get(&task_id) {
        task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
    }
    Ok(())
}

/// 强制重启共用调试浏览器（OOM/卡死、CDP 连不上时）。会丢失当前标签。
#[tauri::command]
async fn restart_debug_browser_cmd(state: State<'_, AppState>) -> Result<String, String> {
    *state.last_main_target.lock() = None;
    *state.active_browser.lock() = None;
    chrome::force_restart_debug_chrome(&state.chrome_child, false).await
}

#[tauri::command]
fn list_public_params_cmd() -> Result<crate::public_params::PublicParamsCatalog, String> {
    crate::public_params::list_public_params_catalog()
}

#[tauri::command]
fn save_public_params_cmd(params: Vec<recipe::RecipeParamKv>) -> Result<(), String> {
    crate::public_params::save_personal_public_params(&params)
}

#[tauri::command]
fn recipe_content_needs_browser_cmd(content: String) -> Result<bool, String> {
    let content = recipe::normalize_automation_text(&content);
    if content.trim().is_empty() {
        return Ok(false);
    }
    // 只判断步骤类型，不校验 {{占位符}}；解析失败则视为无需浏览器（避免纯脚本误弹浏览器条）
    match recipe::parse_recipe_steps(&content) {
        Ok(steps) => Ok(steps
            .iter()
            .any(|s| commands::step_needs_browser(&s.keyword, &s.args))),
        Err(_) => Ok(false),
    }
}

#[tauri::command]
fn parse_recipe_params_cmd(content: String) -> Result<Vec<recipe::RecipeParamKv>, String> {
    recipe::extract_recipe_params_only(&content)
}

#[tauri::command]
fn list_skill_invocations_cmd(
    content: String,
) -> Result<Vec<recipe::SkillInvocationInfo>, String> {
    recipe::list_skill_invocations(&content)
}

#[tauri::command]
fn bind_skill_invocation_params_cmd(
    content: String,
    bindings: Vec<recipe::SkillParamBinding>,
) -> Result<String, String> {
    recipe::bind_skill_invocation_params(&content, &bindings)
}

#[tauri::command]
fn merge_recipe_params_cmd(
    content: String,
    params: Vec<recipe::RecipeParamKv>,
    validate_placeholders: Option<bool>,
    rename_from: Option<String>,
    rename_to: Option<String>,
) -> Result<String, String> {
    let validate = validate_placeholders.unwrap_or(true);
    recipe::merge_params_into_content_ex(
        &content,
        &params,
        validate,
        rename_from.as_deref(),
        rename_to.as_deref(),
    )
}

#[tauri::command]
fn task_set_recipe_params_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    params: Vec<recipe::RecipeParamKv>,
) -> Result<(), String> {
    let ctrl = state
        .task_registry
        .get(&task_id)
        .ok_or_else(|| "任务不存在或已结束".to_string())?;
    ctrl.require_paused()?;
    let map = recipe::ordered_vec_to_params(&params)?;
    // 顶部栏回写不应抹掉「动态参数」；保留 runtime_only 项
    let mut merged = map;
    {
        let runtime_only = ctrl.runtime_only_params.lock().clone();
        let existing = ctrl.recipe_params.lock().clone();
        for name in runtime_only {
            if let Some(v) = existing.get(&name) {
                merged.insert(name, v.clone());
            }
        }
    }
    // 校验当前步骤模板中的占位符仍可解析
    let steps = ctrl.live_steps.lock().clone();
    for step in &steps {
        let _ = recipe::substitute_recipe_params(&step.args, &merged)?;
    }
    let n = params.len();
    *ctrl.recipe_params.lock() = merged;
    ctrl.refresh_pending_step_labels();
    ctrl.push_log(format!("已更新参数（{n} 项）"));
    task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
    Ok(())
}

#[tauri::command]
fn task_insert_steps_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    content: String,
) -> Result<(), String> {
    let ctrl = state
        .task_registry
        .get(&task_id)
        .ok_or_else(|| "任务不存在或已结束".to_string())?;
    ctrl.require_paused()?;
    let content = content.trim();
    if content.is_empty() {
        return Err("请输入要插入的指令".into());
    }
    let existing = ctrl.recipe_params.lock().clone();
    let existing_runtime_only = ctrl.runtime_only_params.lock().clone();
    let (content, params, runtime_only) =
        recipe::apply_recipe_params_ex(content, &existing, &existing_runtime_only)?;
    *ctrl.recipe_params.lock() = params.clone();
    *ctrl.runtime_only_params.lock() = runtime_only;
    let content = crate::web_systems::substitute_web_system_vars(&content)?;
    let mut steps = if content.trim().is_empty() {
        Vec::new()
    } else {
        recipe::parse_recipe_steps(&content)?
    };
    // 静态参数已剥离；含 {{页签地址}} 等的参数/动态参数步骤保留以便执行时捕获
    if steps.is_empty() {
        // 允许暂停时仅用「参数名=值」覆盖当前任务参数
        let detail = recipe::params_to_ordered_vec(&params)
            .into_iter()
            .map(|e| format!("{}={}", e.name, e.value))
            .collect::<Vec<_>>()
            .join("，");
        ctrl.refresh_pending_step_labels();
        ctrl.push_log(format!("已更新参数：{detail}"));
        task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
        return Ok(());
    }
    let n = steps.len();
    ctrl.insert_steps.lock().extend(steps);
    ctrl.push_log(format!("排队插入 {n} 步指令"));
    task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
    Ok(())
}

#[tauri::command]
fn task_skip_step_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    step_index: u32,
) -> Result<(), String> {
    let ctrl = state
        .task_registry
        .get(&task_id)
        .ok_or_else(|| "任务不存在或已结束".to_string())?;
    ctrl.require_paused()?;
    if step_index == 0 {
        return Err("无效的步骤序号".into());
    }
    let i = (step_index as usize) - 1;
    {
        let mut recs = ctrl.step_records.lock();
        let rec = recs.get_mut(i).ok_or_else(|| "步骤不存在".to_string())?;
        if rec.status != "pending" && rec.status != "skipped" {
            return Err("只能跳过尚未执行的步骤".into());
        }
        rec.status = "skipped".into();
        rec.detail = "已跳过".into();
    }
    ctrl.skip_set.lock().insert(i);
    ctrl.push_log(format!("将跳过步骤 {step_index}"));
    task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
    Ok(())
}

#[tauri::command]
fn task_rollback_step_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    step_index: u32,
) -> Result<(), String> {
    let ctrl = state
        .task_registry
        .get(&task_id)
        .ok_or_else(|| "任务不存在或已结束".to_string())?;
    ctrl.require_paused()?;
    if step_index == 0 {
        return Err("无效的步骤序号".into());
    }
    let i = (step_index as usize) - 1;
    {
        let recs = ctrl.step_records.lock();
        let rec = recs.get(i).ok_or_else(|| "步骤不存在".to_string())?;
        if rec.status != "ok" && rec.status != "fail" && rec.status != "skipped" {
            return Err("只能回退已执行或已跳过的步骤".into());
        }
    }
    *ctrl.seek_to.lock() = Some(i);
    ctrl.push_log(format!("请求回退到步骤 {step_index}"));
    task_progress::emit_task_control_feedback(&app, &task_id, &ctrl);
    Ok(())
}

#[tauri::command]
fn get_feishu_relay_config_cmd() -> feishu_relay::FeishuRelayConfig {
    feishu_relay::config()
}

#[tauri::command]
async fn export_recipe_to_task_file_cmd(
    id: String,
    path: Option<String>,
) -> Result<String, String> {
    let recipe = recipe::get_recipe(&id)?
        .ok_or_else(|| "快捷指令不存在".to_string())?;
    export_content_to_task_file(&recipe.name, &recipe.content, path).await
}

/// 将技能名称+步骤内容另存为 .webclaw（不依赖本地 recipes.json）
#[tauri::command]
async fn export_skill_to_task_file_cmd(
    name: String,
    content: String,
    path: Option<String>,
) -> Result<String, String> {
    export_content_to_task_file(&name, &content, path).await
}

async fn export_content_to_task_file(
    name: &str,
    content: &str,
    path: Option<String>,
) -> Result<String, String> {
    let content = content.trim();
    if content.is_empty() {
        return Err("技能内容为空，无法导出".into());
    }
    let name = name.trim();
    let safe: String = name
        .chars()
        .map(|c| {
            if r#"<>:"/\|?*"#.contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string();
    let default_name = format!(
        "{}.webclaw",
        if safe.is_empty() { "技能" } else { safe.as_str() }
    );
    let save_path = if let Some(p) = path {
        std::path::PathBuf::from(p)
    } else {
        tauri::async_runtime::spawn_blocking(move || {
            rfd::FileDialog::new()
                .set_title("另存为 .webclaw 任务文件")
                .set_file_name(&default_name)
                .add_filter("Webclaw 任务", &["webclaw"])
                .save_file()
        })
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "已取消导出".to_string())?
    };
    let mut save_path = save_path;
    if save_path.extension().is_none() {
        save_path.set_extension("webclaw");
    }
    let body = task_file::format_task_file(name, content);
    std::fs::write(&save_path, body).map_err(|e| format!("写入任务文件失败: {e}"))?;
    Ok(save_path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn import_task_file_to_recipe_cmd(path: Option<String>) -> Result<recipe::Recipe, String> {
    let open_path = if let Some(p) = path {
        std::path::PathBuf::from(p)
    } else {
        tauri::async_runtime::spawn_blocking(|| {
            rfd::FileDialog::new()
                .set_title("从 .webclaw 任务文件导入")
                .add_filter("Webclaw 任务", &["webclaw"])
                .pick_file()
        })
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "已取消导入".to_string())?
    };
    let spec = task_file::load_task_file(&open_path)?;
    if spec.mode == task_file::TaskMode::Edit {
        return Err("该任务文件为 edit 模式，请使用双击编辑或改为 run 模式后导入".into());
    }
    recipe::save_recipe(None, spec.name, spec.content)
}

#[tauri::command]
async fn run_task_file_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    let spec = task_file::load_task_file(std::path::Path::new(&path))?;
    launch::run_task_from_spec(&app, &state, spec).await
}

#[tauri::command]
fn register_webclaw_file_association_cmd() -> Result<String, String> {
    file_assoc::register_force()
}

#[tauri::command]
fn get_file_association_status_cmd() -> Result<serde_json::Value, String> {
    Ok(file_assoc::association_status())
}

#[tauri::command]
fn task_result_open_main_cmd(app: tauri::AppHandle) -> Result<(), String> {
    launch::show_main_ui(&app);
    Ok(())
}

#[tauri::command]
fn task_window_close_cmd(window: tauri::WebviewWindow, app: tauri::AppHandle) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())?;
    crate::tray::refresh_tooltip(&app);
    Ok(())
}

#[tauri::command]
fn task_result_quit_cmd(app: tauri::AppHandle, state: State<'_, AppState>) {
    state.task_registry.stop_all();
    tray::quit_app(&app);
}

#[tauri::command]
async fn check_feishu_relay_health_cmd() -> Result<serde_json::Value, String> {
    let cfg = feishu_relay::config();
    let url = feishu_relay::health_url(&cfg);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|_| "无法连接飞书 relay".to_string())?;
    if !res.status().is_success() {
        return Err(format!("飞书 relay 响应异常: HTTP {}", res.status()));
    }
    let body: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("飞书 relay 响应解析失败: {e}"))?;
    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err("飞书 relay 未就绪".into());
    }
    Ok(body)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let task_queue = launch::new_task_queue();
    let launch_meta = LaunchMeta::new();

    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        let queue_for_plugin = task_queue.clone();
        let meta_for_plugin = launch_meta.clone();
        builder = builder.plugin(tauri_plugin_single_instance::init(
            move |app, argv, _cwd| {
                let paths = launch::paths_from_args(argv);
                if paths.is_empty() {
                    launch::show_main_ui(&app);
                    return;
                }
                launch::enqueue_paths(&queue_for_plugin, paths);
                launch::enter_task_run_ui(&app);
                launch::schedule_drain(app.clone(), queue_for_plugin.clone(), meta_for_plugin.clone());
            },
        ));
    }

    builder = builder.plugin(tauri_plugin_shell::init());

    builder
        .manage(AppState {
            logs: Arc::new(Mutex::new(vec![
                "就绪。内置指令可编辑；数据处理请用「脚本合并考勤数据」等命名脚本。".into(),
                format!("快捷指令存储: {}", paths::recipes_file().display()),
                "双击 .webclaw 任务文件可后台执行（进度浮窗，亦支持 --run 路径）。".into(),
            ])),
            chrome_child: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            ai_stream_cancels: Arc::new(Mutex::new(HashMap::new())),
            task_registry: Arc::new(TaskRegistry::new()),
            active_browser: Arc::new(Mutex::new(None)),
            last_main_target: Arc::new(Mutex::new(None)),
            last_browser_snapshot: Arc::new(Mutex::new(None)),
            captcha_waiters: captcha::new_waiters(),
        })
        .manage(task_queue.clone())
        .manage(launch_meta.clone())
        .manage(scheduler::SchedulerState::new())
        .manage(crate::rhai_studio::StudioState::default())
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup({
            let queue = task_queue.clone();
            let meta = launch_meta.clone();
            move |app| {
                paths::ensure_user_scope_initialized();
                tray::setup(app)?;
                let _ = templates::ensure_template_files();
                launch::collect_startup_paths(&queue);
                let has_tasks = !queue.lock().expect("task queue").is_empty();
                if has_tasks {
                    launch::prepare_startup_for_task_files(app.handle(), &meta);
                    let state = app.state::<AppState>();
                    let queued: Vec<String> = queue
                        .lock()
                        .expect("task queue")
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect();
                    state.push_log(format!("任务文件启动: {}", queued.join(" | ")));
                    emit_logs(&app.handle(), &state);
                }
                match file_assoc::register_if_needed() {
                    Ok(msg) => {
                        let state = app.state::<AppState>();
                        state.push_log(msg);
                        emit_logs(&app.handle(), &state);
                    }
                    Err(e) => {
                        let state = app.state::<AppState>();
                        state.push_log(format!("文件关联未注册: {e}"));
                        emit_logs(&app.handle(), &state);
                    }
                }
                if has_tasks {
                    launch::schedule_drain(app.handle().clone(), queue.clone(), meta.clone());
                }
                tray::refresh_tooltip(app.handle());
                let relay_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    crate::run_relay_desktop::ensure_connected(relay_app);
                });
                scheduler::start_scheduler(app.handle().clone());
                let floater_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    let app2 = floater_app.clone();
                    let _ = floater_app.run_on_main_thread(move || {
                        schedule_floater::apply_from_settings(&app2);
                    });
                });
                // 首次启动后台拉取 RapidOCR ONNX 模型到用户机器缓存
                let ocr_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                    let app2 = ocr_app.clone();
                    rhai_runtime::local_ocr::spawn_ensure_ocr_models(move |progress| {
                        let _ = app2.emit("ocr-cache-progress", &progress);
                        if progress.ready {
                            let state = app2.state::<AppState>();
                            state.push_log("RapidOCR 本地模型已就绪");
                            emit_logs(&app2, &state);
                        } else if let Some(err) = progress.error.as_ref() {
                            let state = app2.state::<AppState>();
                            state.push_log(format!("RapidOCR 模型下载失败: {err}"));
                            emit_logs(&app2, &state);
                        }
                    });
                });
                Ok(())
            }
        })
        .invoke_handler(tauri::generate_handler![
            launch_debug_chrome,
            zhj_auth_login,
            run_visual_query,
            list_recipes_cmd,
            get_recipe_cmd,
            save_recipe_cmd,
            delete_recipe_cmd,
            list_scheduled_jobs_cmd,
            list_expired_scheduled_jobs_cmd,
            resolve_expired_scheduled_jobs_cmd,
            get_scheduled_job_cmd,
            save_scheduled_job_cmd,
            delete_scheduled_job_cmd,
            toggle_scheduled_job_cmd,
            run_scheduled_job_now_cmd,
            preview_schedule_next_run_cmd,
            set_schedule_floater_enabled_cmd,
            show_schedule_floater_cmd,
            hide_schedule_floater_cmd,
            set_schedule_floater_collapsed_cmd,
            set_schedule_floater_popup_space_cmd,
            quit_app_cmd,
            show_main_ui_cmd,
            sync_skills_cache_cmd,
            list_web_systems_cmd,
            sync_remote_web_systems_cmd,
            set_current_user_cmd,
            get_current_user_cmd,
            load_chat_history_cmd,
            save_chat_session_cmd,
            save_chat_history_cmd,
            delete_chat_sessions_cmd,
            read_chat_run_log_cmd,
            recover_chat_sessions_from_runs_cmd,
            get_web_system_cmd,
            save_web_system_cmd,
            delete_web_system_cmd,
            resolve_open_web_target_cmd,
            run_recipe_cmd,
            run_recipe_content_cmd,
            check_upload_attach_preflight_cmd,
            export_recipe_to_task_file_cmd,
            export_skill_to_task_file_cmd,
            import_task_file_to_recipe_cmd,
            run_task_file_cmd,
            register_webclaw_file_association_cmd,
            get_file_association_status_cmd,
            task_result_open_main_cmd,
            task_window_close_cmd,
            task_result_quit_cmd,
            task_pause_cmd,
            task_resume_cmd,
            task_stop_cmd,
            parse_recipe_params_cmd,
            recipe_content_needs_browser_cmd,
            list_skill_invocations_cmd,
            bind_skill_invocation_params_cmd,
            list_public_params_cmd,
            save_public_params_cmd,
            merge_recipe_params_cmd,
            task_set_recipe_params_cmd,
            task_insert_steps_cmd,
            task_skip_step_cmd,
            task_rollback_step_cmd,
            captcha_submit_cmd,
            captcha_cancel_cmd,
            ai_assist_recipe,
            clear_execution_cancel_cmd,
            get_browser_tab_snapshot_cmd,
            get_settings_cmd,
            save_settings_cmd,
            dispatch_remote_job_cmd,
            remote_execute_cmd,
            create_pair_code_cmd,
            finish_pending_remote_execute_cmd,
            run_relay_status_cmd,
            run_relay_connect_cmd,
            run_relay_human_reply_cmd,
            run_relay_cancel_job_cmd,
            export_accounts_pack_cmd,
            export_host_pair_cmd,
            export_companion_skill_cmd,
            export_remote_secret_cmd,
            reset_remote_secret_upload_cmd,
            detect_chrome_executable_cmd,
            pick_chrome_executable_cmd,
            get_feishu_relay_config_cmd,
            check_feishu_relay_health_cmd,
            ai_summarize_chat_cmd,
            list_commands_cmd,
            get_command_cmd,
            save_command_cmd,
            delete_command_cmd,
            reset_command_script_cmd,
            list_scripts_cmd,
            get_script_cmd,
            save_script_cmd,
            delete_script_cmd,
            reset_script_cmd,
            resolve_script_doc_cmd,
            crate::rhai_studio::rhai_studio_health,
            crate::rhai_studio::rhai_studio_list_files,
            crate::rhai_studio::rhai_studio_delete_file,
            crate::rhai_studio::rhai_studio_preview_file,
            crate::rhai_studio::rhai_studio_pick_and_add_files,
            crate::rhai_studio::rhai_studio_open_workspace,
            crate::rhai_studio::rhai_studio_run_script,
            crate::rhai_studio::rhai_studio_validate_script,
            crate::rhai_studio::rhai_studio_reset_session,
            crate::rhai_studio::rhai_studio_chat_send,
            crate::rhai_studio::rhai_studio_chat_abort,
            list_assert_scripts_cmd,
            get_assert_script_cmd,
            save_assert_script_cmd,
            delete_assert_script_cmd,
            reset_assert_script_cmd,
            list_templates_cmd,
            get_template_cmd,
            save_template_cmd,
            delete_template_cmd,
            reset_builtin_template_cmd,
            import_template_file_cmd,
            replace_template_file_cmd,
            open_templates_dir_cmd,
            sync_remote_scripts_cmd,
            sync_remote_templates_cmd,
            pick_template_file_path_cmd,
            local_file_size_cmd,
            stage_template_file_cmd,
            zhj_upload_file_cmd,
            zhj_download_file_cmd,
            ai_assist_command_script,
            ai_stream_abort,
            ai_stream_recipe_cmd,
            ai_stream_command_script_cmd,
            get_app_paths,
            local_cache_status_cmd,
            local_cache_open_system_dir_cmd,
            local_cache_ensure_ocr_cmd,
            local_cache_clear_ocr_cmd,
            local_cache_clear_runtime_cmd,
            local_cache_clear_other_users_cmd,
            get_app_version_cmd,
            check_for_update_cmd,
            install_update_cmd,
            check_update_cmd,
            apply_update_cmd,
            open_run_artifacts_dir_cmd,
            open_artifact_file_cmd,
            open_path_externally_cmd,
            open_html_in_browser_cmd,
            open_html_document_in_browser_cmd,
            read_text_file_cmd,
            take_pending_markdown_path_cmd,
            close_markdown_viewer_cmd,
            upload_run_attachments_cmd,
            delete_run_artifacts_cmd,
            get_logs,
            clear_logs,
            get_active_browser_cmd,
            show_browser_cmd,
            restart_debug_browser_cmd,
            page_probe::probe_page_cmd,
            page_probe::probe_use_active_tab_cmd,
            page_probe::set_probe_overlay_cmd,
            page_probe::clear_probe_overlay_cmd,
            page_probe::refresh_probe_overlay_cmd,
            page_probe::probe_click_text_cmd,
            page_probe::set_click_text_probe_overlay_cmd,
            page_probe::clear_click_text_probe_overlay_cmd,
            stop_execution_cmd,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run({
            let queue = task_queue;
            let meta = launch_meta;
            move |app, event| {
                launch::on_run_event(app, event, &queue, &meta);
            }
        });
}
