use crate::run_artifacts::RunSession;
use crate::task_progress::{TaskProgressPayload, TaskStepRecord};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::recipe::Step;

#[derive(Clone)]
pub struct TaskControl {
    pub cancel: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub logs: Arc<Mutex<Vec<String>>>,
    pub step_records: Arc<Mutex<Vec<TaskStepRecord>>>,
    pub run_session: Arc<Mutex<Option<Arc<RunSession>>>>,
    /// 本任务专用 Chrome（独立端口 + Profile），任务结束时应 shutdown
    pub task_chrome: Arc<Mutex<Option<Arc<crate::chrome::ChromeInstance>>>>,
    /// 当前任务使用的调试端口（共用 9222 或任务端口）
    pub debug_port: Arc<Mutex<Option<u16>>>,
    /// 最近一次进度快照，供暂停/终止时立即刷新 UI
    pub last_progress: Arc<Mutex<Option<TaskProgressPayload>>>,
    /// 下一将执行步骤的 0-based 下标（暂停插入用）
    pub next_index: Arc<Mutex<usize>>,
    /// 回退到指定 0-based 下标（含），从该步重新执行
    pub seek_to: Arc<Mutex<Option<usize>>>,
    /// 跳过的 0-based 下标
    pub skip_set: Arc<Mutex<HashSet<usize>>>,
    /// 在 next_index 处插入的步骤
    pub insert_steps: Arc<Mutex<Vec<Step>>>,
    /// 本任务菜谱参数（参数公司=… → {{公司}}）
    pub recipe_params: Arc<Mutex<HashMap<String, String>>>,
    /// 「动态参数」写入的名称：执行期可用，但不回写顶部参数栏
    pub runtime_only_params: Arc<Mutex<HashSet<String>>>,
    /// 当前步骤模板（保留 {{名}}，供改参后刷新展示 / 晚绑定）
    pub live_steps: Arc<Mutex<Vec<Step>>>,
}

impl TaskControl {
    pub fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            pause: Arc::new(AtomicBool::new(false)),
            logs: Arc::new(Mutex::new(Vec::new())),
            step_records: Arc::new(Mutex::new(Vec::new())),
            run_session: Arc::new(Mutex::new(None)),
            task_chrome: Arc::new(Mutex::new(None)),
            debug_port: Arc::new(Mutex::new(None)),
            last_progress: Arc::new(Mutex::new(None)),
            next_index: Arc::new(Mutex::new(0)),
            seek_to: Arc::new(Mutex::new(None)),
            skip_set: Arc::new(Mutex::new(HashSet::new())),
            insert_steps: Arc::new(Mutex::new(Vec::new())),
            recipe_params: Arc::new(Mutex::new(HashMap::new())),
            runtime_only_params: Arc::new(Mutex::new(HashSet::new())),
            live_steps: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn recipe_params_vec(&self) -> Vec<crate::recipe::RecipeParamKv> {
        let params = self.recipe_params.lock();
        let skip = self.runtime_only_params.lock();
        let filtered: HashMap<String, String> = params
            .iter()
            .filter(|(k, _)| !skip.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        crate::recipe::params_to_ordered_vec(&filtered)
    }

    pub fn refresh_pending_step_labels(&self) {
        let params = self.recipe_params.lock().clone();
        let steps = self.live_steps.lock().clone();
        let mut recs = self.step_records.lock();
        for (i, step) in steps.iter().enumerate() {
            let Some(rec) = recs.get_mut(i) else { continue };
            if rec.status != "pending" && rec.status != "skipped" {
                continue;
            }
            let args = crate::recipe::resolve_step_args(&step.args, &params);
            rec.label = format!("{} {}", step.keyword, args);
        }
    }

    pub fn push_log(&self, line: impl Into<String>) {
        let line = line.into();
        self.logs.lock().push(line.clone());
        // 实时落盘：任务中途崩溃/OOM 时仍能保留已产生的执行日志
        let session = self.run_session.lock().clone();
        if let Some(session) = session {
            session.append_exec_log_line(&line);
        }
    }

    /// 记录本任务调试端口，供停止时强制结束浏览器。
    pub fn set_debug_port(&self, port: u16) {
        *self.debug_port.lock() = Some(port);
    }

    /// 立刻杀掉本任务相关 Chrome（含子进程树）。不等 CDP。
    /// 返回是否实际杀了进程 / 清了端口。
    pub fn force_kill_browser(&self) -> Option<u16> {
        let port = self.debug_port.lock().clone().or_else(|| {
            self.task_chrome.lock().as_ref().map(|c| c.port)
        });
        if let Some(ref chrome) = *self.task_chrome.lock() {
            chrome.force_kill();
            return Some(chrome.port);
        }
        if let Some(port) = port {
            crate::browser_network::drop_port_capture(port);
            crate::chrome::kill_listeners_on_port(port);
            return Some(port);
        }
        None
    }

    pub fn require_paused(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("任务已在停止中".into());
        }
        if !self.pause.load(Ordering::SeqCst) {
            return Err("请先暂停任务后再操作".into());
        }
        Ok(())
    }
}

pub struct TaskRegistry {
    tasks: Mutex<HashMap<String, TaskControl>>,
    window_serial: Mutex<u32>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            window_serial: Mutex::new(0),
        }
    }

    pub fn register(&self, task_id: &str) -> TaskControl {
        let ctrl = TaskControl::new();
        self.tasks.lock().insert(task_id.to_string(), ctrl.clone());
        ctrl
    }

    /// 注册已有控制块（如首页 main 需共享 `cancel_flag`）
    pub fn insert(&self, task_id: &str, ctrl: TaskControl) {
        self.tasks.lock().insert(task_id.to_string(), ctrl);
    }

    pub fn get(&self, task_id: &str) -> Option<TaskControl> {
        self.tasks.lock().get(task_id).cloned()
    }

    pub fn remove(&self, task_id: &str) {
        self.tasks.lock().remove(task_id);
    }

    pub fn next_window_offset(&self) -> (i32, i32) {
        let mut n = self.window_serial.lock();
        let idx = *n;
        *n = n.wrapping_add(1);
        drop(n);
        let slot = (idx % 8) as i32;
        (slot * 28, slot * 28)
    }

    pub fn pause_task(&self, task_id: &str) -> Result<(), String> {
        let tasks = self.tasks.lock();
        let ctrl = tasks.get(task_id).ok_or_else(|| "任务不存在或已结束".to_string())?;
        ctrl.pause.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn resume_task(&self, task_id: &str) -> Result<(), String> {
        let tasks = self.tasks.lock();
        let ctrl = tasks.get(task_id).ok_or_else(|| "任务不存在或已结束".to_string())?;
        ctrl.pause.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn stop_task(&self, task_id: &str) -> Result<(), String> {
        let tasks = self.tasks.lock();
        let ctrl = tasks.get(task_id).ok_or_else(|| "任务不存在或已结束".to_string())?;
        ctrl.cancel.store(true, Ordering::SeqCst);
        ctrl.pause.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn stop_all(&self) {
        for ctrl in self.tasks.lock().values() {
            ctrl.cancel.store(true, Ordering::SeqCst);
            ctrl.pause.store(false, Ordering::SeqCst);
        }
    }

    pub fn active_count(&self) -> usize {
        self.tasks.lock().len()
    }

    /// 快照活跃任务（克隆控制块），调用方可在不持有 registry 锁时发 UI 事件，避免停止路径死锁。
    pub fn snapshot_active(&self) -> Vec<(String, TaskControl)> {
        self.tasks
            .lock()
            .iter()
            .map(|(id, ctrl)| (id.clone(), ctrl.clone()))
            .collect()
    }

    pub fn for_each_active<F: FnMut(&str, &TaskControl)>(&self, mut f: F) {
        for (id, ctrl) in self.snapshot_active() {
            f(&id, &ctrl);
        }
    }
}

static CURRENT_HOST_JOB: Mutex<Option<TaskControl>> = Mutex::new(None);
static CANCEL_PENDING: AtomicBool = AtomicBool::new(false);

/// 登记当前宿主机任务，供中继 `cancel_job` 打断。
pub fn register_current_job(ctrl: TaskControl) {
    if CANCEL_PENDING.swap(false, Ordering::SeqCst) {
        ctrl.cancel.store(true, Ordering::SeqCst);
    }
    *CURRENT_HOST_JOB.lock() = Some(ctrl);
}

pub fn clear_current_job() {
    *CURRENT_HOST_JOB.lock() = None;
    CANCEL_PENDING.store(false, Ordering::SeqCst);
}

pub struct CurrentJobGuard;

impl Drop for CurrentJobGuard {
    fn drop(&mut self) {
        clear_current_job();
    }
}

/// 终止当前宿主机任务（取消标志 + 关掉任务浏览器）。
pub fn cancel_current_job() -> bool {
    CANCEL_PENDING.store(true, Ordering::SeqCst);
    let Some(ctrl) = CURRENT_HOST_JOB.lock().clone() else {
        return false;
    };
    ctrl.cancel.store(true, Ordering::SeqCst);
    ctrl.pause.store(false, Ordering::SeqCst);
    let chrome = ctrl.task_chrome.lock().clone();
    if let Some(ch) = chrome {
        ch.shutdown();
    }
    true
}

pub fn window_label(task_id: &str) -> String {
    format!("task-{task_id}")
}
