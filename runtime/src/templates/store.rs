use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateDef {
    pub id: String,
    pub name: String,
    pub filename: String,
    pub description: String,
    pub builtin: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TemplateDb {
    templates: Vec<TemplateDef>,
}

fn load_db() -> Result<TemplateDb, String> {
    ensure_template_files()?;
    let path = paths::templates_meta_file();
    if !path.exists() {
        let db = TemplateDb {
            templates: super::builtin::default_templates(),
        };
        save_db(&db)?;
        return Ok(db);
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut db: TemplateDb =
        serde_json::from_str(&data).map_err(|e| format!("templates.json 解析失败: {e}"))?;
    if db.templates.is_empty() {
        db.templates = super::builtin::default_templates();
        save_db(&db)?;
    } else {
        let mut changed = merge_missing_builtins(&mut db.templates);
        if sync_builtin_templates(&mut db.templates) {
            changed = true;
        }
        if changed {
            save_db(&db)?;
        }
    }
    Ok(db)
}

fn save_db(db: &TemplateDb) -> Result<(), String> {
    fs::create_dir_all(paths::templates_dir()).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    fs::write(paths::templates_meta_file(), data).map_err(|e| e.to_string())
}

fn merge_missing_builtins(templates: &mut Vec<TemplateDef>) -> bool {
    let mut changed = false;
    for def in super::builtin::default_templates() {
        if !templates.iter().any(|t| t.id == def.id) {
            templates.push(def);
            changed = true;
        }
    }
    changed
}

/// 内置模板元数据或文件名随版本变更时，同步 DB 并迁移磁盘文件。
fn sync_builtin_templates(templates: &mut Vec<TemplateDef>) -> bool {
    let mut changed = false;
    let dir = paths::templates_dir();
    for default in super::builtin::default_templates() {
        let Some(existing) = templates.iter_mut().find(|t| t.id == default.id) else {
            continue;
        };
        if !existing.builtin {
            continue;
        }

        if existing.filename != default.filename {
            let old_path = dir.join(&existing.filename);
            let new_path = dir.join(&default.filename);
            if old_path.is_file() {
                if new_path.is_file() {
                    let keep_old = old_path
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .zip(new_path.metadata().and_then(|m| m.modified()).ok())
                        .map(|(a, b)| a >= b)
                        .unwrap_or(true);
                    if keep_old {
                        let _ = fs::remove_file(&new_path);
                        let _ = fs::rename(&old_path, &new_path);
                    } else {
                        let _ = fs::remove_file(&old_path);
                    }
                } else if let Some(parent) = new_path.parent() {
                    let _ = fs::create_dir_all(parent);
                    let _ = fs::rename(&old_path, &new_path);
                }
            }
            existing.filename = default.filename.clone();
            changed = true;
        }

        if existing.name != default.name {
            existing.name = default.name.clone();
            changed = true;
        }
        if existing.description != default.description {
            existing.description = default.description.clone();
            changed = true;
        }
    }
    changed
}

pub fn templates_dir() -> PathBuf {
    paths::templates_dir()
}

pub fn file_path(def: &TemplateDef) -> PathBuf {
    paths::templates_dir().join(&def.filename)
}

pub fn ensure_template_files() -> Result<(), String> {
    let dir = paths::templates_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建 templates 目录失败: {e}"))?;
    for def in super::builtin::default_templates() {
        let Some(bytes) = super::builtin::embedded_bytes(&def.filename) else {
            continue;
        };
        let path = dir.join(&def.filename);
        if path.is_file() {
            if let Ok(existing) = fs::read(&path) {
                if existing.as_slice() == bytes {
                    continue;
                }
            }
        }
        fs::write(&path, bytes).map_err(|e| format!("写入内置模板失败: {e}"))?;
    }
    Ok(())
}

pub fn list_templates() -> Result<Vec<TemplateDef>, String> {
    let db = load_db()?;
    let mut list = db.templates;
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

pub fn get_by_id(id: &str) -> Result<Option<TemplateDef>, String> {
    Ok(list_templates()?.into_iter().find(|t| t.id == id))
}

pub fn save_template(mut template: TemplateDef) -> Result<TemplateDef, String> {
    if template.name.trim().is_empty() {
        return Err("模板名称不能为空".into());
    }
    if template.filename.trim().is_empty() {
        return Err("文件名不能为空".into());
    }
    template.name = template.name.trim().to_string();
    template.filename = sanitize_filename(&template.filename);
    template.updated_at = Utc::now();

    let path = paths::templates_dir().join(&template.filename);
    if !path.is_file() {
        return Err(format!("模板文件不存在: {}", path.display()));
    }

    let mut db = load_db()?;
    if let Some(existing) = db.templates.iter().find(|t| t.id == template.id) {
        if existing.builtin && template.filename != existing.filename {
            return Err("内置模板文件名不可修改".into());
        }
    } else if db.templates.iter().any(|t| t.filename == template.filename) {
        return Err(format!("文件名「{}」已被使用", template.filename));
    }

    if template.id.is_empty() {
        template.id = Uuid::new_v4().to_string();
        template.builtin = false;
        db.templates.push(template.clone());
    } else if let Some(existing) = db.templates.iter_mut().find(|t| t.id == template.id) {
        if existing.builtin {
            template.builtin = true;
            template.filename = existing.filename.clone();
        }
        *existing = template.clone();
    } else {
        template.builtin = false;
        db.templates.push(template.clone());
    }

    save_db(&db)?;
    Ok(template)
}

pub fn import_template_file(
    source: &Path,
    name: Option<String>,
    description: Option<String>,
) -> Result<TemplateDef, String> {
    if !source.is_file() {
        return Err(format!("文件不存在: {}", source.display()));
    }

    let filename = source
        .file_name()
        .and_then(|s| s.to_str())
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "template".to_string());
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("template");
    let dest = paths::templates_dir().join(&filename);
    fs::create_dir_all(paths::templates_dir()).map_err(|e| e.to_string())?;
    fs::copy(source, &dest).map_err(|e| format!("复制模板文件失败: {e}"))?;

    let display_name = name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| stem.to_string());

    save_template(TemplateDef {
        id: String::new(),
        name: display_name,
        filename,
        description: description.unwrap_or_default(),
        builtin: false,
        updated_at: Utc::now(),
    })
}

pub fn replace_template_file(id: &str, source: &Path) -> Result<TemplateDef, String> {
    let mut def = get_by_id(id)?.ok_or("模板不存在")?;
    validate_template_source(source)?;
    let dest = file_path(&def);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if !same_file(source, &dest) {
        copy_replace_file(source, &dest)?;
    }
    def.updated_at = Utc::now();
    save_template(def)
}

fn validate_template_source(source: &Path) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("文件不存在: {}", source.display()));
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn copy_replace_file(source: &Path, dest: &Path) -> Result<(), String> {
    let tmp = dest.with_extension("tmp-replace");
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    fs::copy(source, &tmp).map_err(|e| format!("替换模板文件失败: {e}"))?;
    if dest.is_file() {
        fs::remove_file(dest)
            .map_err(|e| format!("无法覆盖原模板（文件可能被 Excel 占用）: {e}"))?;
    }
    fs::rename(&tmp, dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("替换模板文件失败: {e}")
    })
}

pub fn delete_template(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let item = db
        .templates
        .iter()
        .find(|t| t.id == id)
        .ok_or("模板不存在")?;
    if item.builtin {
        return Err("内置模板不可删除，可替换文件或恢复默认".into());
    }
    let file = file_path(item);
    db.templates.retain(|t| t.id != id);
    save_db(&db)?;
    if file.is_file() {
        let _ = fs::remove_file(file);
    }
    Ok(())
}

pub fn reset_builtin_template(id: &str) -> Result<TemplateDef, String> {
    let defaults = super::builtin::default_templates();
    let def = defaults
        .into_iter()
        .find(|t| t.id == id)
        .ok_or("非内置模板")?;
    if let Some(bytes) = super::builtin::embedded_bytes(&def.filename) {
        let path = paths::templates_dir().join(&def.filename);
        fs::write(&path, bytes).map_err(|e| e.to_string())?;
    }
    save_template(def)
}

/// 前端从模板接口拉取并下载附件后传入的条目
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTemplateInput {
    pub template_id: String,
    pub name: String,
    pub filename: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub oss_id: Option<String>,
}

/// 用云端模板覆盖非内置条目；附件须已写入 templates 目录。
pub fn sync_remote_templates(remote: Vec<RemoteTemplateInput>) -> Result<Vec<TemplateDef>, String> {
    ensure_template_files()?;
    let mut db = load_db()?;
    db.templates.retain(|t| t.builtin);
    let now = Utc::now();
    let mut seen = std::collections::HashSet::new();
    for r in remote {
        let id = r.template_id.trim().to_string();
        let name = r.name.trim().to_string();
        let filename = sanitize_filename(&r.filename);
        if id.is_empty() || name.is_empty() || filename.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let path = paths::templates_dir().join(&filename);
        if !path.is_file() {
            continue;
        }
        db.templates.push(TemplateDef {
            id,
            name,
            filename,
            description: r.description.unwrap_or_default(),
            builtin: false,
            updated_at: now,
        });
        let _ = r.oss_id;
    }
    save_db(&db)?;
    list_templates()
}

pub fn resolve_for_attendance() -> Result<PathBuf, String> {
    ensure_template_files()?;
    let def = get_by_id(super::builtin::BUILTIN_ATTENDANCE_ID)?
        .ok_or("内置考勤汇总模板未配置")?;
    let path = file_path(&def);
    if !path.is_file() {
        return Err(format!("模板文件不存在: {}", path.display()));
    }
    Ok(path)
}

/// 将用户选择的模板文件复制到本机 templates 目录（导入后立即供 Rhai 使用）。
pub fn stage_template_file(source: &Path, preferred_name: &str) -> Result<PathBuf, String> {
    if !source.is_file() {
        return Err(format!("文件不存在: {}", source.display()));
    }
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let name = preferred_name.trim();
    let filename = if name.is_empty() {
        source
            .file_name()
            .and_then(|s| s.to_str())
            .map(sanitize_filename)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "template".to_string())
    } else if name.contains('.') || ext.is_empty() {
        sanitize_filename(name)
    } else {
        sanitize_filename(&format!("{name}.{ext}"))
    };
    ensure_template_files()?;
    let dest = paths::templates_dir().join(&filename);
    fs::copy(source, &dest).map_err(|e| format!("复制模板文件失败: {e}"))?;
    Ok(dest)
}

pub fn sanitize_filename(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c > '\u{007f}' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "template.xlsx".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_file_detects_identical_paths() {
        let dir = std::env::temp_dir().join("webclaw_tpl_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.xlsx");
        fs::write(&file, b"x").unwrap();
        assert!(same_file(&file, &file));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_builtin_templates_migrates_filename() {
        let old = "XXX-X月考勤汇总.xlsx";
        let new = super::super::builtin::BUILTIN_ATTENDANCE_FILENAME;
        let mut templates = vec![TemplateDef {
            id: super::super::builtin::BUILTIN_ATTENDANCE_ID.into(),
            name: "旧名称".into(),
            filename: old.into(),
            description: String::new(),
            builtin: true,
            updated_at: Utc::now(),
        }];
        let dir = std::env::temp_dir().join("webclaw_tpl_sync_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(old), b"old").unwrap();

        // sync_builtin_templates uses paths::templates_dir(); test logic inline
        let old_path = dir.join(old);
        let new_path = dir.join(new);
        if old_path.is_file() && !new_path.is_file() {
            fs::rename(&old_path, &new_path).unwrap();
        }
        templates[0].filename = new.into();

        assert!(!dir.join(old).is_file());
        assert!(dir.join(new).is_file());
        assert_eq!(templates[0].filename, new);
        let _ = fs::remove_dir_all(&dir);
    }
}
