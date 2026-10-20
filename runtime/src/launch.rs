use crate::executor;
use crate::task_file::{TaskMode, TaskSpec};
use crate::task_progress;
use crate::task_run;
use crate::tray;
use crate::{task_file, AppState, LaunchMeta};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

pub type TaskQueue = Arc<Mutex<Vec<PathBuf>>>;

pub fn new_task_queue() -> TaskQueue {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn enqueue_paths(queue: &TaskQueue, paths: impl IntoIterator<Item = PathBuf>) {
    let mut q = queue.lock().expect("task queue poisoned");
    for p in paths {
        if is_webclaw_path(&p) {
            q.push(p);
        }
    }
}

fn is_webclaw_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("webclaw"))
        .unwrap_or(false)
}

pub fn paths_from_args(args: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    let args: Vec<String> = args.into_iter().collect();
    let mut paths = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if i == 0 {
            i += 1;
            continue;
        }
        if arg == "--run" || arg == "--run-and-exit" {
            if let Some(next) = args.get(i + 1) {
                paths.push(PathBuf::from(next));
                i += 2;
                continue;
            }
        }
        if arg.starts_with("--run=") {
            let p = arg.trim_start_matches("--run=");
            if !p.is_empty() {
                paths.push(PathBuf::from(p));
            }
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        if let Some(p) = path_from_os_arg(arg) {
            if is_webclaw_path(&p) {
                paths.push(p);
            }
        }
        i += 1;
    }
    paths
}

fn path_from_os_arg(arg: &str) -> Option<PathBuf> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    if looks_like_windows_path(arg) {
        return Some(PathBuf::from(arg));
    }
    if let Ok(url) = url::Url::parse(arg) {
        if url.scheme() == "file" {
            if let Ok(path) = url.to_file_path() {
                return Some(path);
            }
        }
    }
    Some(PathBuf::from(arg))
}

fn looks_like_windows_path(arg: &str) -> bool {
    let bytes = arg.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    arg.starts_with(r"\\")
}

pub fn collect_startup_paths(queue: &TaskQueue) {
    let paths = paths_from_args(std::env::args());
    enqueue_paths(queue, paths);
}

pub fn focus_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

pub fn show_main_ui<R: Runtime>(app: &AppHandle<R>) {
    crate::tray::apply_main_window_icon(app);
    task_progress::show_main_window(app);
    focus_main_window(app);
}

pub fn prepare_startup_for_task_files<R: Runtime>(app: &AppHandle<R>, meta: &LaunchMeta) {
    meta.started_with_task_files.store(true, Ordering::SeqCst);
    enter_task_run_ui(app);
}

pub fn enter_task_run_ui<R: Runtime>(app: &AppHandle<R>) {
    task_progress::hide_main_window(app);
}

#[derive(Clone, serde::Serialize)]
pub struct TaskFileEditPayload {
    pub path: String,
    pub name: String,
    pub content: String,
}

#[derive(Clone, serde::Serialize)]
pub struct TaskFileRunPayload {
    pub task_id: String,
    pub path: String,
    pub name: String,
    pub background: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskHistoryAppendPayload {
    pub task_id: String,
    /// 为本次后台任务新建的对话 ID（前端据此创建会话，勿写入当前激活对话）
    pub chat_session_id: String,
    pub name: String,
    pub path: Option<String>,
    pub log_text: String,
    pub success: bool,
    pub summary: String,
    pub run_dir: Option<String>,
    #[serde(default)]
    pub recipe_params: Vec<crate::recipe::RecipeParamKv>,
}

pub async fn run_task_from_spec<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    spec: TaskSpec,
) -> Result<String, String> {
    if spec.mode == TaskMode::Edit {
        return run_edit_task(app, spec).await;
    }
    let background = task_progress::is_background_ui(spec.ui);
    if background {
        let task_id = Uuid::new_v4().to_string();
        let spawned_id = task_id.clone();
        let registry = state.task_registry.clone();
        let control = registry.register(&task_id);
        tray::refresh_tooltip(app);
        let offset = registry.next_window_offset();
        task_progress::create_task_window(app, &task_id, offset)?;
        task_progress::show_task_window(app, &task_id, &spec.name);
        let spec_clone = spec;
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = app2.state::<AppState>();
            if let Err(e) = run_background_task(&app2, &state, &task_id, control, spec_clone).await {
                state.push_log(format!("后台任务失败: {e}"));
                crate::emit_logs_public(&app2, &state);
            }
        });
        Ok(format!("spawned:{spawned_id}"))
    } else {
        run_foreground_task(app, state, spec).await
    }
}

async fn run_edit_task<R: Runtime>(app: &AppHandle<R>, spec: TaskSpec) -> Result<String, String> {
    if let Some(path) = &spec.source_path {
        let _ = app.emit(
            "task-file-edit",
            TaskFileEditPayload {
                path: path.to_string_lossy().into_owned(),
                name: spec.name.clone(),
                content: spec.content.clone(),
            },
        );
    }
    focus_main_window(app);
    Ok("edit".into())
}

async fn run_foreground_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    spec: TaskSpec,
) -> Result<String, String> {
    focus_main_window(app);
    let path_str = spec
        .source_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(ref path) = path_str {
        let _ = app.emit(
            "task-file-run-started",
            TaskFileRunPayload {
                task_id: String::new(),
                path: path.clone(),
                name: spec.name.clone(),
                background: false,
            },
        );
    }

    // 浏览器由执行器按步骤决定：仅「打开」「登录」拉起；纯脚本/参数不启动
    let result = executor::execute_recipe_content(
        app,
        state,
        Some(&spec.name),
        &spec.content,
        None,
    )
    .await;

    if let Some(path) = path_str {
        let _ = app.emit(
            "task-file-run-finished",
            serde_json::json!({
                "path": path,
                "name": spec.name,
                "background": false,
                "result": result.as_ref().ok().cloned(),
                "error": result.as_ref().err().map(|e| e.to_string()),
            }),
        );
    }

    if let Err(ref e) = result {
        let msg = format!("「{}」\n{e}", spec.name);
        let _ = tauri::async_runtime::spawn_blocking(move || {
            rfd::MessageDialog::new()
                .set_title("webclaw 任务失败")
                .set_description(msg)
                .set_level(rfd::MessageLevel::Error)
                .show();
        })
        .await;
    }

    result
}

async fn run_background_task<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    task_id: &str,
    control: task_run::TaskControl,
    spec: TaskSpec,
) -> Result<(), String> {
    let path_str = spec
        .source_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());

    // 每次后台任务绑定独立对话，避免写入主窗当前激活会话
    let chat_session_id = Uuid::new_v4().to_string();

    enter_task_run_ui(app);

    if let Some(ref path) = path_str {
        let _ = app.emit(
            "task-file-run-started",
            TaskFileRunPayload {
                task_id: task_id.to_string(),
                path: path.clone(),
                name: spec.name.clone(),
                background: true,
            },
        );
    }

    let result = executor::execute_recipe_for_task(
        app,
        state,
        task_id,
        &control,
        Some(&spec.name),
        &spec.content,
        Some(&chat_session_id),
    )
    .await;

    let (success, summary) = match &result {
        Ok(r) => executor::parse_exec_result(r),
        Err(e) => (false, e.clone()),
    };

    let steps = control.step_records.lock().clone();
    let (run_dir, artifacts) = control
        .run_session
        .lock()
        .as_ref()
        .map(|s| (Some(s.run_dir_display()), Some(s.list_artifacts())))
        .unwrap_or((None, None));
    task_progress::show_result_page(
        app,
        task_id,
        &spec.name,
        success,
        &summary,
        steps,
        run_dir.clone(),
        artifacts,
        control.recipe_params_vec(),
    );

    let log_text = executor::format_task_logs_for_history(
        &control,
        &spec.name,
        path_str.as_deref(),
    );
    let history = TaskHistoryAppendPayload {
        task_id: task_id.to_string(),
        chat_session_id,
        name: spec.name.clone(),
        path: path_str.clone(),
        log_text,
        success,
        summary: summary.clone(),
        run_dir,
        recipe_params: control.recipe_params_vec(),
    };
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.emit("task-history-append", &history);
    } else {
        let _ = app.emit("task-history-append", &history);
    }

    if let Some(path) = path_str {
        let _ = app.emit(
            "task-file-run-finished",
            serde_json::json!({
                "task_id": task_id,
                "path": path,
                "name": spec.name,
                "background": true,
                "result": result.as_ref().ok().cloned(),
                "error": result.as_ref().err().map(|e| e.to_string()),
            }),
        );
    }

    state.task_registry.remove(task_id);
    tray::refresh_tooltip(app);
    crate::scheduler::notify_execution_idle(app);
    Ok(())
}

pub fn spawn_task_from_path<R: Runtime>(app: &AppHandle<R>, path: PathBuf) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app2.state::<AppState>();
        match task_file::load_task_file(&path) {
            Ok(spec) => {
                state.push_log(format!(
                    "任务文件: {} | {}",
                    path.display(),
                    spec.name
                ));
                crate::emit_logs_public(&app2, &state);
                if let Err(e) = run_task_from_spec(&app2, &state, spec).await {
                    state.push_log(format!("任务启动失败 ({}): {e}", path.display()));
                    crate::emit_logs_public(&app2, &state);
                }
            }
            Err(e) => {
                state.push_log(format!("任务文件加载失败 ({}): {e}", path.display()));
                crate::emit_logs_public(&app2, &state);
                let task_id = Uuid::new_v4().to_string();
                let offset = state.task_registry.next_window_offset();
                let _ = task_progress::create_task_window(&app2, &task_id, offset);
                task_progress::show_result_page(
                    &app2,
                    &task_id,
                    "任务文件",
                    false,
                    &e,
                    vec![],
                    None,
                    None,
                    vec![],
                );
            }
        }
    });
}

pub async fn drain_and_run_queue<R: Runtime>(
    app: AppHandle<R>,
    queue: &TaskQueue,
    _meta: &LaunchMeta,
) {
    let batch: Vec<PathBuf> = {
        let mut q = queue.lock().expect("task queue poisoned");
        if q.is_empty() {
            return;
        }
        q.drain(..).collect()
    };

    for path in batch {
        spawn_task_from_path(&app, path);
    }
}

pub fn schedule_drain<R: Runtime>(app: AppHandle<R>, queue: TaskQueue, meta: LaunchMeta) {
    tauri::async_runtime::spawn(async move {
        drain_and_run_queue(app, &queue, &meta).await;
    });
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "ios")),
    allow(unused_variables)
)]
pub fn on_run_event<R: Runtime>(
    app: &AppHandle<R>,
    event: tauri::RunEvent,
    queue: &TaskQueue,
    meta: &LaunchMeta,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    match event {
        tauri::RunEvent::Opened { urls } => {
            handle_opened_urls(app, queue, meta, urls);
        }
        // 点窗口关闭会 hide 到托盘；再点 Dock 图标走 Reopen，需重新显示主界面
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => {
            show_main_ui(app);
        }
        _ => {}
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn handle_opened_urls<R: Runtime>(
    app: &AppHandle<R>,
    queue: &TaskQueue,
    meta: &LaunchMeta,
    urls: Vec<url::Url>,
) {
    let paths: Vec<PathBuf> = urls
        .into_iter()
        .filter_map(|u| u.to_file_path().ok())
        .filter(|p| is_webclaw_path(p))
        .collect();
    if paths.is_empty() {
        return;
    }
    enqueue_paths(queue, paths);
    // 与 `--run` / 双击启动一致：切入后台任务 UI，避免仅入队不切界面
    if !meta.started_with_task_files.load(Ordering::SeqCst) {
        prepare_startup_for_task_files(app, meta);
    } else {
        enter_task_run_ui(app);
    }
    schedule_drain(app.clone(), queue.clone(), meta.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_windows_drive_path_from_args() {
        let args = vec![
            r"D:\tools\browser-native\target\release\webclaw.exe".into(),
            r"D:\tasks\月报导出.webclaw".into(),
        ];
        let paths = paths_from_args(args);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].to_string_lossy().ends_with("月报导出.webclaw"));
    }

    #[test]
    fn parse_run_flag_path() {
        let args = vec![
            "webclaw.exe".into(),
            "--run".into(),
            r"C:\Users\me\job.webclaw".into(),
        ];
        let paths = paths_from_args(args);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn windows_path_not_treated_as_url() {
        let p = path_from_os_arg(r"D:\foo\bar.webclaw").unwrap();
        assert_eq!(p, PathBuf::from(r"D:\foo\bar.webclaw"));
    }
}
