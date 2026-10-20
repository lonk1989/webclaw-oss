//! 设置 → 脚本 →「脚本工作台」：多轮对话生成 Rhai，并在沙箱试跑。

mod chat;
mod files;
mod preview;
mod validate;

use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use crate::AppState as RootState;

pub use files::FileItem;
pub use preview::Preview;

pub struct StudioState {
    pub sessions: Mutex<HashMap<String, Vec<chat::ChatMessage>>>,
}

impl Default for StudioState {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioHealth {
    pub ok: bool,
    pub has_api_key: bool,
    pub model: String,
    pub workspace: String,
}

#[tauri::command]
pub fn rhai_studio_health() -> StudioHealth {
    let ws = files::workspace_dir();
    StudioHealth {
        ok: true,
        has_api_key: crate::ark::has_api_key(),
        model: crate::ark::endpoint_id().to_string(),
        workspace: ws.display().to_string(),
    }
}

#[tauri::command]
pub fn rhai_studio_list_files(kind: String) -> Result<Vec<FileItem>, String> {
    files::list_files(&kind)
}

#[tauri::command]
pub fn rhai_studio_delete_file(kind: String, name: String) -> Result<(), String> {
    files::delete_file(&kind, &name)
}

#[tauri::command]
pub fn rhai_studio_preview_file(kind: String, name: String) -> Result<Preview, String> {
    let path = files::safe_join(&kind, &name)?;
    Ok(preview::preview_path(&path))
}

#[tauri::command]
pub fn rhai_studio_pick_and_add_files(kind: String) -> Result<Vec<String>, String> {
    let picked = rfd::FileDialog::new()
        .set_title("添加附件到脚本工作台")
        .add_filter(
            "表格/文本",
            &["xls", "xlsx", "xlsm", "csv", "txt", "zip", "rhai"],
        )
        .pick_files()
        .unwrap_or_default();
    let paths: Vec<String> = picked
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    files::add_files(&kind, &paths)
}

#[tauri::command]
pub fn rhai_studio_open_workspace() -> Result<(), String> {
    let dir = files::ensure_workspace()?;
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResultDto {
    pub ok: bool,
    pub message: String,
    pub logs: Vec<String>,
    pub artifacts: Vec<ArtifactDto>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ArtifactDto {
    pub name: String,
    pub path: String,
}

#[tauri::command]
pub fn rhai_studio_validate_script(script: String) -> validate::ValidateResult {
    validate::validate_script(&script)
}

#[tauri::command]
pub fn rhai_studio_run_script(script: String, args: String) -> Result<RunResultDto, String> {
    if script.trim().is_empty() {
        return Err("脚本为空".into());
    }
    let check = validate::validate_script(&script);
    if !check.ok {
        return Ok(RunResultDto {
            ok: false,
            message: String::new(),
            logs: check.warnings,
            artifacts: vec![],
            error: Some(format!("校验未通过：\n{}", check.errors.join("\n"))),
        });
    }
    let ws = files::ensure_workspace()?;
    crate::templates::ensure_template_files()?;
    let templates = crate::templates::templates_dir();

    match crate::rhai_runtime::run_script(&script, &ws, &templates, &args, None) {
        Ok(r) => Ok(RunResultDto {
            ok: true,
            message: r.message,
            logs: r.logs,
            artifacts: r
                .artifacts
                .into_iter()
                .map(|a| ArtifactDto {
                    name: a.name,
                    path: a.path.to_string_lossy().into_owned(),
                })
                .collect(),
            error: None,
        }),
        Err(e) => Ok(RunResultDto {
            ok: false,
            message: String::new(),
            logs: vec![],
            artifacts: vec![],
            error: Some(e),
        }),
    }
}

#[tauri::command]
pub fn rhai_studio_reset_session(
    state: State<'_, StudioState>,
    session_id: String,
) -> Result<(), String> {
    state.sessions.lock().remove(&session_id);
    Ok(())
}

#[tauri::command]
pub async fn rhai_studio_chat_send(
    app: AppHandle,
    studio: State<'_, StudioState>,
    root: State<'_, RootState>,
    session_id: String,
    message: String,
) -> Result<String, String> {
    let message = message.trim().to_string();
    if message.is_empty() {
        return Err("消息为空".into());
    }
    let sid = if session_id.trim().is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        session_id
    };

    let attachment = files::attachment_context();
    let user_content = format!("{message}\n\n{attachment}");

    let history = {
        let mut map = studio.sessions.lock();
        let hist = map.entry(sid.clone()).or_default();
        hist.push(chat::ChatMessage {
            role: "user".into(),
            content: user_content,
        });
        hist.clone()
    };

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut map = root.ai_stream_cancels.lock();
        map.insert(format!("rhai-studio-{sid}"), cancel.clone());
    }

    let result = chat::stream_chat(&app, &sid, history, cancel.clone()).await;

    {
        let mut map = root.ai_stream_cancels.lock();
        map.remove(&format!("rhai-studio-{sid}"));
    }

    let result = result?;

    {
        let mut map = studio.sessions.lock();
        if let Some(hist) = map.get_mut(&sid) {
            if !result.full_text.trim().is_empty() {
                hist.push(chat::ChatMessage {
                    role: "assistant".into(),
                    content: result.full_text.clone(),
                });
            }
        }
    }

    let asks = result.asks.clone();
    if !asks.is_empty() {
        let _ = app.emit(
            "rhai-studio-event",
            serde_json::json!({ "sessionId": sid, "event": "asks", "data": { "questions": asks } }),
        );
    }
    if let Some(script) = result.script {
        if asks.is_empty() {
            let _ = app.emit(
                "rhai-studio-event",
                serde_json::json!({ "sessionId": sid, "event": "script", "data": script }),
            );
        }
    }
    let _ = app.emit(
        "rhai-studio-event",
        serde_json::json!({ "sessionId": sid, "event": "done", "data": { "text": result.full_text } }),
    );

    Ok(sid)
}

#[tauri::command]
pub fn rhai_studio_chat_abort(
    root: State<'_, RootState>,
    session_id: String,
) -> Result<(), String> {
    let key = format!("rhai-studio-{session_id}");
    if let Some(flag) = root.ai_stream_cancels.lock().get(&key) {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}
