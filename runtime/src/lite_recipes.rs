//! Local `.webclaw` recipe library under `WEBCLAW_HOME/recipes` (CLI fallback).
//!
//! DSH prefers the workspace `recipes/` directory; this module serves lite CLI
//! and path resolution for `run --recipe`.

use serde_json::json;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `<WEBCLAW_HOME>/recipes` — created by `ensure_lite_env`.
pub fn recipes_dir() -> Result<PathBuf, String> {
    let home = std::env::var("WEBCLAW_HOME").map_err(|_| "WEBCLAW_HOME 未设置".to_string())?;
    let dir = PathBuf::from(home).join("recipes");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 recipes 目录失败: {e}"))?;
    Ok(dir)
}

/// Sanitize a recipe stem for a safe filename (no path separators).
pub fn safe_recipe_name(raw: &str) -> Result<String, String> {
    let s: String = raw
        .trim()
        .trim_end_matches(".webclaw")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let s = s.trim_matches(|c| c == '.' || c == '-' || c == '_').to_string();
    if s.is_empty() {
        return Err("菜谱名称无效".into());
    }
    Ok(s)
}

/// Resolve a recipe path for `run --recipe`: absolute/existing first, then
/// `WEBCLAW_HOME/recipes/<name>.webclaw`, then as given.
pub fn resolve_recipe_path(input: &Path) -> PathBuf {
    if input.is_absolute() && input.is_file() {
        return input.to_path_buf();
    }
    if input.is_file() {
        return input.to_path_buf();
    }
    if let Ok(dir) = recipes_dir() {
        let name = input
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !name.is_empty() {
            let mut cand = dir.join(name);
            if !cand.extension().is_some_and(|e| e == "webclaw") {
                cand = dir.join(format!(
                    "{}.webclaw",
                    Path::new(name)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(name)
                ));
            }
            if cand.is_file() {
                return cand;
            }
            // Bare stem: recipes/foo.webclaw
            let stem = input
                .file_stem()
                .and_then(|s| s.to_str())
                .or_else(|| input.to_str())
                .unwrap_or("");
            if !stem.is_empty() {
                let p = dir.join(format!("{stem}.webclaw"));
                if p.is_file() {
                    return p;
                }
            }
        }
    }
    input.to_path_buf()
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

fn title_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix('#') {
            let title = rest.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
        break;
    }
    None
}

/// Save UTF-8 recipe body to `WEBCLAW_HOME/recipes/<name>.webclaw`.
pub fn save_recipe_file(name: &str, body: &str, title_line: Option<&str>) -> Result<PathBuf, String> {
    let stem = safe_recipe_name(name)?;
    let mut content = strip_bom(body).trim().to_string();
    if content.is_empty() {
        return Err("菜谱正文不能为空".into());
    }
    if let Some(t) = title_line.map(str::trim).filter(|s| !s.is_empty()) {
        if !content.starts_with('#') {
            content = format!("# {t}\n{content}");
        }
    }
    if !content.ends_with('\n') {
        content.push('\n');
    }
    let path = recipes_dir()?.join(format!("{stem}.webclaw"));
    std::fs::write(&path, content.as_bytes()).map_err(|e| format!("写入失败: {e}"))?;
    Ok(path)
}

/// List `.webclaw` files in the recipes directory.
pub fn list_recipe_files() -> Result<Vec<serde_json::Value>, String> {
    let dir = recipes_dir()?;
    let mut items = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("读取 recipes 失败: {e}"))?;
    for ent in entries {
        let ent = ent.map_err(|e| format!("读取目录项失败: {e}"))?;
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext != "webclaw" {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let meta = ent.metadata().ok();
        let mtime_ms = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        let bytes = meta.map(|m| m.len()).unwrap_or(0);
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        let title = title_from_body(strip_bom(&body));
        items.push(json!({
            "name": name,
            "path": path.display().to_string(),
            "bytes": bytes,
            "mtimeMs": mtime_ms,
            "title": title,
        }));
    }
    items.sort_by(|a, b| {
        let na = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let nb = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
        na.cmp(nb)
    });
    Ok(items)
}

fn read_stdin() -> Result<String, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// CLI: `webclaw-lite recipe save|list …`
pub fn recipe_cli(args: &[String]) -> Result<(), String> {
    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let items = list_recipe_files()?;
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "type": "recipes",
                    "dir": recipes_dir()?.display().to_string(),
                    "items": items,
                }))
                .unwrap_or_default()
            );
            Ok(())
        }
        "save" => {
            let mut name = None;
            let mut file = None;
            let mut title = None;
            let mut stdin = false;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--name" | "-n" => {
                        i += 1;
                        name = Some(args.get(i).ok_or("--name 需要名称")?.clone());
                    }
                    "--file" | "-f" => {
                        i += 1;
                        file = Some(PathBuf::from(args.get(i).ok_or("--file 需要路径")?));
                    }
                    "--title" | "-t" => {
                        i += 1;
                        title = Some(args.get(i).ok_or("--title 需要标题")?.clone());
                    }
                    "--stdin" => stdin = true,
                    "-h" | "--help" => {
                        eprintln!(
                            "webclaw-lite recipe save --name <stem> (--stdin | --file <path>) [--title 标题]\nwebclaw-lite recipe list"
                        );
                        return Ok(());
                    }
                    other => return Err(format!("未知参数: {other}")),
                }
                i += 1;
            }
            let name = name.ok_or("请指定 --name")?;
            let body = if stdin {
                read_stdin()?
            } else if let Some(p) = file {
                std::fs::read_to_string(&p).map_err(|e| format!("读文件失败: {e}"))?
            } else {
                return Err("请指定 --stdin 或 --file".into());
            };
            let path = save_recipe_file(&name, &body, title.as_deref())?;
            let meta = std::fs::metadata(&path).ok();
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "type": "recipe_saved",
                    "name": safe_recipe_name(&name)?,
                    "path": path.display().to_string(),
                    "bytes": meta.map(|m| m.len()).unwrap_or(0),
                }))
                .unwrap_or_default()
            );
            Ok(())
        }
        "-h" | "--help" | "help" => {
            eprintln!(
                "webclaw-lite recipe — 本地菜谱库（WEBCLAW_HOME/recipes）

用法:
  webclaw-lite recipe save --name <stem> (--stdin | --file <path>) [--title 标题]
  webclaw-lite recipe list
"
            );
            Ok(())
        }
        other => Err(format!("未知子命令: {other}（可用 save、list）")),
    }
}
