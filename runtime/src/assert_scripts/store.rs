use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertScriptDef {
    pub id: String,
    /// 断言脚本名称（「断言」指令后的标识）
    pub name: String,
    pub description: String,
    pub builtin: bool,
    /// Rhai 断言脚本正文（浏览器 XPath / URL / 标题 API）
    pub script: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AssertScriptDb {
    scripts: Vec<AssertScriptDef>,
}

fn save_db(db: &AssertScriptDb) -> Result<(), String> {
    std::fs::create_dir_all(paths::app_data_dir()).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    std::fs::write(paths::assert_scripts_file(), data).map_err(|e| e.to_string())
}

fn merge_missing_builtins(scripts: &mut Vec<AssertScriptDef>) -> bool {
    let mut changed = false;
    for def in super::builtin::default_assert_scripts() {
        if !scripts.iter().any(|s| s.id == def.id) {
            scripts.push(def);
            changed = true;
        }
    }
    changed
}

fn finalize_db(mut db: AssertScriptDb) -> Result<AssertScriptDb, String> {
    let mut changed = false;
    if db.scripts.is_empty() {
        db.scripts = super::builtin::default_assert_scripts();
        changed = true;
    } else if merge_missing_builtins(&mut db.scripts) {
        changed = true;
    }
    if changed {
        save_db(&db)?;
    }
    Ok(db)
}

fn load_db() -> Result<AssertScriptDb, String> {
    let path = paths::assert_scripts_file();
    if !path.exists() {
        let db = AssertScriptDb {
            scripts: super::builtin::default_assert_scripts(),
        };
        save_db(&db)?;
        return Ok(db);
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    match serde_json::from_str::<AssertScriptDb>(&data) {
        Ok(db) => finalize_db(db),
        Err(e) => {
            let bak = path.with_extension("json.bak");
            let _ = std::fs::copy(&path, &bak);
            eprintln!(
                "assert_scripts.json 已损坏，已恢复内置断言脚本（备份 assert_scripts.json.bak）。原错误: {e}"
            );
            let db = AssertScriptDb {
                scripts: super::builtin::default_assert_scripts(),
            };
            save_db(&db)?;
            Ok(db)
        }
    }
}

pub fn list_assert_scripts() -> Result<Vec<AssertScriptDef>, String> {
    let mut db = load_db()?;
    db.scripts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(db.scripts)
}

pub fn get_assert_script(id: &str) -> Result<Option<AssertScriptDef>, String> {
    Ok(list_assert_scripts()?.into_iter().find(|s| s.id == id))
}

/// 解析「断言」指令参数：最长脚本名匹配 + 剩余作为 ARGS
pub fn resolve_invocation(args: &str) -> Result<(AssertScriptDef, String), String> {
    let args = args.trim();
    if args.is_empty() {
        return Err("请指定断言脚本名称，如：断言检查页面标题".into());
    }
    let all = list_assert_scripts()?;
    let mut best: Option<&AssertScriptDef> = None;
    for s in &all {
        if args == s.name || args.starts_with(&format!("{} ", s.name)) {
            if best.map(|b| b.name.chars().count()).unwrap_or(0) < s.name.chars().count() {
                best = Some(s);
            }
        }
    }
    let script = best.cloned().ok_or_else(|| {
        let names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
        format!(
            "未找到断言脚本「{}」。已登记: {}",
            args.split_whitespace().next().unwrap_or(args),
            if names.is_empty() {
                "（无）".into()
            } else {
                names.join("、")
            }
        )
    })?;
    let rest = args[script.name.len()..].trim().to_string();
    Ok((script, rest))
}

pub fn save_assert_script(mut script: AssertScriptDef) -> Result<AssertScriptDef, String> {
    if script.name.trim().is_empty() {
        return Err("断言脚本名称不能为空".into());
    }
    script.name = script.name.trim().to_string();
    script.updated_at = Utc::now();

    let mut db = load_db()?;
    let idx = db.scripts.iter().position(|s| s.id == script.id);
    if let Some(idx) = idx {
        let existing_builtin = db.scripts[idx].builtin;
        if existing_builtin {
            script.name = db.scripts[idx].name.clone();
            script.builtin = true;
        } else if db
            .scripts
            .iter()
            .any(|s| s.id != script.id && s.name == script.name)
        {
            return Err(format!("断言脚本名称「{}」已存在", script.name));
        }
        db.scripts[idx] = script.clone();
    } else {
        if db.scripts.iter().any(|s| s.name == script.name) {
            return Err(format!("断言脚本名称「{}」已存在", script.name));
        }
        if script.id.is_empty() {
            script.id = Uuid::new_v4().to_string();
        }
        script.builtin = false;
        db.scripts.push(script.clone());
    }
    save_db(&db)?;
    Ok(script)
}

pub fn delete_assert_script(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let s = db
        .scripts
        .iter()
        .find(|s| s.id == id)
        .ok_or("断言脚本不存在")?;
    if s.builtin {
        return Err("内置断言脚本不可删除，可编辑后保存或恢复默认".into());
    }
    db.scripts.retain(|s| s.id != id);
    save_db(&db)
}

pub fn reset_builtin_assert_script(id: &str) -> Result<AssertScriptDef, String> {
    let def = super::builtin::default_assert_scripts()
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("非内置断言脚本")?;
    save_assert_script(def)
}
