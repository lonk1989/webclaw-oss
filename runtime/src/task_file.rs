use crate::recipe;
use serde::Deserialize;
use std::path::{Path, PathBuf};

const HEADER_MARKER: &str = "---";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskChrome {
    #[default]
    Auto,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskUi {
    #[default]
    Background,
    Full,
    Minimal,
    Silent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskMode {
    #[default]
    Run,
    Edit,
}

#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub name: String,
    pub content: String,
    pub chrome: TaskChrome,
    pub ui: TaskUi,
    pub mode: TaskMode,
    pub source_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct RecipeRefFile {
    kind: String,
    id: String,
    #[serde(default)]
    name: Option<String>,
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

fn default_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("未命名任务")
        .to_string()
}

fn parse_header_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (k, v) = line.split_once(':')?;
    Some((k.trim().to_lowercase(), v.trim().to_string()))
}

fn apply_header_field(spec: &mut TaskSpec, key: &str, value: &str) {
    match key {
        "name" => spec.name = value.to_string(),
        "mode" => {
            spec.mode = match value.to_lowercase().as_str() {
                "edit" => TaskMode::Edit,
                _ => TaskMode::Run,
            };
        }
        "chrome" => {
            spec.chrome = match value.to_lowercase().as_str() {
                "skip" | "none" | "off" => TaskChrome::Skip,
                _ => TaskChrome::Auto,
            };
        }
        "ui" => {
            spec.ui = match value.to_lowercase().as_str() {
                "full" => TaskUi::Full,
                "minimal" | "min" => TaskUi::Minimal,
                "silent" | "quiet" => TaskUi::Silent,
                "background" | "bg" | "progress" => TaskUi::Background,
                _ => TaskUi::Background,
            };
        }
        _ => {}
    }
}

fn looks_like_header_line(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return true;
    }
    if t == HEADER_MARKER {
        return true;
    }
    let lower = t.to_lowercase();
    lower.starts_with("name:")
        || lower.starts_with("mode:")
        || lower.starts_with("chrome:")
        || lower.starts_with("ui:")
}

fn split_header_body(raw: &str) -> (Vec<String>, String) {
    let raw = strip_bom(raw);
    if let Some(idx) = raw.find(HEADER_MARKER) {
        let (head, rest) = raw.split_at(idx);
        let body = rest[HEADER_MARKER.len()..].trim_start_matches(['\r', '\n']).to_string();
        let header_lines: Vec<String> = head.lines().map(|l| l.to_string()).collect();
        return (header_lines, body);
    }

    let lines: Vec<&str> = raw.lines().collect();
    let mut header_end = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if !looks_like_header_line(line) {
            header_end = i;
            break;
        }
        if i == lines.len() - 1 {
            header_end = lines.len();
        }
    }

    if header_end == 0 {
        return (vec![], raw.trim().to_string());
    }

    let has_kv = lines[..header_end].iter().any(|l| parse_header_line(l).is_some());
    if !has_kv {
        return (vec![], raw.trim().to_string());
    }

    let body = lines[header_end..].join("\n").trim().to_string();
    (lines[..header_end].iter().map(|s| (*s).to_string()).collect(), body)
}

fn parse_inline_spec(path: &Path, raw: &str) -> Result<TaskSpec, String> {
    let mut spec = TaskSpec {
        name: default_name_from_path(path),
        content: String::new(),
        chrome: TaskChrome::Auto,
        ui: TaskUi::Background,
        mode: TaskMode::Run,
        source_path: Some(path.to_path_buf()),
    };

    let (header_lines, body) = split_header_body(raw);
    for line in &header_lines {
        if let Some((k, v)) = parse_header_line(line) {
            apply_header_field(&mut spec, &k, &v);
        }
    }

    spec.content = body.trim().to_string();
    if spec.content.is_empty() {
        return Err(format!("任务文件内容为空: {}", path.display()));
    }
    Ok(spec)
}

fn parse_recipe_ref(path: &Path, doc: RecipeRefFile) -> Result<TaskSpec, String> {
    if doc.kind != "recipe-ref" {
        return Err(format!("不支持的任务类型: {}", doc.kind));
    }
    let recipe = recipe::get_recipe(&doc.id)?
        .ok_or_else(|| format!("引用的快捷指令不存在: {}", doc.id))?;
    Ok(TaskSpec {
        name: doc.name.unwrap_or_else(|| recipe.name.clone()),
        content: recipe.content,
        chrome: TaskChrome::Auto,
        ui: TaskUi::Background,
        mode: TaskMode::Run,
        source_path: Some(path.to_path_buf()),
    })
}

pub fn load_task_file(path: &Path) -> Result<TaskSpec, String> {
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("webclaw")) != Some(true)
    {
        return Err(format!("不是 .webclaw 任务文件: {}", path.display()));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("读取任务文件失败: {e}"))?;
    let trimmed = strip_bom(raw.trim());
    if trimmed.starts_with('{') {
        let doc: RecipeRefFile =
            serde_json::from_str(trimmed).map_err(|e| format!("任务文件 JSON 无效: {e}"))?;
        return parse_recipe_ref(path, doc);
    }
    parse_inline_spec(path, trimmed)
}

pub fn format_task_file(_name: &str, content: &str) -> String {
    format!("{}\n", content.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_content() {
        let dir = std::env::temp_dir().join("webclaw_test_plain.webclaw");
        std::fs::write(&dir, "打开https://example.com，点击登录").unwrap();
        let spec = load_task_file(&dir).unwrap();
        assert_eq!(spec.content, "打开https://example.com，点击登录");
        assert_eq!(spec.mode, TaskMode::Run);
        let _ = std::fs::remove_file(&dir);
    }
}
