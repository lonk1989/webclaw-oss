mod ai_repair;
mod ai_analyze;
mod ark;
mod assert_runtime;
mod assert_scripts;
mod browser_context;
mod browser_memory;
mod browser_network;
mod browser_session;
mod captcha;
#[cfg(feature = "desktop")]
mod chat_history;
mod chrome;
mod client_download;
mod commands;
mod corp_skills;
mod cdp;
mod executor;
mod feishu;
mod feishu_push;
mod feishu_relay;
#[cfg(feature = "desktop")]
mod file_assoc;
mod human_bridge;
#[cfg(feature = "desktop")]
mod launch;
mod login_verify;
mod host_agent;
mod page_recovery;
mod page_probe;
mod page_collect;
mod print_pdf;
mod templates;
mod paths;
mod public_params;
mod recipe;
mod remote_job;
pub mod rhai_runtime;
#[cfg(feature = "desktop")]
mod rhai_studio;
mod run_artifacts;
mod run_relay;
#[cfg(feature = "desktop")]
mod run_relay_desktop;
mod run_sink;
mod run_user_log;
mod scripts;
#[cfg(feature = "desktop")]
mod scheduler;
#[cfg(feature = "desktop")]
mod schedule_floater;
mod settings;
mod task_file;
mod task_progress;
mod task_run;
#[cfg(feature = "desktop")]
mod tray;
#[cfg(feature = "desktop")]
mod updater;
mod web_systems;
pub mod lite;
pub mod lite_probe;
pub mod lite_recipes;
pub mod worker;
#[cfg(not(feature = "desktop"))]
mod tauri_stub;
mod ui_host;

use chrono::Local;
use crate::task_run::{TaskControl, TaskRegistry};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct AppState {
    pub logs: Arc<Mutex<Vec<String>>>,
    pub chrome_child: Arc<Mutex<Option<std::process::Child>>>,
    pub cancel_flag: Arc<AtomicBool>,
    pub ai_stream_cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub task_registry: Arc<TaskRegistry>,
    /// å½å CDP éççæµè§å¨æ ç­¾ï¼é¦é¡µ main æåå°ä»»å¡ï¼
    pub active_browser: browser_session::ActiveBrowserSlot,
    /// é¦é¡µäº¤äºæ¨¡å¼ä¸æ¬¡éççæ ç­¾ï¼ä¸æ¬¡æ§è¡ä¼åå¤ç¨
    pub last_main_target: Arc<Mutex<Option<String>>>,
    /// ä¸æ¬¡å·²ç¥æµè§å¨æ ç­¾å¿«ç§ï¼ä»»å¡ç»æåä»ä¿çï¼ä¾ AI ä¸ä¸æï¼
    pub last_browser_snapshot: browser_context::BrowserSnapshotSlot,
    /// éªè¯ç å¼¹çªç­å¾ç¨æ·è¾å¥
    pub captcha_waiters: captcha::CaptchaWaiters,
}

#[derive(Clone)]
pub struct LaunchMeta {
    pub started_with_task_files: Arc<AtomicBool>,
}

impl LaunchMeta {
    pub fn new() -> Self {
        Self {
            started_with_task_files: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl AppState {
    pub fn push_log(&self, line: impl Into<String>) {
        let ts = Local::now().format("%H:%M:%S%.3f");
        let msg = format!("[{ts}] {}", line.into());
        self.logs.lock().push(msg);
    }
}

#[cfg(feature = "desktop")]
include!("desktop_app.rs");

#[cfg(not(feature = "desktop"))]
pub fn run() {
    eprintln!("webclaw æ¡é¢ç«¯è¯·ç¨ --features desktop æå»ºï¼ä¼ä¼´ç©ºé´è¯·è¿è¡ webclaw-worker");
    std::process::exit(2);
}
