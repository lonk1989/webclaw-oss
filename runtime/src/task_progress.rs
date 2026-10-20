use parking_lot::Mutex;
use std::collections::HashMap;
use crate::run_artifacts::RunArtifact;
use crate::task_run;
use crate::task_file::TaskUi;
use serde::Serialize;
use crate::ui_host::{AppHandle, Emitter, LogicalSize, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

const RUNNING_SIZE: (f64, f64) = (420.0, 620.0);
const RESULT_SIZE: (f64, f64) = (440.0, 560.0);

#[derive(Clone, Serialize)]
pub struct TaskStepRecord {
    pub index: u32,
    pub label: String,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Serialize)]
pub struct TaskProgressPayload {
    pub task_id: String,
    pub task_name: String,
    pub phase: String,
    pub current: u32,
    pub total: u32,
    pub label: String,
    pub indeterminate: bool,
    pub paused: bool,
    #[serde(default)]
    pub stopping: bool,
    pub queued: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<TaskStepRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<RunArtifact>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipe_params: Vec<crate::recipe::RecipeParamKv>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgressPayload {
    pub task_id: String,
    /// waiting | fetching | done | error | hidden
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub indeterminate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u32>,
    pub visible: bool,
}

pub struct DownloadProgressSink<'a, R: Runtime> {
    app: &'a AppHandle<R>,
    task_id: &'a str,
}

impl<'a, R: Runtime> DownloadProgressSink<'a, R> {
    pub fn new(app: &'a AppHandle<R>, task_id: &'a str) -> Self {
        Self { app, task_id }
    }

    pub fn waiting(&self, message: &str, percent: Option<u32>) {
        self.emit(DownloadProgressPayload {
            task_id: self.task_id.to_string(),
            phase: "waiting".into(),
            message: message.to_string(),
            filename: None,
            indeterminate: percent.is_none(),
            percent,
            visible: true,
        });
    }

    pub fn fetching(&self, filename: &str) {
        self.emit(DownloadProgressPayload {
            task_id: self.task_id.to_string(),
            phase: "fetching".into(),
            message: format!("正在下载 {filename}"),
            filename: Some(filename.to_string()),
            indeterminate: true,
            percent: None,
            visible: true,
        });
    }

    pub fn done(&self, filename: &str) {
        self.emit(DownloadProgressPayload {
            task_id: self.task_id.to_string(),
            phase: "done".into(),
            message: format!("已下载 {filename}"),
            filename: Some(filename.to_string()),
            indeterminate: false,
            percent: Some(100),
            visible: true,
        });
    }

    pub fn error(&self, message: &str) {
        self.emit(DownloadProgressPayload {
            task_id: self.task_id.to_string(),
            phase: "error".into(),
            message: message.to_string(),
            filename: None,
            indeterminate: false,
            percent: None,
            visible: true,
        });
    }

    pub fn hide(&self) {
        self.emit(DownloadProgressPayload {
            task_id: self.task_id.to_string(),
            phase: "hidden".into(),
            message: String::new(),
            filename: None,
            indeterminate: false,
            percent: None,
            visible: false,
        });
    }

    fn emit(&self, payload: DownloadProgressPayload) {
        emit_download_progress(self.app, self.task_id, payload);
    }
}

static DOWNLOAD_PROGRESS_LAST: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
    std::sync::OnceLock::new();

fn progress_dedup_key(payload: &DownloadProgressPayload) -> String {
    format!(
        "{}|{}|{}|{:?}|{:?}",
        payload.phase,
        payload.message,
        payload.filename.as_deref().unwrap_or(""),
        payload.percent,
        payload.visible
    )
}

pub fn emit_download_progress<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    payload: DownloadProgressPayload,
) {
    let map = DOWNLOAD_PROGRESS_LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let key = progress_dedup_key(&payload);
    {
        let mut guard = map.lock();
        if guard.get(task_id) == Some(&key) {
            return;
        }
        guard.insert(task_id.to_string(), key);
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.emit("download-progress-update", &payload);
    }
    let label = task_run::window_label(task_id);
    if task_id != "main" {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.emit("download-progress-update", &payload);
        }
    }
}

pub fn clear_download_progress_dedup(task_id: &str) {
    if let Some(map) = DOWNLOAD_PROGRESS_LAST.get() {
        map.lock().remove(task_id);
    }
}

pub fn is_background_ui(ui: TaskUi) -> bool {
    matches!(
        ui,
        TaskUi::Background | TaskUi::Minimal | TaskUi::Silent
    )
}

pub fn emit_task_progress<R: Runtime>(app: &AppHandle<R>, task_id: &str, payload: TaskProgressPayload) {
    let label = task_run::window_label(task_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.emit("task-progress-update", payload);
    }
}

/// 用户点击暂停/继续/终止后，立即把控制状态推送到任务窗口（不等待当前步骤结束）。
pub fn emit_task_control_feedback<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    control: &task_run::TaskControl,
) {
    let stopping = control.cancel.load(std::sync::atomic::Ordering::SeqCst);
    let paused = control.pause.load(std::sync::atomic::Ordering::SeqCst) && !stopping;
    // 先分别取快照，避免 last_progress → step_records 嵌套锁与执行器死锁（点停止时易卡死/崩溃）
    let existing = control.last_progress.lock().clone();
    let steps = control.step_records.lock().clone();
    let (run_dir, artifacts) = control
        .run_session
        .lock()
        .as_ref()
        .map(|s| (Some(s.run_dir_display()), Some(s.list_artifacts())))
        .unwrap_or((None, None));
    let recipe_params = control.recipe_params_vec();
    let mut payload = existing.unwrap_or_else(|| TaskProgressPayload {
        task_id: task_id.to_string(),
        task_name: "任务".into(),
        phase: "step".into(),
        current: 0,
        total: 0,
        label: String::new(),
        indeterminate: true,
        paused: false,
        stopping: false,
        queued: false,
        steps: Some(steps.clone()),
        summary: None,
        success: None,
        run_dir,
        artifacts,
        recipe_params: recipe_params.clone(),
    });
    payload.paused = paused;
    payload.stopping = stopping;
    payload.steps = Some(steps);
    payload.recipe_params = recipe_params;
    if stopping {
        payload.phase = "stopping".into();
        payload.label = "正在终止…".into();
    } else if paused {
        payload.phase = "paused".into();
        if payload.label.is_empty() || payload.label == "正在准备任务…" {
            payload.label = "已暂停".into();
        }
    }
    *control.last_progress.lock() = Some(payload.clone());
    emit_task_progress(app, task_id, payload);
}

fn resize_task_window<R: Runtime>(app: &AppHandle<R>, task_id: &str, width: f64, height: f64) {
    let label = task_run::window_label(task_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.set_size(LogicalSize::new(width, height));
    }
}

pub fn create_task_window<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    offset: (i32, i32),
) -> Result<(), String> {
    let label = task_run::window_label(task_id);
    if app.get_webview_window(&label).is_some() {
        return Ok(());
    }
    let url = format!("task-progress.html?id={task_id}");
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
        .title("webclaw 任务")
        .inner_size(RUNNING_SIZE.0, RUNNING_SIZE.1)
        .resizable(false)
        .maximizable(false)
        .minimizable(true)
        .always_on_top(true)
        .decorations(false)
        .transparent(true)
        .shadow(true);
    #[cfg(feature = "desktop")]
    if let Ok(icon) = crate::tray::load_hires_icon() {
        builder = builder.icon(icon).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        builder = builder.drag_and_drop(false);
    }
    if offset != (0, 0) {
        builder = builder.position(
            120.0 + f64::from(offset.0),
            80.0 + f64::from(offset.1),
        );
    } else {
        builder = builder.center();
    }
    builder
        .build()
        .map_err(|e| format!("无法创建任务窗口: {e}"))?;
    Ok(())
}

pub fn show_task_window<R: Runtime>(app: &AppHandle<R>, task_id: &str, task_name: &str) {
    let label = task_run::window_label(task_id);
    resize_task_window(app, task_id, RUNNING_SIZE.0, RUNNING_SIZE.1);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
    emit_task_progress(
        app,
        task_id,
        TaskProgressPayload {
            task_id: task_id.to_string(),
            task_name: task_name.to_string(),
            phase: "prepare".into(),
            current: 0,
            total: 0,
            label: "正在准备任务…".into(),
            indeterminate: true,
            paused: false,
            stopping: false,
            queued: false,
            steps: None,
            summary: None,
            success: None,
            run_dir: None,
            artifacts: None,
            recipe_params: Vec::new(),
        },
    );
}

pub fn show_result_page<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    task_name: &str,
    success: bool,
    summary: &str,
    steps: Vec<TaskStepRecord>,
    run_dir: Option<String>,
    artifacts: Option<Vec<RunArtifact>>,
    recipe_params: Vec<crate::recipe::RecipeParamKv>,
) {
    resize_task_window(app, task_id, RESULT_SIZE.0, RESULT_SIZE.1);
    let label = task_run::window_label(task_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.unminimize();
    }
    emit_task_progress(
        app,
        task_id,
        TaskProgressPayload {
            task_id: task_id.to_string(),
            task_name: task_name.to_string(),
            phase: "result".into(),
            current: 0,
            total: 0,
            label: summary.to_string(),
            indeterminate: false,
            paused: false,
            stopping: false,
            queued: false,
            steps: Some(steps),
            summary: Some(summary.to_string()),
            success: Some(success),
            run_dir,
            artifacts,
            recipe_params,
        },
    );
}

pub fn close_task_window<R: Runtime>(app: &AppHandle<R>, task_id: &str) {
    let label = task_run::window_label(task_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
    }
}

pub fn hide_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}
