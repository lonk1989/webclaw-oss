use crate::executor;
use crate::launch;
use crate::recipe::{self, ordered_vec_to_params};
use crate::task_progress;
use crate::tray;
use crate::AppState;
use chrono::Utc;
use parking_lot::Mutex;
use std::collections::VecDeque;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

use super::store::{self, ScheduledJob};

pub const EXECUTION_IDLE_EVENT: &str = "execution-idle";
pub const SCHEDULED_JOBS_CHANGED_EVENT: &str = "scheduled-jobs-changed";
pub const SCHEDULED_JOBS_EXPIRED_EVENT: &str = "scheduled-jobs-expired";

const TICK_SECS: u64 = 60;

pub struct SchedulerState {
    run_queue: Mutex<VecDeque<String>>,
    draining: Mutex<bool>,
}

impl SchedulerState {
    pub fn new() -> Self {
        Self {
            run_queue: Mutex::new(VecDeque::new()),
            draining: Mutex::new(false),
        }
    }
}

pub fn notify_execution_idle<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(EXECUTION_IDLE_EVENT, ());
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        drain_run_queue(app2).await;
    });
}

pub fn start_scheduler<R: Runtime>(app: AppHandle<R>) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        startup_catchup(app2.clone()).await;
        loop {
            tick(app2.clone()).await;
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)).await;
        }
    });
}

async fn startup_catchup<R: Runtime>(app: AppHandle<R>) {
    let now = Utc::now();
    let expired = match store::scan_and_mark_expired_on_startup(now) {
        Ok(j) => j,
        Err(e) => {
            log_scheduler(&app, format!("过期定时任务扫描失败: {e}"));
            return;
        }
    };
    if expired.is_empty() {
        return;
    }
    log_scheduler(
        &app,
        format!(
            "发现 {} 个过期定时任务，等待用户确认是否补跑",
            expired.len()
        ),
    );
    let _ = app.emit(SCHEDULED_JOBS_CHANGED_EVENT, ());
    let _ = app.emit(SCHEDULED_JOBS_EXPIRED_EVENT, &expired);
}

pub async fn resolve_expired_jobs_and_enqueue<R: Runtime>(
    app: AppHandle<R>,
    run_ids: Vec<String>,
) -> Result<(), String> {
    let now = Utc::now();
    let enqueued = store::resolve_expired_jobs(&run_ids, now)?;
    if enqueued.is_empty() {
        let _ = app.emit(SCHEDULED_JOBS_CHANGED_EVENT, ());
        return Ok(());
    }
    let scheduler = app.state::<SchedulerState>();
    {
        let mut q = scheduler.run_queue.lock();
        for id in enqueued {
            if !q.iter().any(|x| x == &id) {
                q.push_back(id);
            }
        }
    }
    let _ = app.emit(SCHEDULED_JOBS_CHANGED_EVENT, ());
    drain_run_queue(app).await;
    Ok(())
}

async fn tick<R: Runtime>(app: AppHandle<R>) {
    let now = Utc::now();
    let due = match store::due_jobs(now) {
        Ok(j) => j,
        Err(e) => {
            log_scheduler(&app, format!("定时任务扫描失败: {e}"));
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    let scheduler = app.state::<SchedulerState>();
    let mut enqueued = false;
    {
        let mut q = scheduler.run_queue.lock();
        for job in due {
            if q.iter().any(|id| id == &job.id) {
                continue;
            }
            let _ = store::mark_job_queued(&job.id);
            let _ = store::advance_job_after_enqueue(&job.id, now);
            q.push_back(job.id);
            enqueued = true;
        }
    }
    if enqueued {
        let _ = app.emit(SCHEDULED_JOBS_CHANGED_EVENT, ());
        drain_run_queue(app).await;
    }
}

pub async fn enqueue_job_now<R: Runtime>(app: AppHandle<R>, job_id: String) -> Result<(), String> {
    store::get_scheduled_job(&job_id)?
        .ok_or_else(|| "定时任务不存在".to_string())?;
    let scheduler = app.state::<SchedulerState>();
    {
        let mut q = scheduler.run_queue.lock();
        if q.iter().any(|id| id == &job_id) {
            return Ok(());
        }
        q.push_back(job_id.clone());
    }
    let _ = store::mark_job_queued(&job_id);
    let _ = app.emit(SCHEDULED_JOBS_CHANGED_EVENT, ());
    drain_run_queue(app).await;
    Ok(())
}

async fn drain_run_queue<R: Runtime>(app: AppHandle<R>) {
    let scheduler = app.state::<SchedulerState>();
    {
        let mut draining = scheduler.draining.lock();
        if *draining {
            return;
        }
        *draining = true;
    }

    loop {
        let state = app.state::<AppState>();
        if state.task_registry.active_count() > 0 {
            break;
        }

        let job_id = {
            let mut q = scheduler.run_queue.lock();
            q.pop_front()
        };
        let Some(job_id) = job_id else {
            break;
        };

        let job = match store::get_scheduled_job(&job_id) {
            Ok(Some(j)) => j,
            Ok(None) => continue,
            Err(e) => {
                log_scheduler(&app, format!("读取定时任务失败: {e}"));
                continue;
            }
        };

        if !store::recipe_exists(&job.recipe_id) {
            let _ = store::mark_job_recipe_missing(&job.id);
            let _ = app.emit(SCHEDULED_JOBS_CHANGED_EVENT, ());
            continue;
        }

        spawn_scheduled_recipe(app.clone(), job);
        break;
    }

    *scheduler.draining.lock() = false;
}

fn spawn_scheduled_recipe<R: Runtime>(app: AppHandle<R>, job: ScheduledJob) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run_scheduled_job(&app, &job).await {
            log_scheduler(&app, format!("定时任务执行失败 ({}): {e}", job.name));
        }
        notify_execution_idle(&app);
        let _ = app.emit(SCHEDULED_JOBS_CHANGED_EVENT, ());
    });
}

async fn run_scheduled_job<R: Runtime>(app: &AppHandle<R>, job: &ScheduledJob) -> Result<(), String> {
    let recipe = recipe::get_recipe(&job.recipe_id)?
        .ok_or_else(|| "绑定的技能不存在（请先在主页同步技能列表）".to_string())?;

    let display_name = format!("[定时] {}", recipe.name);
    let task_id = format!("sched-{}", Uuid::new_v4());
    let state = app.state::<AppState>();
    let registry = state.task_registry.clone();
    let control = registry.register(&task_id);
    tray::refresh_tooltip(app);

    let initial_params = ordered_vec_to_params(&job.recipe_params)?;
    // 定时任务：表单已合并公共/技能参数，执行时剥离全部「参数」步骤，仅保留「动态参数」
    let (content, _) = recipe::strip_recipe_param_steps(&recipe.content)?;

    let offset = registry.next_window_offset();
    task_progress::create_task_window(app, &task_id, offset)?;
    task_progress::show_task_window(app, &task_id, &display_name);

    launch::enter_task_run_ui(app);

    let chat_session_id = Uuid::new_v4().to_string();
    let ran_at = Utc::now();

    let result = executor::execute_recipe_for_task_from_with_params(
        app,
        &state,
        &task_id,
        &control,
        Some(&display_name),
        &content,
        Some(&chat_session_id),
        None,
        &initial_params,
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
        &task_id,
        &display_name,
        success,
        &summary,
        steps,
        run_dir.clone(),
        artifacts,
        control.recipe_params_vec(),
    );

    let log_text = executor::format_task_logs_for_history(&control, &display_name, None);
    let history = launch::TaskHistoryAppendPayload {
        task_id: task_id.clone(),
        chat_session_id,
        name: display_name.clone(),
        path: None,
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

    state.task_registry.remove(&task_id);
    tray::refresh_tooltip(app);

    store::update_job_after_run(&job.id, success, &summary, ran_at)?;

    Ok(())
}

fn log_scheduler<R: Runtime>(app: &AppHandle<R>, line: String) {
    let state = app.state::<AppState>();
    state.push_log(line);
    crate::emit_logs_public(app, &state);
}
