use crate::paths;
use crate::rhai_studio::preview;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const FOLDERS: &[&str] = &["downloads", "files"];

#[derive(Clone, Serialize)]
pub struct FileItem {
    pub name: String,
    pub size: u64,
    pub kind: String,
}

pub fn workspace_dir() -> PathBuf {
    paths::app_data_dir().join("rhai-studio")
}

pub fn ensure_workspace() -> Result<PathBuf, String> {
    let ws = workspace_dir();
    for kind in FOLDERS {
        fs::create_dir_all(ws.join(kind)).map_err(|e| e.to_string())?;
    }
    // templates 使用正式模板目录（通过 run_script 的 templates_dir 传入）
    Ok(ws)
}

pub fn safe_join(kind: &str, name: &str) -> Result<PathBuf, String> {
    if kind != "downloads" && kind != "files" && kind != "templates" {
        return Err("目录必须是 downloads / files / templates".into());
    }
    let name = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "非法文件名".to_string())?;
    if name.is_empty() || name.starts_with('.') || name.contains("..") {
        return Err("非法文件名".into());
    }
    if kind == "templates" {
        crate::templates::ensure_template_files()?;
        return Ok(crate::templates::templates_dir().join(name));
    }
    let ws = ensure_workspace()?;
    Ok(ws.join(kind).join(name))
}

fn list_dir_kind(kind: &str) -> Result<Vec<FileItem>, String> {
    let dir = if kind == "templates" {
        crate::templates::ensure_template_files()?;
        crate::templates::templates_dir()
    } else if kind == "downloads" || kind == "files" {
        ensure_workspace()?.join(kind)
    } else {
        return Err("非法目录".into());
    };
    let mut items = Vec::new();
    if !dir.is_dir() {
        return Ok(items);
    }
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_file() {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
                .to_string();
            if name.starts_with('.') {
                continue;
            }
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            items.push(FileItem {
                name,
                size,
                kind: kind.to_string(),
            });
        }
    }
    Ok(items)
}

/// `attachments` = downloads ∪ files；`workspace` = 附件 ∪ 模板（控制台合并视图）
pub fn list_files(kind: &str) -> Result<Vec<FileItem>, String> {
    let mut items = match kind {
        "attachments" => {
            let mut v = list_dir_kind("downloads")?;
            v.extend(list_dir_kind("files")?);
            v
        }
        "workspace" | "all" => {
            let mut v = list_dir_kind("downloads")?;
            v.extend(list_dir_kind("files")?);
            v.extend(list_dir_kind("templates")?);
            v
        }
        "templates" | "downloads" | "files" => list_dir_kind(kind)?,
        _ => return Err("非法目录".into()),
    };
    items.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.kind.cmp(&b.kind)));
    Ok(items)
}

pub fn delete_file(kind: &str, name: &str) -> Result<(), String> {
    if kind == "templates" {
        return Err("模板请在「设置 → 模板」中管理，工作台不直接删除模板".into());
    }
    let path = safe_join(kind, name)?;
    if !path.is_file() {
        return Err("文件不存在".into());
    }
    fs::remove_file(path).map_err(|e| e.to_string())
}

pub fn add_files(kind: &str, paths: &[String]) -> Result<Vec<String>, String> {
    if kind != "downloads" && kind != "files" {
        return Err("只能向 downloads / files 添加附件".into());
    }
    let mut added = Vec::new();
    for src in paths {
        let src_path = PathBuf::from(src);
        if !src_path.is_file() {
            continue;
        }
        let name = src_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| "非法文件名".to_string())?;
        let dest = safe_join(kind, name)?;
        fs::copy(&src_path, &dest).map_err(|e| format!("复制失败 {name}: {e}"))?;
        added.push(name.to_string());
    }
    Ok(added)
}

pub fn attachment_context() -> String {
    let mut lines = vec!["【附件预览】脚本工作台沙箱中的文件：".to_string()];
    let mut any = false;
    for kind in ["downloads", "templates", "files"] {
        let Ok(items) = list_files(kind) else {
            continue;
        };
        for it in items {
            any = true;
            let Ok(path) = safe_join(kind, &it.name) else {
                continue;
            };
            lines.push(format!("\n## {kind}/{}  ({} bytes)", it.name, it.size));
            let prev = preview::preview_path(&path);
            if let Some(rows) = prev.rows {
                if let Some(header) = rows.first() {
                    lines.push(format!("表头: {}", header.join(" | ")));
                }
                for row in rows.iter().skip(1) {
                    lines.push(format!("样例: {}", row.join(" | ")));
                }
            } else if let Some(text) = prev.text {
                let t: String = text.chars().take(1500).collect();
                lines.push(t);
            } else if let Some(err) = prev.error {
                lines.push(err);
            }
        }
    }
    if !any {
        lines.push("（当前没有附件。若用户描述了表结构，可先 ask 确认列名。）".into());
    }
    lines.join("\n")
}
