//! 首页对话历史：按用户落盘到 `chat/sessions/<id>.json`，不再依赖 localStorage。

use crate::paths;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn chat_dir() -> PathBuf {
    let dir = paths::app_data_dir().join("chat");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn sessions_dir() -> PathBuf {
    let dir = chat_dir().join("sessions");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn index_path() -> PathBuf {
    chat_dir().join("index.json")
}

fn sanitize_session_id(raw: &str) -> String {
    let s: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if s.is_empty() {
        "unknown".into()
    } else {
        s
    }
}

fn session_path(id: &str) -> PathBuf {
    sessions_dir().join(format!("{}.json", sanitize_session_id(id)))
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let data = serde_json::to_string_pretty(value).map_err(|e| format!("序列化失败: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data.as_bytes()).map_err(|e| format!("写入临时文件失败: {e}"))?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("无法覆盖旧文件: {e}")
        })?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("落盘失败: {e}")
    })
}

fn read_json_file(path: &Path) -> Result<Value, String> {
    let data = fs::read_to_string(path).map_err(|e| format!("读取失败 {}: {e}", path.display()))?;
    serde_json::from_str(&data).map_err(|e| format!("解析失败 {}: {e}", path.display()))
}

fn session_id_of(session: &Value) -> Option<String> {
    session
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn msg_count(session: &Value) -> usize {
    session
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

fn index_entry_from_session(session: &Value) -> Option<Value> {
    let id = session_id_of(session)?;
    let title = session
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("对话")
        .to_string();
    let created_at = session
        .get("createdAt")
        .and_then(|v| v.as_u64())
        .or_else(|| session.get("createdAt").and_then(|v| v.as_i64()).map(|n| n as u64))
        .unwrap_or_else(now_ms);
    let mut entry = json!({
        "id": id,
        "title": title,
        "createdAt": created_at,
        "updatedAt": now_ms(),
        "msgCount": msg_count(session),
    });
    if let Some(obj) = entry.as_object_mut() {
        for key in ["runDir", "taskId", "path", "recipeParams", "artifacts"] {
            if let Some(v) = session.get(key) {
                if !v.is_null() {
                    obj.insert(key.to_string(), v.clone());
                }
            }
        }
    }
    Some(entry)
}

fn load_index() -> Vec<Value> {
    let path = index_path();
    if !path.is_file() {
        return Vec::new();
    }
    match read_json_file(&path) {
        Ok(Value::Array(arr)) => arr,
        _ => Vec::new(),
    }
}

fn save_index(entries: &[Value]) -> Result<(), String> {
    write_json_atomic(&index_path(), &Value::Array(entries.to_vec()))
}

fn rebuild_index_from_disk() -> Result<Vec<Value>, String> {
    let mut entries = Vec::new();
    let dir = sessions_dir();
    if !dir.is_dir() {
        save_index(&entries)?;
        return Ok(entries);
    }
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| format!("读取会话目录失败: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    files.sort();
    for path in files {
        if let Ok(session) = read_json_file(&path) {
            if msg_count(&session) == 0 {
                continue;
            }
            if let Some(entry) = index_entry_from_session(&session) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|a, b| {
        let ta = a.get("createdAt").and_then(|v| v.as_u64()).unwrap_or(0);
        let tb = b.get("createdAt").and_then(|v| v.as_u64()).unwrap_or(0);
        tb.cmp(&ta)
    });
    save_index(&entries)?;
    Ok(entries)
}

fn upsert_index_entry(session: &Value) -> Result<(), String> {
    let Some(entry) = index_entry_from_session(session) else {
        return Err("会话缺少 id".into());
    };
    let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut entries = load_index();
    entries.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some(id.as_str()));
    if msg_count(session) > 0 {
        entries.push(entry);
    }
    entries.sort_by(|a, b| {
        let ta = a.get("createdAt").and_then(|v| v.as_u64()).unwrap_or(0);
        let tb = b.get("createdAt").and_then(|v| v.as_u64()).unwrap_or(0);
        tb.cmp(&ta)
    });
    save_index(&entries)
}

/// 加载全部对话（含 messages）。启动时一次读入；单会话文件避免整库配额问题。
pub fn load_all_sessions() -> Result<Vec<Value>, String> {
    paths::ensure_user_scope_initialized();
    let dir = sessions_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let index = {
        let idx = load_index();
        if idx.is_empty() {
            rebuild_index_from_disk()?
        } else {
            idx
        }
    };

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in &index {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let path = session_path(id);
        if !path.is_file() {
            continue;
        }
        match read_json_file(&path) {
            Ok(session) if msg_count(&session) > 0 => {
                seen.insert(sanitize_session_id(id));
                out.push(session);
            }
            _ => {}
        }
    }

    // 索引遗漏的会话文件也挂上
    if let Ok(rd) = fs::read_dir(&dir) {
        for ent in rd.filter_map(|e| e.ok()) {
            let path = ent.path();
            if path
                .extension()
                .and_then(|x| x.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if seen.contains(&stem) {
                continue;
            }
            if let Ok(session) = read_json_file(&path) {
                if msg_count(&session) > 0 {
                    out.push(session);
                }
            }
        }
    }

    out.sort_by(|a, b| {
        let ta = a.get("createdAt").and_then(|v| v.as_u64()).unwrap_or(0);
        let tb = b.get("createdAt").and_then(|v| v.as_u64()).unwrap_or(0);
        tb.cmp(&ta)
    });
    Ok(out)
}

pub fn save_session(mut session: Value) -> Result<(), String> {
    paths::ensure_user_scope_initialized();
    let id = session_id_of(&session).ok_or_else(|| "会话缺少 id".to_string())?;
    let safe = sanitize_session_id(&id);
    if let Some(obj) = session.as_object_mut() {
        obj.insert("id".into(), json!(safe));
        if !obj.contains_key("createdAt") {
            obj.insert("createdAt".into(), json!(now_ms()));
        }
    }
    if msg_count(&session) == 0 {
        let path = session_path(&safe);
        if path.is_file() {
            let _ = fs::remove_file(&path);
        }
        return upsert_index_entry(&session);
    }
    write_json_atomic(&session_path(&safe), &session)?;
    upsert_index_entry(&session)
}

pub fn save_all_sessions(sessions: Vec<Value>) -> Result<(), String> {
    paths::ensure_user_scope_initialized();
    let mut kept_ids = std::collections::HashSet::new();
    for session in sessions {
        if session_id_of(&session).is_none() {
            continue;
        }
        if msg_count(&session) == 0 {
            continue;
        }
        let id = session_id_of(&session).unwrap();
        let safe = sanitize_session_id(&id);
        kept_ids.insert(safe.clone());
        let mut session = session;
        if let Some(obj) = session.as_object_mut() {
            obj.insert("id".into(), json!(safe));
        }
        write_json_atomic(&session_path(&safe), &session)?;
    }

    // 删除未保留的旧文件
    if let Ok(rd) = fs::read_dir(sessions_dir()) {
        for ent in rd.filter_map(|e| e.ok()) {
            let path = ent.path();
            if path
                .extension()
                .and_then(|x| x.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if !kept_ids.contains(&stem) {
                let _ = fs::remove_file(&path);
            }
        }
    }
    rebuild_index_from_disk()?;
    Ok(())
}

pub fn delete_sessions(ids: &[String]) -> Result<(), String> {
    paths::ensure_user_scope_initialized();
    for id in ids {
        let path = session_path(id);
        if path.is_file() {
            let _ = fs::remove_file(&path);
        }
    }
    let mut entries = load_index();
    entries.retain(|e| {
        let id = e.get("id").and_then(|v| v.as_str()).unwrap_or("");
        !ids.iter().any(|x| sanitize_session_id(x) == sanitize_session_id(id))
    });
    save_index(&entries)
}

/// 读取产物目录下 `executions/run-log.txt`（供 UI 回填执行气泡）。
pub fn read_run_log(run_dir: &str) -> Result<String, String> {
    let dir = PathBuf::from(run_dir.trim());
    if dir.as_os_str().is_empty() {
        return Err("run_dir 为空".into());
    }
    let path = dir.join("executions").join("run-log.txt");
    if !path.is_file() {
        return Err(format!("找不到 run-log.txt: {}", path.display()));
    }
    fs::read_to_string(&path).map_err(|e| format!("读取 run-log 失败: {e}"))
}

fn folder_mtime_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or_else(now_ms)
}

/// 扫描 `runs/sessions/*`：尚无聊天文件但有 run-log 的目录，合成历史会话（补救 localStorage 丢失）。
pub fn recover_sessions_from_runs() -> Result<Vec<Value>, String> {
    paths::ensure_user_scope_initialized();
    let runs_sessions = paths::runs_dir().join("sessions");
    if !runs_sessions.is_dir() {
        return Ok(Vec::new());
    }

    let existing = load_all_sessions()?;
    let mut have: std::collections::HashSet<String> = existing
        .iter()
        .filter_map(session_id_of)
        .map(|id| sanitize_session_id(&id))
        .collect();

    let mut created = Vec::new();
    let mut dirs: Vec<PathBuf> = fs::read_dir(&runs_sessions)
        .map_err(|e| format!("读取 runs/sessions 失败: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(folder_mtime_ms(p)));

    for dir in dirs.into_iter().take(80) {
        let id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let safe = sanitize_session_id(&id);
        if safe == "unknown" || have.contains(&safe) {
            continue;
        }
        let log_path = dir.join("executions").join("run-log.txt");
        if !log_path.is_file() {
            continue;
        }
        let raw = match fs::read_to_string(&log_path) {
            Ok(s) if s.trim().len() > 20 => s,
            _ => continue,
        };
        let title = raw
            .lines()
            .find(|l| l.contains("快捷指令执行开始") || l.contains("开始 ·"))
            .map(|l| {
                l.replace('═', "")
                    .replace("快捷指令执行开始", "")
                    .replace("开始 ·", "")
                    .trim()
                    .chars()
                    .take(24)
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "恢复的执行记录".into());

        let preview = {
            let mut out = String::new();
            for line in raw.lines().take(40) {
                if out.len() > 1200 {
                    break;
                }
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("\n…（完整执行日志已保存在产物目录 run-log.txt，打开对话时自动回填）");
            out
        };
        let created_at = folder_mtime_ms(&dir);
        let session = json!({
            "id": safe,
            "title": title,
            "createdAt": created_at,
            "runDir": dir.to_string_lossy(),
            "messages": [{
                "id": format!("recovered-{safe}"),
                "role": "assistant",
                "text": preview,
                "ts": created_at,
                "meta": "执行完成",
                "live": false,
                "runLog": true,
                "runLogExternal": true,
            }],
        });
        save_session(session.clone())?;
        have.insert(safe);
        created.push(session);
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_home<F: FnOnce()>(f: F) {
        let _g = LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "webclaw-chat-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("WEBCLAW_HOME", &dir);
        f();
        let _ = fs::remove_dir_all(&dir);
        std::env::remove_var("WEBCLAW_HOME");
    }

    #[test]
    fn save_load_roundtrip() {
        with_temp_home(|| {
            let session = json!({
                "id": "abc-123",
                "title": "测试",
                "createdAt": 1000,
                "messages": [{ "id": "m1", "role": "user", "text": "你好", "ts": 1001 }]
            });
            save_session(session).unwrap();
            let all = load_all_sessions().unwrap();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0]["title"], "测试");
            assert_eq!(all[0]["messages"][0]["text"], "你好");
        });
    }

    #[test]
    fn delete_removes_file() {
        with_temp_home(|| {
            save_session(json!({
                "id": "to-del",
                "title": "x",
                "createdAt": 1,
                "messages": [{ "role": "user", "text": "a", "ts": 1 }]
            }))
            .unwrap();
            delete_sessions(&["to-del".into()]).unwrap();
            assert!(load_all_sessions().unwrap().is_empty());
        });
    }
}
