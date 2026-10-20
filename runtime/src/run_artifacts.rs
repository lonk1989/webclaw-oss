use chrono::Local;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use crate::ui_host::{AppHandle, Emitter, Manager, Runtime};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Screenshot,
    Document,
    Download,
    Other,
}

impl ArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Screenshot => "screenshot",
            ArtifactKind::Document => "document",
            ArtifactKind::Download => "download",
            ArtifactKind::Other => "other",
        }
    }

    fn subdir(self) -> &'static str {
        match self {
            ArtifactKind::Screenshot => "screenshots",
            ArtifactKind::Document => "documents",
            ArtifactKind::Download => "downloads",
            ArtifactKind::Other => "files",
        }
    }

    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "screenshot" | "截图" | "image" | "png" => ArtifactKind::Screenshot,
            "document" | "doc" | "总结" | "summary" | "markdown" | "md" | "txt" => {
                ArtifactKind::Document
            }
            "download" | "下载" | "file" => ArtifactKind::Download,
            _ => ArtifactKind::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunArtifact {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub rel_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_index: Option<u32>,
    pub size_bytes: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunArtifactsPayload {
    pub task_id: String,
    pub run_id: String,
    pub run_dir: String,
    pub artifacts: Vec<RunArtifact>,
}

pub struct RunSession {
    pub run_id: String,
    pub task_id: String,
    pub task_name: String,
    pub run_dir: PathBuf,
    artifacts: Mutex<Vec<RunArtifact>>,
    screenshot_serial: Mutex<u32>,
    /// 本轮是否已开始写入 executions/run-log.txt（路径固定为该文件）
    exec_log_path: Mutex<Option<PathBuf>>,
    /// 本任务已成功落盘的下载 URL，避免 Performance 历史记录重复触发
    handled_download_urls: Mutex<HashSet<String>>,
    /// 正在拉取中的 URL，避免自动泵与「等待下载」并发重复 fetch
    inflight_download_urls: Mutex<HashSet<String>>,
    /// 拉取失败次数；超过上限才放弃，避免一次性令牌偶发失败后永不重试
    download_fail_counts: Mutex<HashMap<String, u32>>,
    /// 已落盘、尚未被「等待下载」确认的产物 id（自动泵可能在点击步骤就下完）
    pending_wait_download_ids: Mutex<Vec<String>>,
    /// 下载产物最近一次纳入等待窗口的时间（含同名覆盖落盘）
    download_noticed_at: Mutex<HashMap<String, Instant>>,
    /// 上一轮「等待下载」确认时刻；其后落盘的文件才算本轮新文件
    last_wait_download_ack_at: Mutex<Instant>,
    /// 自动泵是否允许拉取落盘（含「上传」的菜谱默认关闭，仅「等待下载」期间打开）
    /// 自动泵是否允许拉取落盘（默认关闭，仅「等待下载」及其前一步打开）
    auto_fetch_armed: std::sync::atomic::AtomicBool,
    /// 是否启用「仅等待下载窗口武装」策略（现已对所有菜谱为 true）
    auto_fetch_wait_only: std::sync::atomic::AtomicBool,
    /// 本轮断言结果（多步写入同一 documents/断言报告-*.md）
    assert_report: Mutex<AssertReportState>,
}

#[derive(Debug, Default)]
struct AssertReportState {
    entries: Vec<AssertReportEntry>,
    file_name: Option<String>,
    artifact_registered: bool,
}

#[derive(Debug, Clone)]
pub struct AssertReportEntry {
    pub step_index: Option<u32>,
    pub script_name: String,
    pub args: String,
    pub ok: bool,
    pub detail: String,
}

impl RunSession {
    pub fn new(
        task_id: &str,
        task_name: &str,
        chat_session_id: Option<&str>,
    ) -> Result<Arc<Self>, String> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let bound_session_id =
            chat_session_id.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let (run_dir, session_bound) = if let Some(ref sid) = bound_session_id {
            (paths::chat_session_runs_dir(sid), true)
        } else {
            let folder_name = format!(
                "{}_{}_{}",
                Local::now().format("%Y%m%d_%H%M%S"),
                sanitize_folder_name(task_name),
                &run_id[..8]
            );
            (paths::runs_dir().join(folder_name), false)
        };
        for sub in ["screenshots", "documents", "downloads", "files", "executions"] {
            fs::create_dir_all(run_dir.join(sub)).map_err(|e| format!("创建产物目录失败: {e}"))?;
        }
        let readme_path = run_dir.join("README.md");
        if !readme_path.is_file() {
            let readme = if session_bound {
                format!(
                    "# webclaw 对话产物\n\n对话 ID: {}\n\n同一会话内多次执行的截图、下载、文档均保存在此目录。\n\n子目录:\n- screenshots/\n- documents/\n- downloads/\n- files/\n- executions/ 内部排障日志（不在产物面板展示）\n",
                    bound_session_id.as_deref().unwrap_or("")
                )
            } else {
                format!(
                    "# webclaw 运行产物\n\n任务: {task_name}\n时间: {}\n任务 ID: {task_id}\n运行 ID: {run_id}\n\n子目录:\n- screenshots/ 菜谱「截图」指令或脚本产物\n- documents/ 总结与文本、「采集」生成的页面文字目录\n- downloads/ 浏览器下载文件\n- files/ 手动添加的附件与脚本输出\n",
                    Local::now().format("%Y-%m-%d %H:%M:%S")
                )
            };
            fs::write(&readme_path, readme).map_err(|e| e.to_string())?;
        }

        let (artifacts, screenshot_serial) = scan_existing_artifacts(&run_dir);
        Ok(Arc::new(Self {
            run_id,
            task_id: task_id.to_string(),
            task_name: task_name.to_string(),
            run_dir,
            artifacts: Mutex::new(artifacts),
            screenshot_serial: Mutex::new(screenshot_serial),
            exec_log_path: Mutex::new(None),
            handled_download_urls: Mutex::new(HashSet::new()),
            inflight_download_urls: Mutex::new(HashSet::new()),
            download_fail_counts: Mutex::new(HashMap::new()),
            pending_wait_download_ids: Mutex::new(Vec::new()),
            download_noticed_at: Mutex::new(HashMap::new()),
            // 扫盘得到的历史附件不计入「新下载」
            last_wait_download_ack_at: Mutex::new(Instant::now()),
            auto_fetch_armed: AtomicBool::new(true),
            auto_fetch_wait_only: AtomicBool::new(false),
            assert_report: Mutex::new(AssertReportState::default()),
        }))
    }

    pub fn task_runs_dir(task_id: &str) -> PathBuf {
        let safe: String = task_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let safe = if safe.is_empty() { "task".into() } else { safe };
        paths::runs_dir().join(format!("task-{safe}"))
    }

    /// 新任务建目录；继续任务沿用同一 `task-{id}` 目录（下载/截图留给整合脚本）。
    pub fn new_or_resume(
        task_id: &str,
        task_name: &str,
        resume: bool,
    ) -> Result<Arc<Self>, String> {
        let run_dir = Self::task_runs_dir(task_id);
        if resume && run_dir.is_dir() {
            return Ok(Arc::new(Self::open_for_upload(&run_dir, task_id, task_name)?));
        }
        for sub in ["screenshots", "documents", "downloads", "files", "executions"] {
            fs::create_dir_all(run_dir.join(sub)).map_err(|e| format!("创建产物目录失败: {e}"))?;
        }
        let readme_path = run_dir.join("README.md");
        if !readme_path.is_file() {
            let readme = format!(
                "# webclaw 运行产物\n\n任务: {task_name}\n时间: {}\n任务 ID: {task_id}\n\n同一远程任务的多轮继续共用此目录。\n\n子目录:\n- screenshots/ 菜谱「截图」指令或脚本产物\n- documents/ 总结与文本、「采集」生成的页面文字目录\n- downloads/ 浏览器下载文件\n- files/ 手动添加的附件与脚本输出\n",
                Local::now().format("%Y-%m-%d %H:%M:%S")
            );
            fs::write(&readme_path, readme).map_err(|e| e.to_string())?;
        }
        let _ = fs::write(run_dir.join(".webclaw-task-id"), task_id);
        let (artifacts, screenshot_serial) = scan_existing_artifacts(&run_dir);
        Ok(Arc::new(Self {
            run_id: task_id.to_string(),
            task_id: task_id.to_string(),
            task_name: task_name.to_string(),
            run_dir,
            artifacts: Mutex::new(artifacts),
            screenshot_serial: Mutex::new(screenshot_serial),
            exec_log_path: Mutex::new(None),
            handled_download_urls: Mutex::new(HashSet::new()),
            inflight_download_urls: Mutex::new(HashSet::new()),
            download_fail_counts: Mutex::new(HashMap::new()),
            pending_wait_download_ids: Mutex::new(Vec::new()),
            download_noticed_at: Mutex::new(HashMap::new()),
            last_wait_download_ack_at: Mutex::new(Instant::now()),
            auto_fetch_armed: AtomicBool::new(true),
            auto_fetch_wait_only: AtomicBool::new(false),
            assert_report: Mutex::new(AssertReportState::default()),
        }))
    }

    /// 打开已有 runs 目录（用于后处理脚本测试）
    pub fn open_existing(run_dir: &Path) -> Result<Self, String> {
        Self::open_for_upload(run_dir, "test", "后处理测试")
    }

    /// 打开已有产物目录并扫描文件（用于手动添加附件）
    pub fn open_for_upload(run_dir: &Path, task_id: &str, task_name: &str) -> Result<Self, String> {
        if !run_dir.is_dir() {
            return Err(format!("运行目录不存在: {}", run_dir.display()));
        }
        for sub in ["screenshots", "documents", "downloads", "files", "executions"] {
            fs::create_dir_all(run_dir.join(sub)).map_err(|e| format!("创建产物目录失败: {e}"))?;
        }
        let run_id = run_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("upload")
            .to_string();
        let (artifacts, screenshot_serial) = scan_existing_artifacts(run_dir);
        Ok(Self {
            run_id,
            task_id: task_id.to_string(),
            task_name: task_name.to_string(),
            run_dir: run_dir.to_path_buf(),
            artifacts: Mutex::new(artifacts),
            screenshot_serial: Mutex::new(screenshot_serial),
            exec_log_path: Mutex::new(None),
            handled_download_urls: Mutex::new(HashSet::new()),
            inflight_download_urls: Mutex::new(HashSet::new()),
            download_fail_counts: Mutex::new(HashMap::new()),
            pending_wait_download_ids: Mutex::new(Vec::new()),
            download_noticed_at: Mutex::new(HashMap::new()),
            last_wait_download_ack_at: Mutex::new(Instant::now()),
            auto_fetch_armed: AtomicBool::new(true),
            auto_fetch_wait_only: AtomicBool::new(false),
            assert_report: Mutex::new(AssertReportState::default()),
        })
    }

    /// 确保对话绑定的产物目录存在（尚未跑过任务时也可先传附件）
    pub fn ensure_chat_session_dir(chat_session_id: &str) -> Result<PathBuf, String> {
        let sid = chat_session_id.trim();
        if sid.is_empty() {
            return Err("对话 ID 为空".into());
        }
        let run_dir = paths::chat_session_runs_dir(sid);
        for sub in ["screenshots", "documents", "downloads", "files", "executions"] {
            fs::create_dir_all(run_dir.join(sub)).map_err(|e| format!("创建产物目录失败: {e}"))?;
        }
        let readme_path = run_dir.join("README.md");
        if !readme_path.is_file() {
            let readme = format!(
                "# webclaw 对话产物\n\n对话 ID: {sid}\n\n同一会话内多次执行的截图、下载、文档均保存在此目录。\n\n子目录:\n- screenshots/\n- documents/\n- downloads/\n- files/\n- executions/ 内部排障日志（不在产物面板展示）\n"
            );
            fs::write(&readme_path, readme).map_err(|e| e.to_string())?;
        }
        Ok(run_dir)
    }

    pub fn is_download_url_handled(&self, url: &str) -> bool {
        self.handled_download_urls.lock().contains(url)
            || self.inflight_download_urls.lock().contains(url)
    }

    /// 开始拉取：若已成功或正在拉取则返回 false。
    pub fn try_begin_download_url(&self, url: &str) -> bool {
        if self.handled_download_urls.lock().contains(url) {
            return false;
        }
        let mut inflight = self.inflight_download_urls.lock();
        if inflight.contains(url) {
            return false;
        }
        inflight.insert(url.to_string());
        true
    }

    pub fn mark_download_url_handled(&self, url: &str) {
        self.inflight_download_urls.lock().remove(url);
        self.handled_download_urls.lock().insert(url.to_string());
        self.download_fail_counts.lock().remove(url);
    }

    pub fn release_download_url_inflight(&self, url: &str) {
        self.inflight_download_urls.lock().remove(url);
    }

    /// 记录一次拉取失败；返回当前失败次数。达到 `max_fails` 时标记已处理并返回 `None`（放弃）。
    pub fn record_download_fetch_failure(&self, url: &str, max_fails: u32) -> Option<u32> {
        self.release_download_url_inflight(url);
        let mut counts = self.download_fail_counts.lock();
        let n = counts.entry(url.to_string()).or_insert(0);
        *n = n.saturating_add(1);
        let cur = *n;
        drop(counts);
        if cur >= max_fails {
            self.mark_download_url_handled(url);
            None
        } else {
            Some(cur)
        }
    }

    pub fn download_artifact_ids(&self) -> HashSet<String> {
        self.artifacts
            .lock()
            .iter()
            .filter(|a| a.kind == "download")
            .map(|a| a.id.clone())
            .collect()
    }

    pub fn new_download_artifacts_since(&self, before: &HashSet<String>) -> Vec<RunArtifact> {
        self.artifacts
            .lock()
            .iter()
            .filter(|a| a.kind == "download" && !before.contains(&a.id))
            .cloned()
            .collect()
    }

    fn note_download_for_wait(&self, art: &RunArtifact) {
        if art.kind != "download" {
            return;
        }
        // wait_only：未武装时落盘不计入「等待下载」，避免上一轮迟到文件毒化下一轮
        if self.is_auto_fetch_wait_only() && !self.is_auto_fetch_armed() {
            self.append_download_trace(format!(
                "落盘但不计入等待（未武装）: {}",
                art.name
            ));
            return;
        }
        self.download_noticed_at
            .lock()
            .insert(art.id.clone(), Instant::now());
        let mut q = self.pending_wait_download_ids.lock();
        if !q.iter().any(|id| id == &art.id) {
            q.push(art.id.clone());
        }
    }

    /// 开启新的「点击导出 → 等待下载」窗口：丢弃上一轮未确认/迟到落盘，避免误判成功。
    pub fn begin_wait_download_window(&self) {
        let dropped = self.pending_wait_download_ids.lock().len();
        self.pending_wait_download_ids.lock().clear();
        *self.last_wait_download_ack_at.lock() = Instant::now();
        if dropped > 0 {
            self.append_download_trace(format!(
                "下载截获 | 新等待窗口：已丢弃上一轮未确认落盘 {dropped} 个"
            ));
        } else {
            self.append_download_trace("下载截获 | 新等待窗口");
        }
    }

    fn resolve_download_ids(&self, ids: &[String]) -> Vec<RunArtifact> {
        if ids.is_empty() {
            return Vec::new();
        }
        let list = self.artifacts.lock();
        ids.iter()
            .filter_map(|id| list.iter().find(|a| a.id == *id).cloned())
            .collect()
    }

    /// 取出尚未被「等待下载」确认的落盘文件（自动泵可能在点击步骤就已完成）。
    /// 若 id 暂时对不上产物表，会放回队列，避免静默丢确认。
    pub fn take_pending_wait_downloads(&self) -> Vec<RunArtifact> {
        let ids: Vec<String> = std::mem::take(&mut *self.pending_wait_download_ids.lock());
        if ids.is_empty() {
            return Vec::new();
        }
        let found = self.resolve_download_ids(&ids);
        if found.len() < ids.len() {
            let found_ids: HashSet<&str> = found.iter().map(|a| a.id.as_str()).collect();
            let mut q = self.pending_wait_download_ids.lock();
            for id in ids {
                if !found_ids.contains(id.as_str()) && !q.iter().any(|x| x == &id) {
                    q.push(id);
                }
            }
        }
        found
    }

    /// 「等待下载」就绪探测：pending 队列 ∪ 上次确认之后新注意到的下载。
    pub fn poll_wait_download_ready(&self) -> Vec<RunArtifact> {
        let pending = self.take_pending_wait_downloads();
        if !pending.is_empty() {
            return pending;
        }
        let since = *self.last_wait_download_ack_at.lock();
        let noticed = self.download_noticed_at.lock();
        let recent_ids: Vec<String> = noticed
            .iter()
            .filter(|(_, at)| **at > since)
            .map(|(id, _)| id.clone())
            .collect();
        drop(noticed);
        self.resolve_download_ids(&recent_ids)
    }

    /// 确认本轮已消费的下载，避免下一轮「等待下载」误用同一批文件。
    pub fn ack_wait_downloads(&self, arts: &[RunArtifact]) {
        if arts.is_empty() {
            return;
        }
        let ids: HashSet<&str> = arts.iter().map(|a| a.id.as_str()).collect();
        self.pending_wait_download_ids
            .lock()
            .retain(|id| !ids.contains(id.as_str()));
        *self.last_wait_download_ack_at.lock() = Instant::now();
    }

    pub fn pending_wait_download_count(&self) -> usize {
        let pending = self.pending_wait_download_ids.lock().len();
        if pending > 0 {
            return pending;
        }
        let since = *self.last_wait_download_ack_at.lock();
        self.download_noticed_at
            .lock()
            .values()
            .filter(|at| **at > since)
            .count()
    }

    /// 配置自动落盘武装策略。
    /// 一律：默认不武装，仅在「等待下载」及其前一步武装（覆盖点击瞬时下完，并降低空闲期 CDP 压力）。
    /// `has_upload_step` 保留给调用方写日志等区分，不再改变武装策略。
    pub fn configure_auto_fetch_for_recipe(&self, _has_upload_step: bool) {
        self.auto_fetch_wait_only.store(true, Ordering::SeqCst);
        self.auto_fetch_armed.store(false, Ordering::SeqCst);
    }

    pub fn set_auto_fetch_armed(&self, armed: bool) {
        self.auto_fetch_armed.store(armed, Ordering::SeqCst);
    }

    pub fn is_auto_fetch_armed(&self) -> bool {
        self.auto_fetch_armed.load(Ordering::SeqCst)
    }

    pub fn is_auto_fetch_wait_only(&self) -> bool {
        self.auto_fetch_wait_only.load(Ordering::SeqCst)
    }

    /// 追加下载截获追踪日志（写入 executions/download-capture.log，供排查误抓；不对用户展示）
    pub fn append_download_trace(&self, line: impl AsRef<str>) {
        let path = self.run_dir.join("executions").join("download-capture.log");
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let stamp = Local::now().format("%H:%M:%S%.3f");
        let entry = format!("[{stamp}] {}\n", line.as_ref());
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(entry.as_bytes())
            });
    }

    fn run_key(&self) -> String {
        self.run_id.chars().take(8).collect()
    }

    fn ensure_exec_log_file(&self) -> Option<PathBuf> {
        let mut slot = self.exec_log_path.lock();
        if let Some(p) = slot.as_ref() {
            return Some(p.clone());
        }
        let exec_dir = self.run_dir.join("executions");
        if fs::create_dir_all(&exec_dir).is_err() {
            return None;
        }
        let run_key = self.run_key();
        let path = exec_dir.join("run-log.txt");
        let header = format!(
            "\n\n════ {} | run {run_key} ════\n",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
        );
        if fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(header.as_bytes())
            })
            .is_err()
        {
            return None;
        }
        *slot = Some(path.clone());
        Some(path)
    }

    /// 执行过程中实时追加一行日志（中途崩溃也能保留已产生的日志）
    pub fn append_exec_log_line(&self, line: impl AsRef<str>) {
        let line = line.as_ref();
        if line.is_empty() {
            return;
        }
        let Some(path) = self.ensure_exec_log_file() else {
            return;
        };
        let entry = format!("{line}\n");
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(entry.as_bytes())
            });
    }

    pub fn run_dir_display(&self) -> String {
        self.run_dir.to_string_lossy().into_owned()
    }

    pub fn list_artifacts(&self) -> Vec<RunArtifact> {
        self.artifacts.lock().clone()
    }

    /// 删除运行目录内的产物文件（真实删盘），并刷新列表。
    /// `paths` 为绝对路径；必须位于本 `run_dir` 下，否则拒绝。
    pub fn delete_artifacts(
        &self,
        paths: &[String],
    ) -> Result<(Vec<String>, Vec<String>, Vec<RunArtifact>), String> {
        if paths.is_empty() {
            return Err("未选择要删除的文件".into());
        }
        let run_dir = self
            .run_dir
            .canonicalize()
            .unwrap_or_else(|_| self.run_dir.clone());

        let mut deleted = Vec::new();
        let mut errors = Vec::new();

        for raw in paths {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let p = PathBuf::from(trimmed);
            let name_guess = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| trimmed.to_string());

            let canon = match p.canonicalize() {
                Ok(c) => c,
                Err(_) => {
                    // 文件可能已不存在：仍从列表移除
                    let key = path_key(&p);
                    let mut list = self.artifacts.lock();
                    list.retain(|a| path_key(Path::new(&a.path)) != key);
                    deleted.push(name_guess);
                    continue;
                }
            };
            if !path_is_within(&canon, &run_dir) {
                errors.push(format!("拒绝删除目录外文件: {name_guess}"));
                continue;
            }
            if !canon.is_file() {
                errors.push(format!("不是文件: {name_guess}"));
                continue;
            }
            let name = canon
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(name_guess);
            match fs::remove_file(&canon) {
                Ok(()) => {
                    let key = path_key(&canon);
                    let mut list = self.artifacts.lock();
                    list.retain(|a| path_key(Path::new(&a.path)) != key);
                    deleted.push(name);
                }
                Err(e) => errors.push(format!("删除失败 {name}: {e}")),
            }
        }

        if deleted.is_empty() {
            return Err(if errors.is_empty() {
                "未删除任何文件".into()
            } else {
                errors.join("；")
            });
        }

        // 与磁盘再对齐一次（避免列表残留）
        let (fresh, serial) = scan_existing_artifacts(&self.run_dir);
        *self.artifacts.lock() = fresh;
        *self.screenshot_serial.lock() = serial;

        Ok((deleted, errors, self.list_artifacts()))
    }

    pub fn register_existing(
        &self,
        kind: ArtifactKind,
        file_path: &Path,
        step_index: Option<u32>,
        display_name: Option<&str>,
    ) -> Result<RunArtifact, String> {
        if !file_path.is_file() {
            return Err(format!("产物文件不存在: {}", file_path.display()));
        }
        let name = display_name
            .map(String::from)
            .unwrap_or_else(|| {
                file_path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".into())
            });
        let rel = file_path
            .strip_prefix(&self.run_dir)
            .map(|p| path_key(p))
            .unwrap_or_else(|_| name.clone());
        let size = fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        let path_str = path_key(file_path);
        let art = RunArtifact {
            id: uuid::Uuid::new_v4().to_string(),
            kind: kind.as_str().into(),
            name,
            path: path_str.clone(),
            rel_path: rel,
            step_index,
            size_bytes: size,
            created_at: Local::now().format("%H:%M:%S").to_string(),
        };
        let mut list = self.artifacts.lock();
        if let Some(existing) = list.iter_mut().find(|a| path_key(Path::new(&a.path)) == path_str) {
            // 同一路径只保留一条；同名再次落盘仍要通知「等待下载」
            existing.name = art.name.clone();
            existing.path = path_str;
            existing.rel_path = art.rel_path.clone();
            existing.size_bytes = size;
            if step_index.is_some() {
                existing.step_index = step_index;
            }
            let out = existing.clone();
            drop(list);
            if kind == ArtifactKind::Download {
                self.note_download_for_wait(&out);
            }
            return Ok(out);
        }
        list.push(art.clone());
        drop(list);
        if kind == ArtifactKind::Download {
            self.note_download_for_wait(&art);
        }
        Ok(art)
    }

    fn next_path(&self, kind: ArtifactKind, filename: &str) -> PathBuf {
        self.run_dir.join(kind.subdir()).join(filename)
    }

    fn push_bytes(
        &self,
        kind: ArtifactKind,
        filename: &str,
        data: &[u8],
        step_index: Option<u32>,
    ) -> Result<RunArtifact, String> {
        let path = self.next_path(kind, filename);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, data).map_err(|e| format!("写入产物失败: {e}"))?;
        self.register_existing(kind, &path, step_index, None)
    }

    pub fn save_text_document(
        &self,
        filename: &str,
        content: &str,
        step_index: Option<u32>,
    ) -> Result<RunArtifact, String> {
        self.push_bytes(ArtifactKind::Document, filename, content.as_bytes(), step_index)
    }

    /// 追加一条断言结果，写入/更新本轮唯一的 `documents/断言报告-*.md`
    pub fn record_assert_result(&self, entry: AssertReportEntry) -> Result<(), String> {
        let mut st = self.assert_report.lock();
        st.entries.push(entry);
        if st.file_name.is_none() {
            let stamp = Local::now().format("%Y%m%d-%H%M%S");
            st.file_name = Some(format!("断言报告-{stamp}.md"));
        }
        let fname = st.file_name.clone().unwrap();
        let body = render_assert_report_md(&self.task_name, &st.entries);
        let path = self.run_dir.join("documents").join(&fname);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, body).map_err(|e| format!("写入断言报告失败: {e}"))?;
        let need_register = !st.artifact_registered;
        if need_register {
            st.artifact_registered = true;
        }
        drop(st);
        if need_register {
            let _ = self.register_existing(ArtifactKind::Document, &path, None, Some(&fname));
        }
        Ok(())
    }

    /// 任务结束时输出的断言摘要（无断言则 None）
    pub fn assert_report_user_summary(&self) -> Option<String> {
        let st = self.assert_report.lock();
        if st.entries.is_empty() {
            return None;
        }
        let pass = st.entries.iter().filter(|e| e.ok).count();
        let fail = st.entries.len() - pass;
        let mut lines = Vec::new();
        lines.push(format!("断言汇总：{pass} 通过 / {fail} 失败"));
        for (i, e) in st.entries.iter().enumerate() {
            let mark = if e.ok { "✓" } else { "✗" };
            let step = e
                .step_index
                .map(|s| format!("步骤{s} "))
                .unwrap_or_default();
            let args = if e.args.is_empty() {
                String::new()
            } else {
                format!(" {}", e.args)
            };
            let detail = if e.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", e.detail)
            };
            lines.push(format!(
                "{}. {} {}{}{}{}",
                i + 1,
                mark,
                step,
                e.script_name,
                args,
                detail
            ));
        }
        if let Some(f) = &st.file_name {
            lines.push(format!("报告：documents/{f}，可在运行产物中预览"));
        }
        Some(format!("⟦断言报告⟧\n{}\n⟦/断言报告⟧", lines.join("\n")))
    }

    pub fn save_png_bytes(
        &self,
        filename: &str,
        png: &[u8],
        step_index: Option<u32>,
    ) -> Result<RunArtifact, String> {
        self.push_bytes(ArtifactKind::Screenshot, filename, png, step_index)
    }

    pub fn save_step_screenshot(
        &self,
        png: &[u8],
        step_index: u32,
        step_label: &str,
    ) -> Result<RunArtifact, String> {
        let mut n = self.screenshot_serial.lock();
        *n += 1;
        let serial = *n;
        drop(n);
        let slug = sanitize_filename(step_label);
        let name = format!("step-{step_index:02}-{serial:02}-{slug}.png");
        self.save_png_bytes(&name, png, Some(step_index))
    }

    pub fn save_download_bytes(
        &self,
        filename: &str,
        data: &[u8],
        step_index: Option<u32>,
    ) -> Result<RunArtifact, String> {
        let fname = sanitize_download_filename_for_disk(filename);
        self.push_bytes(ArtifactKind::Download, &fname, data, step_index)
    }

    pub fn ingest_download_path(
        &self,
        source: &Path,
        step_index: Option<u32>,
        name: Option<&str>,
    ) -> Result<RunArtifact, String> {
        self.ingest_path(ArtifactKind::Download, source, step_index, name)
    }

    /// 手动添加附件：落入 `files/`（与浏览器自动下载的 `downloads/` 区分）
    pub fn ingest_attachment_path(
        &self,
        source: &Path,
        step_index: Option<u32>,
        name: Option<&str>,
    ) -> Result<RunArtifact, String> {
        self.ingest_path(ArtifactKind::Other, source, step_index, name)
    }

    fn ingest_path(
        &self,
        kind: ArtifactKind,
        source: &Path,
        step_index: Option<u32>,
        name: Option<&str>,
    ) -> Result<RunArtifact, String> {
        let file_name = name
            .map(String::from)
            .or_else(|| {
                source
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .ok_or_else(|| "附件文件".to_string())?;
        let dest = self.next_path(kind, &sanitize_download_filename_for_disk(&file_name));
        if source != dest {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(source, &dest).map_err(|e| format!("复制附件失败: {e}"))?;
        }
        self.register_existing(kind, &dest, step_index, Some(&file_name))
    }

    pub fn process_value(&self, v: &Value, step_index: Option<u32>) -> Result<Vec<RunArtifact>, String> {
        let mut out = Vec::new();
        if let Some(one) = v.get("artifact") {
            if let Ok(a) = self.process_one(one, step_index) {
                out.push(a);
            }
        }
        if let Some(arr) = v.get("artifacts").and_then(|a| a.as_array()) {
            for item in arr {
                if let Ok(a) = self.process_one(item, step_index) {
                    out.push(a);
                }
            }
        }
        Ok(out)
    }

    fn process_one(&self, item: &Value, step_index: Option<u32>) -> Result<RunArtifact, String> {
        let kind = item
            .get("kind")
            .or_else(|| item.get("type"))
            .and_then(|x| x.as_str())
            .map(ArtifactKind::from_str)
            .unwrap_or(ArtifactKind::Other);

        let name = item
            .get("name")
            .or_else(|| item.get("filename"))
            .and_then(|x| x.as_str())
            .unwrap_or("artifact");

        let step = step_index.or_else(|| {
            item.get("step")
                .or_else(|| item.get("stepIndex"))
                .and_then(|x| x.as_u64())
                .map(|n| n as u32)
        });

        if let Some(content) = item.get("content").and_then(|x| x.as_str()) {
            let fname = ensure_extension(name, kind);
            return self.save_text_document(&fname, content, step);
        }

        if let Some(b64) = item
            .get("data")
            .or_else(|| item.get("base64"))
            .and_then(|x| x.as_str())
        {
            let bytes = decode_base64(b64)?;
            let fname = ensure_extension(name, kind);
            return self.push_bytes(kind, &fname, &bytes, step);
        }

        if let Some(path) = item.get("path").and_then(|x| x.as_str()) {
            let p = PathBuf::from(path);
            if p.is_file() {
                if kind == ArtifactKind::Download {
                    return self.ingest_download_path(&p, step, Some(name));
                }
                // 已在本次 run 目录内（如 Rhai 写入 files/）— 直接登记，勿再复制
                if path_is_within(&p, &self.run_dir) {
                    let display = if name.is_empty() || name == "artifact" {
                        p.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or(name)
                            .to_string()
                    } else {
                        name.to_string()
                    };
                    return self.register_existing(kind, &p, step, Some(&display));
                }
                let dest_name = sanitize_download_filename_for_disk(
                    if name.is_empty() || name == "artifact" {
                        p.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("artifact")
                    } else {
                        name
                    },
                );
                let dest = self.next_path(kind, &dest_name);
                if p != dest {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    fs::copy(&p, &dest).map_err(|e| format!("复制产物失败: {e}"))?;
                }
                return self.register_existing(kind, &dest, step, Some(name));
            }
        }

        Err(format!("无法识别的产物描述: {name}"))
    }

    /// 扫描 `runs/.../downloads/` 中新文件并登记为产物
    pub fn sync_downloads(&self) -> Result<Vec<RunArtifact>, String> {
        self.sync_downloads_in_run_dir()
    }

    /// 扫描 `runs/.../files/` 中新文件并登记为产物（Rhai 脚本输出等）
    pub fn sync_files(&self) -> Result<Vec<RunArtifact>, String> {
        let dir = self.run_dir.join("files");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let known: HashSet<String> = self
            .artifacts
            .lock()
            .iter()
            .map(|a| path_key(Path::new(&a.path)))
            .collect();
        let mut added = Vec::new();
        let entries = fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if is_ignored_artifact_name(name) || is_degenerate_artifact_name(name) {
                continue;
            }
            let key = path_key(&path);
            if known.contains(&key) {
                continue;
            }
            if let Ok(art) = self.register_existing(ArtifactKind::Other, &path, None, None) {
                added.push(art);
            }
        }
        Ok(added)
    }

    fn sync_downloads_in_run_dir(&self) -> Result<Vec<RunArtifact>, String> {
        let dir = self.run_dir.join("downloads");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let known: HashSet<String> = self
            .artifacts
            .lock()
            .iter()
            .map(|a| path_key(Path::new(&a.path)))
            .collect();
        let mut added = Vec::new();
        let entries = fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if is_ignored_artifact_name(name) || is_incomplete_download(name) {
                continue;
            }
            let key = path_key(&path);
            if known.contains(&key) {
                continue;
            }
            if let Ok(art) = self.register_existing(ArtifactKind::Download, &path, None, None) {
                added.push(art);
            }
        }
        Ok(added)
    }

    pub fn write_run_summary_log(&self, lines: &[String]) -> Result<(), String> {
        let mut body = lines.join("\n");
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        let exec_dir = self.run_dir.join("executions");
        fs::create_dir_all(&exec_dir).map_err(|e| format!("创建 executions 目录失败: {e}"))?;

        let run_key = self.run_key();
        let live = self.exec_log_path.lock().is_some();
        let consolidated = exec_dir.join("run-log.txt");
        let header = format!(
            "\n\n════ {} | run {run_key} ════\n",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
        );
        // 只写一份 executions/run-log.txt；实时追加过则以完整内存日志替换本轮段落
        if live {
            replace_run_log_section(&consolidated, &run_key, &header, &body)?;
        } else if consolidated.is_file() {
            let mut existing = fs::read_to_string(&consolidated).unwrap_or_default();
            existing.push_str(&header);
            existing.push_str(&body);
            fs::write(&consolidated, existing).map_err(|e| format!("追加 run-log.txt 失败: {e}"))?;
        } else {
            fs::write(&consolidated, format!("{header}{body}"))
                .map_err(|e| format!("写入 run-log.txt 失败: {e}"))?;
        }
        *self.exec_log_path.lock() = Some(consolidated);
        // 不登记到产物列表：executions/ 为内部排障目录
        Ok(())
    }
}

fn is_degenerate_artifact_name(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    !stem.is_empty() && stem.chars().all(|c| c == '-' || c == '_' || c == '.')
}

/// 用完整正文替换 run-log.txt 中本轮 `run {key}` 段落；找不到则追加。
fn replace_run_log_section(
    consolidated: &Path,
    run_key: &str,
    header: &str,
    body: &str,
) -> Result<(), String> {
    let marker = format!("| run {run_key} ════");
    let existing = fs::read_to_string(consolidated).unwrap_or_default();
    let Some(marker_at) = existing.rfind(&marker) else {
        let mut out = existing;
        out.push_str(header);
        out.push_str(body);
        return fs::write(consolidated, out).map_err(|e| format!("追加 run-log.txt 失败: {e}"));
    };
    let section_start = existing[..marker_at]
        .rfind("════")
        .unwrap_or(marker_at);
    let search_from = marker_at + marker.len();
    let next_section = existing[search_from..]
        .find("\n\n════ ")
        .map(|i| search_from + i);
    let mut out = String::with_capacity(existing.len() + body.len() + header.len());
    out.push_str(&existing[..section_start]);
    out.push_str(header);
    out.push_str(body);
    if let Some(n) = next_section {
        out.push_str(&existing[n..]);
    }
    fs::write(consolidated, out).map_err(|e| format!("更新 run-log.txt 失败: {e}"))
}

/// 跳过不应出现在产物列表中的临时/隐藏文件（如 Office `~$*.xlsx` 锁文件）。
fn is_ignored_artifact_name(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    if name.starts_with('.') || name.starts_with("~$") {
        return true;
    }
    if name.ends_with(".tmp-replace") {
        return true;
    }
    // 脚本工作副本，不进产物列表（最终表为「YYYY.MM.DD每日检查记录表.xlsx」）
    if name.eq_ignore_ascii_case("_daily-check-work.xlsx") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower == "thumbs.db" || lower == "desktop.ini" || lower == ".ds_store"
}

/// 统一路径键：正斜杠 + Windows 下小写，避免 `files/a.xlsx` 与 `files\a.xlsx` 被当成两条产物
fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        s.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        s
    }
}

fn path_is_within(child: &Path, parent: &Path) -> bool {
    let c = path_key(child);
    let p = path_key(parent).trim_end_matches('/').to_string();
    c == p || c.starts_with(&(p.clone() + "/"))
}

#[cfg(test)]
mod path_key_tests {
    use super::path_key;
    use std::path::PathBuf;

    #[test]
    fn slash_variants_share_key() {
        let base = PathBuf::from(r"D:\data\runs\sessions\abc");
        let mixed = base.join("files/考勤记录表-已填充.xlsx");
        let native = base.join("files").join("考勤记录表-已填充.xlsx");
        assert_eq!(path_key(&mixed), path_key(&native));
    }
}

#[cfg(test)]
mod exec_log_tests {
    use super::*;

    #[test]
    fn live_append_and_finalize_writes_exec_log() {
        let dir = std::env::temp_dir().join(format!("webclaw-exec-log-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("documents")).unwrap();
        fs::create_dir_all(dir.join("executions")).unwrap();
        let session = RunSession::open_for_upload(&dir, "t", "test").unwrap();

        session.append_exec_log_line("步骤 1 成功");
        session.append_exec_log_line("步骤 2 成功");
        session.append_download_trace("开始拉取: demo.xlsx");

        let run_log = dir.join("executions").join("run-log.txt");
        let live = fs::read_to_string(&run_log).unwrap();
        assert!(live.contains("步骤 1 成功"));
        assert!(live.contains("步骤 2 成功"));
        // 不再额外写 stamp_runKey.txt
        let extras: Vec<_> = fs::read_dir(dir.join("executions"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "run-log.txt" && n != "download-capture.log")
            .collect();
        assert!(extras.is_empty(), "unexpected exec files: {extras:?}");

        let capture = dir.join("executions").join("download-capture.log");
        assert!(capture.is_file());
        assert!(fs::read_to_string(&capture).unwrap().contains("demo.xlsx"));

        session
            .write_run_summary_log(&[
                "步骤 1 成功".into(),
                "步骤 2 成功".into(),
                "════ 结束 ════".into(),
            ])
            .unwrap();

        let final_body = fs::read_to_string(&run_log).unwrap();
        assert_eq!(final_body.matches("步骤 1 成功").count(), 1);
        assert!(final_body.contains("════ 结束 ════"));

        // 内部日志不进入用户产物列表
        let arts = session.list_artifacts();
        assert!(!arts.iter().any(|a| {
            a.name.contains("run-log")
                || a.name.contains("download-capture")
                || a.rel_path.contains("executions/")
        }));

        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod wait_download_ack_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn wait_sees_file_saved_before_wait_even_if_pending_drained() {
        let dir = std::env::temp_dir().join(format!("webclaw-wait-dl-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("downloads")).unwrap();
        let session = RunSession::open_for_upload(&dir, "t", "test").unwrap();

        let art = session
            .save_download_bytes("2026-07月报.xls", b"PK\x03\x04hello", None)
            .unwrap();
        // 模拟 pending 被异常掏空，但仍应能凭 noticed_at 识别
        let _ = session.take_pending_wait_downloads();
        assert!(session.pending_wait_download_ids.lock().is_empty());

        let ready = session.poll_wait_download_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, art.id);

        session.ack_wait_downloads(&ready);
        assert!(session.poll_wait_download_ready().is_empty());

        // 同名再次落盘应重新进入等待窗口
        std::thread::sleep(Duration::from_millis(5));
        let again = session
            .save_download_bytes("2026-07月报.xls", b"PK\x03\x04world!!", None)
            .unwrap();
        assert_eq!(again.id, art.id);
        let ready2 = session.poll_wait_download_ready();
        assert_eq!(ready2.len(), 1);
        assert_eq!(ready2[0].id, art.id);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_wait_window_ignores_stale_pending_from_prior_round() {
        let dir = std::env::temp_dir().join(format!("webclaw-wait-stale-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("downloads")).unwrap();
        let session = RunSession::open_for_upload(&dir, "t", "test").unwrap();
        session.configure_auto_fetch_for_recipe(false);
        session.set_auto_fetch_armed(true);

        let stale = session
            .save_download_bytes("页面结果导出_old.xlsx", b"PK\x03\x04stale", None)
            .unwrap();
        assert!(!session.poll_wait_download_ready().is_empty());

        // 模拟：上一轮结束后迟到落盘已入队；下一轮点击导出时开新窗口
        session.set_auto_fetch_armed(false);
        let _ = session
            .save_download_bytes("页面结果导出_late.xlsx", b"PK\x03\x04late!", None)
            .unwrap();
        // 未武装：不应再入 pending（即便手动塞一个旧 id）
        session.pending_wait_download_ids.lock().push(stale.id.clone());
        session.begin_wait_download_window();
        session.set_auto_fetch_armed(true);
        assert!(
            session.poll_wait_download_ready().is_empty(),
            "新窗口不得立刻吃掉上一轮文件"
        );

        std::thread::sleep(Duration::from_millis(5));
        let fresh = session
            .save_download_bytes("加班台账.xlsx", b"PK\x03\x04fresh", None)
            .unwrap();
        let ready = session.poll_wait_download_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, fresh.id);
        assert_eq!(ready[0].name, "加班台账.xlsx");

        let _ = fs::remove_dir_all(&dir);
    }
}

fn scan_existing_artifacts(run_dir: &Path) -> (Vec<RunArtifact>, u32) {
    let mut artifacts = Vec::new();
    let mut max_serial = 0u32;
    for kind in [
        ArtifactKind::Screenshot,
        ArtifactKind::Document,
        ArtifactKind::Download,
        ArtifactKind::Other,
    ] {
        let sub = run_dir.join(kind.subdir());
        if !sub.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&sub) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            if name == "run-log.txt" || name == "download-capture.log" {
                continue;
            }
            if is_ignored_artifact_name(&name) || is_degenerate_artifact_name(&name) {
                continue;
            }
            if kind == ArtifactKind::Screenshot {
                if let Some(serial) = parse_screenshot_serial(&name) {
                    max_serial = max_serial.max(serial);
                }
            }
            let rel = path
                .strip_prefix(run_dir)
                .map(|p| path_key(p))
                .unwrap_or_else(|_| name.clone());
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let modified = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    chrono::DateTime::<Local>::from(t)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_else(|| Local::now().format("%H:%M:%S").to_string());
            artifacts.push(RunArtifact {
                id: uuid::Uuid::new_v4().to_string(),
                kind: kind.as_str().into(),
                name,
                path: path_key(&path),
                rel_path: rel,
                step_index: None,
                size_bytes: size,
                created_at: modified,
            });
        }
    }
    // executions/ 为内部排障目录（执行日志、下载捕获），不扫入用户产物列表
    artifacts.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    (artifacts, max_serial)
}

fn parse_screenshot_serial(filename: &str) -> Option<u32> {
    // step-02-03-label.png → serial 03
    let base = filename.strip_suffix(".png")?;
    let parts: Vec<&str> = base.split('-').collect();
    if parts.len() >= 3 && parts[0] == "step" {
        return parts[2].parse().ok();
    }
    None
}

pub fn emit_artifacts_update<R: Runtime>(app: &AppHandle<R>, session: &RunSession) {
    let payload = RunArtifactsPayload {
        task_id: session.task_id.clone(),
        run_id: session.run_id.clone(),
        run_dir: session.run_dir_display(),
        artifacts: session.list_artifacts(),
    };
    let label = crate::task_run::window_label(&session.task_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.emit("run-artifacts-update", &payload);
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.emit("run-artifacts-update", &payload);
    } else {
        let _ = app.emit("run-artifacts-update", &payload);
    }
}

#[cfg(test)]
mod resume_dir_tests {
    use super::*;

    #[test]
    fn new_or_resume_keeps_same_dir_and_downloads() {
        let tid = format!("ut-{}", uuid::Uuid::new_v4());
        let s1 = RunSession::new_or_resume(&tid, "测试", false).unwrap();
        let dl = s1.run_dir.join("downloads");
        std::fs::write(dl.join("a.xlsx"), b"x").unwrap();
        let path1 = s1.run_dir.clone();
        drop(s1);

        let s2 = RunSession::new_or_resume(&tid, "测试", true).unwrap();
        assert_eq!(s2.run_dir, path1);
        assert!(s2.run_dir.join("downloads").join("a.xlsx").is_file());
        let _ = std::fs::remove_dir_all(&path1);
    }
}

fn render_assert_report_md(task_name: &str, entries: &[AssertReportEntry]) -> String {
    let pass = entries.iter().filter(|e| e.ok).count();
    let fail = entries.len() - pass;
    let mut out = String::new();
    out.push_str("# 断言报告\n\n");
    out.push_str(&format!("任务：{task_name}\n\n"));
    out.push_str(&format!(
        "时间：{}\n\n",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    ));
    out.push_str(&format!("汇总：**{pass} 通过** / **{fail} 失败**（共 {} 条）\n\n", entries.len()));
    out.push_str("| # | 结果 | 步骤 | 脚本 | 参数 | 说明 |\n");
    out.push_str("|---|------|------|------|------|------|\n");
    for (i, e) in entries.iter().enumerate() {
        let mark = if e.ok { "通过" } else { "失败" };
        let step = e
            .step_index
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".into());
        let args = if e.args.is_empty() {
            "—".into()
        } else {
            e.args.replace('|', "\\|")
        };
        let detail = if e.detail.is_empty() {
            "—".into()
        } else {
            e.detail.replace('|', "\\|").replace('\n', " ")
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            mark,
            step,
            e.script_name.replace('|', "\\|"),
            args,
            detail
        ));
    }
    out.push('\n');
    for (i, e) in entries.iter().enumerate() {
        let mark = if e.ok { "通过" } else { "失败" };
        out.push_str(&format!("## {}. [{}] {}\n\n", i + 1, mark, e.script_name));
        if let Some(s) = e.step_index {
            out.push_str(&format!("- 步骤：{s}\n"));
        }
        if !e.args.is_empty() {
            out.push_str(&format!("- 参数：`{}`\n", e.args));
        }
        if !e.detail.is_empty() {
            out.push_str(&format!("- 说明：{}\n", e.detail));
        }
        out.push('\n');
    }
    out
}

fn sanitize_folder_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(32) {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() {
        "task".into()
    } else {
        out
    }
}

/// 产物文档时间戳（与页面采集、AI 分析等命名一致）
pub fn document_time_stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// `报告.md` → `报告-20250902-134900.md`
pub fn stamp_document_filename(filename: &str) -> String {
    if let Some((stem, ext)) = filename.rsplit_once('.') {
        format!("{stem}-{}.{ext}", document_time_stamp())
    } else {
        format!("{filename}-{}", document_time_stamp())
    }
}

fn sanitize_filename(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(48) {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() {
        "file".into()
    } else {
        out
    }
}

/// 下载文件名：保留中文等 Unicode，仅去掉路径非法字符。
fn sanitize_download_filename_for_disk(s: &str) -> String {
    let base = Path::new(s)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(s);
    let mut out = String::new();
    for c in base.chars().take(120) {
        if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            continue;
        }
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c > '\u{007f}' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() {
        "download.xlsx".into()
    } else {
        out
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_download_filename_for_disk;

    #[test]
    fn keeps_chinese_in_download_filename() {
        let name = sanitize_download_filename_for_disk("页面结果导出_20260710132442.xlsx");
        assert_eq!(name, "页面结果导出_20260710132442.xlsx");
        let name2 = sanitize_download_filename_for_disk("考勤月报_科技事业部_2026-05-01.xlsx");
        assert_eq!(name2, "考勤月报_科技事业部_2026-05-01.xlsx");
        let name3 = sanitize_download_filename_for_disk("2026-06月报.xls");
        assert_eq!(name3, "2026-06月报.xls");
    }
}

fn ensure_extension(name: &str, kind: ArtifactKind) -> String {
    let n = sanitize_filename(name);
    if Path::new(&n).extension().is_some() {
        return n;
    }
    match kind {
        ArtifactKind::Screenshot => format!("{n}.png"),
        ArtifactKind::Document => format!("{n}.md"),
        ArtifactKind::Download => n,
        ArtifactKind::Other => n,
    }
}

fn is_incomplete_download(name: &str) -> bool {
    name.ends_with(".crdownload") || name.ends_with(".tmp") || name.starts_with('.')
}

fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let s = input.trim();
    let data = s
        .strip_prefix("data:")
        .and_then(|x| x.split_once(',').map(|(_, b)| b))
        .unwrap_or(s);
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("Base64 解码失败: {e}"))
}
