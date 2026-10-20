//! Local skill cache helpers for webclaw-lite (no enterprise session).
//!
//! `skills sync --from-dir <path>` imports `.webclaw` / `.txt` / `.md` files as named skills.
//! Remote corp-session sync is intentionally omitted from the OSS build.

use crate::recipe::{replace_skills_cache, SkillCacheEntry};
use std::path::{Path, PathBuf};

/// Display name for status — always absent in OSS (no corp login bridge).
pub fn signed_in_user() -> Option<String> {
    None
}

/// Import skills from a local directory into `skills-cache.json`.
pub async fn sync_skills_from_dir(dir: &Path) -> Result<u32, String> {
    if !dir.is_dir() {
        return Err(format!("目录不存在: {}", dir.display()));
    }
    let mut skills: Vec<SkillCacheEntry> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))?;
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
        if !matches!(ext.as_str(), "webclaw" | "txt" | "md") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取 {}: {e}", path.display()))?;
        let content = content.trim().to_string();
        if content.is_empty() {
            continue;
        }
        skills.push(SkillCacheEntry {
            id: name.clone(),
            name,
            content,
            status: Some(1),
        });
    }
    let n = skills.len() as u32;
    replace_skills_cache(skills)?;
    Ok(n)
}

/// Legacy name kept for call sites that expected remote sync — now an explicit error.
pub async fn sync_skills() -> Result<u32, String> {
    Err(
        "OSS 构建不支持远程技能同步。请使用: webclaw-lite skills sync --from-dir <目录>"
            .into(),
    )
}

pub fn list_cached_names() -> Result<Vec<serde_json::Value>, String> {
    let items = crate::recipe::list_skills_cache()?;
    Ok(items
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "enabled": s.status.unwrap_or(1) != 0,
            })
        })
        .collect())
}

/// Resolve optional `--from-dir` after `skills sync`.
pub fn parse_sync_from_dir(args: &[String]) -> Option<PathBuf> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--from-dir" {
            return args.get(i + 1).map(|s| PathBuf::from(s));
        }
        i += 1;
    }
    None
}
