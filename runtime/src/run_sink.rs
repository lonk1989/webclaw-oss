//! 运行事件下沉：桌面 UI / worker JSONL 共用。

use parking_lot::Mutex;
use serde::Serialize;
use std::io::{self, Write};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    RunStart {
        run_id: String,
        title: String,
    },
    StepStart {
        index: u32,
        total: u32,
        keyword: String,
        args: String,
    },
    StepOk {
        index: u32,
        total: u32,
        detail: String,
    },
    StepFail {
        index: u32,
        total: u32,
        error: String,
    },
    NeedHuman {
        kind: String,
        prompt_id: String,
        hint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        image_path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_file: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        inbox_image: Option<String>,
        /// 远程执行经中继回桌面弹窗；JSONL 下沉时会剥掉以免刷屏
        #[serde(skip_serializing_if = "Option::is_none")]
        image_base64: Option<String>,
    },
    NeedAccounts {
        systems: Vec<NeedAccountItem>,
    },
    Log {
        message: String,
    },
    Done {
        ok: u32,
        fail: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_dir: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeedAccountItem {
    pub id: String,
    pub name: String,
    pub url: String,
}

pub trait RunSink: Send + Sync {
    fn emit(&self, event: RunEvent);
}

/// 每行一条 JSON，写到 stdout（供 OpenClaw Skill 解析）。
pub struct JsonlRunSink;

impl RunSink for JsonlRunSink {
    fn emit(&self, event: RunEvent) {
        // stdout 给人看/给助手：不要塞大图 base64
        let event = match event {
            RunEvent::NeedHuman {
                kind,
                prompt_id,
                hint,
                image_path,
                reply_file,
                inbox_image,
                image_base64: _,
            } => RunEvent::NeedHuman {
                kind,
                prompt_id,
                hint,
                image_path,
                reply_file,
                inbox_image,
                image_base64: None,
            },
            other => other,
        };
        if let Ok(line) = serde_json::to_string(&event) {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

/// 同时写入多个 sink（如 JSONL + 运行中继）。
pub struct FanoutRunSink {
    pub sinks: Vec<Arc<dyn RunSink>>,
}

impl RunSink for FanoutRunSink {
    fn emit(&self, event: RunEvent) {
        for s in &self.sinks {
            s.emit(event.clone());
        }
    }
}

static WORKER_SINK: OnceLock<Mutex<Option<Arc<dyn RunSink>>>> = OnceLock::new();

fn sink_slot() -> &'static Mutex<Option<Arc<dyn RunSink>>> {
    WORKER_SINK.get_or_init(|| Mutex::new(None))
}

/// worker 进程启动时安装；桌面模式保持 None。
pub fn install_worker_sink(sink: Arc<dyn RunSink>) {
    *sink_slot().lock() = Some(sink);
}

pub fn clear_worker_sink() {
    *sink_slot().lock() = None;
}

pub fn worker_mode() -> bool {
    sink_slot().lock().is_some()
        || std::env::var("WEBCLAW_WORKER")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

pub fn emit_event(event: RunEvent) {
    if let Some(s) = sink_slot().lock().as_ref() {
        s.emit(event);
    }
}

pub fn emit_log(message: impl Into<String>) {
    let message = message.into();
    if lite_stdout_skips_log(&message) {
        return;
    }
    emit_event(RunEvent::Log {
        message,
    });
}

/// 轻客户端 stdout 只留给步骤与人机事件。内存采样、探针仍写入 run 目录日志文件。
fn lite_stdout_skips_log(message: &str) -> bool {
    if std::env::var_os("WEBCLAW_VERBOSE").is_some() {
        return false;
    }
    if std::env::var_os("WEBCLAW_LITE").is_none() {
        return false;
    }
    message.contains("内存")
        || message.contains("working set")
        || message.contains("WorkingSet")
        || message.contains("定时 #")
        || message.contains("探针")
        || message.contains("UA内存")
        || message.contains("taskkill")
        || message.contains("已启用浏览器内存")
}
