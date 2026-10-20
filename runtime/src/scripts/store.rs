use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptDef {
    pub id: String,
    /// 脚本名称（「脚本」指令后的标识，如「合并考勤数据」）
    pub name: String,
    pub description: String,
    pub builtin: bool,
    /// Rhai 脚本正文
    pub script: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScriptDb {
    scripts: Vec<ScriptDef>,
}

fn save_db(db: &ScriptDb) -> Result<(), String> {
    std::fs::create_dir_all(paths::app_data_dir()).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    std::fs::write(paths::scripts_file(), data).map_err(|e| e.to_string())
}

fn merge_missing_builtins(scripts: &mut Vec<ScriptDef>) -> bool {
    let mut changed = false;
    for def in super::builtin::default_scripts() {
        if !scripts.iter().any(|s| s.id == def.id) {
            scripts.push(def);
            changed = true;
        }
    }
    changed
}

/// 修复/同步内置「合并考勤数据」脚本正文
fn refresh_builtin_merge_attendance(scripts: &mut Vec<ScriptDef>) -> bool {
    let mut changed = false;
    if let Some(s) = scripts
        .iter_mut()
        .find(|c| c.id == "builtin_merge_attendance" && c.builtin)
    {
        let embedded = crate::rhai_runtime::FILL_ATTENDANCE_SCRIPT;
        if s.script != embedded
            && (s.script.contains("(?!")
                || s.script.contains(r#"find_file("files", "^.*考勤汇总"#)
                || s.script.contains("copy_file(template")
                || s.script.contains(r#"find_template("^.*考勤汇总"#)
                || s.script.contains(r#"find_file("docs""#)
                || s.script.contains(r#"find_file("result""#)
                || !s.script.contains("track_output")
                || !s.script.contains("load_hr_info")
                || !s.script.contains("fill_header")
                || s.script.contains("if org == department")
                || !s.script.contains("excel_serial_ymd")
                || !s.script.contains("find_ot_cols")
                || !s.script.contains("一次插入全部行")
                || !s.script.contains("merge_employees")
                || !s.script.contains("load_frontline_people")
                || !s.script.contains("default_frontline_daily")
                || !s.script.contains("find_file_optional")
                || !s.script.contains("find_template_optional")
                || s.script.contains(r#""出勤": """#)
                || !s.script.contains(r#""出勤": "√""#)
                || !s.script.contains("未找到考勤月报，本次不生成考勤记录表")
                || !s.script.contains("load_leave_register")
                || !s.script.contains("load_ot_schedule")
                || !s.script.contains("load_daily_status")
                || !s.script.contains("resolve_day_symbol")
                || !s.script.contains("按请假登记 / 加班排班 / 日报刷新日历格")
                || !s.script.contains("（非全）")
                || !s.script.contains("name_dept_key")
                || !s.script.contains("normalize_name")
                || !s.script.contains(r#""产假": "Q""#)
                || !s.script.contains("build_other_leave_remark")
                || !s.script.contains("ws_remove_columns")
                || !s.script.contains(r#""外出": "√""#)
                || !s.script.contains(r#""出差": "√""#)
                || !s.script.contains("禁止姓名兜底")
                || s.script.contains("请再执行「脚本日报补充考勤」"))
        {
            s.script = embedded.to_string();
            changed = true;
        }
        if s.name != "合并考勤数据" {
            s.name = "合并考勤数据".into();
            changed = true;
        }
        let want_desc = super::builtin::merge_attendance_description();
        if s.description != want_desc {
            s.description = want_desc.into();
            changed = true;
        }
    }
    changed
}

/// 修复/同步内置「考勤数据对比」脚本正文
fn refresh_builtin_compare_attendance(scripts: &mut Vec<ScriptDef>) -> bool {
    let mut changed = false;
    if let Some(s) = scripts
        .iter_mut()
        .find(|c| c.id == "builtin_compare_attendance" && c.builtin)
    {
        let embedded = crate::rhai_runtime::COMPARE_ATTENDANCE_SCRIPT;
        if s.script != embedded
            && (s.script.is_empty()
                || !s.script.contains("extract_zip")
                || !s.script.contains("sum_attendance_ot_hours")
                || !s.script.contains("read_ot_summary_total")
                || !s.script.contains("考勤数据对比审核意见"))
        {
            s.script = embedded.to_string();
            changed = true;
        }
        if s.name != "考勤数据对比" {
            s.name = "考勤数据对比".into();
            changed = true;
        }
        let want_desc = super::builtin::compare_attendance_description();
        if s.description != want_desc {
            s.description = want_desc.into();
            changed = true;
        }
    }
    changed
}

/// 同步内置「OCR验证道路清扫」临时脚本正文
fn refresh_builtin_ocr_road_clean_classify(scripts: &mut Vec<ScriptDef>) -> bool {
    let mut changed = false;
    if let Some(s) = scripts
        .iter_mut()
        .find(|c| c.id == "builtin_ocr_road_clean_classify" && c.builtin)
    {
        let embedded = crate::rhai_runtime::OCR_ROAD_CLEAN_CLASSIFY_SCRIPT;
        if s.script != embedded {
            s.script = embedded.to_string();
            changed = true;
        }
        if s.name != "OCR验证道路清扫" {
            s.name = "OCR验证道路清扫".into();
            changed = true;
        }
        let want_desc = super::builtin::ocr_road_clean_classify_description();
        if s.description != want_desc {
            s.description = want_desc.into();
            changed = true;
        }
    }
    changed
}

fn refresh_builtin_scene_pair_score(scripts: &mut Vec<ScriptDef>) -> bool {
    let mut changed = false;
    if let Some(s) = scripts
        .iter_mut()
        .find(|c| c.id == "builtin_scene_pair_score" && c.builtin)
    {
        let embedded = crate::rhai_runtime::SCENE_PAIR_SCORE_SCRIPT;
        if s.script != embedded {
            s.script = embedded.to_string();
            changed = true;
        }
        if s.name != "场景配对打分" {
            s.name = "场景配对打分".into();
            changed = true;
        }
        let want_desc = super::builtin::scene_pair_score_description();
        if s.description != want_desc {
            s.description = want_desc.into();
            changed = true;
        }
    }
    changed
}

/// 移除已废弃的内置脚本（含日报拆分版、旧「合并考勤数据含日报」、网球场地样例、微信图片监控）
fn remove_obsolete_builtin_scripts(scripts: &mut Vec<ScriptDef>) -> bool {
    let before = scripts.len();
    scripts.retain(|s| {
        s.id != "builtin_merge_attendance_daily"
            && s.id != "builtin_patch_attendance_daily"
            && s.id != "builtin_tennis_venue"
            && s.id != "builtin_watch_wechat_images"
            && s.id != "builtin_find_wechat_group"
    });
    scripts.len() != before
}

/// 从旧 commands.json 中迁移「合并考勤数据」到脚本库（保留用户改过的正文）
fn migrate_from_legacy_command(scripts: &mut Vec<ScriptDef>) -> bool {
    let cmd_path = paths::commands_file();
    if !cmd_path.exists() {
        return false;
    }
    let Ok(data) = std::fs::read_to_string(&cmd_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else {
        return false;
    };
    let Some(arr) = v.get("commands").and_then(|x| x.as_array()) else {
        return false;
    };
    let Some(legacy) = arr.iter().find(|c| {
        c.get("id").and_then(|x| x.as_str()) == Some("builtin_merge_attendance")
    }) else {
        return false;
    };
    let legacy_script = legacy
        .get("script")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if legacy_script.is_empty() {
        return false;
    }

    if let Some(existing) = scripts
        .iter_mut()
        .find(|s| s.id == "builtin_merge_attendance")
    {
        let default = crate::rhai_runtime::FILL_ATTENDANCE_SCRIPT;
        // 仅当脚本库仍是默认、而旧指令已定制时，迁入定制正文
        if existing.script == default && legacy_script != default {
            existing.script = legacy_script;
            existing.updated_at = Utc::now();
            return true;
        }
        return false;
    }

    scripts.push(ScriptDef {
        id: "builtin_merge_attendance".into(),
        name: legacy
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("合并考勤数据")
            .into(),
        description: legacy
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("用法：脚本合并考勤数据")
            .into(),
        builtin: true,
        script: legacy_script,
        updated_at: Utc::now(),
    });
    true
}

fn finalize_db(mut db: ScriptDb) -> Result<ScriptDb, String> {
    let mut changed = false;
    if db.scripts.is_empty() {
        db.scripts = super::builtin::default_scripts();
        changed = true;
    } else {
        if merge_missing_builtins(&mut db.scripts) {
            changed = true;
        }
        if migrate_from_legacy_command(&mut db.scripts) {
            changed = true;
        }
        if refresh_builtin_merge_attendance(&mut db.scripts) {
            changed = true;
        }
        if refresh_builtin_compare_attendance(&mut db.scripts) {
            changed = true;
        }
        if refresh_builtin_ocr_road_clean_classify(&mut db.scripts) {
            changed = true;
        }
        if refresh_builtin_scene_pair_score(&mut db.scripts) {
            changed = true;
        }
        if super::builtin::refresh_example_builtins(&mut db.scripts) {
            changed = true;
        }
        if remove_obsolete_builtin_scripts(&mut db.scripts) {
            changed = true;
        }
    }
    if changed {
        save_db(&db)?;
    }
    Ok(db)
}

fn load_db() -> Result<ScriptDb, String> {
    let path = paths::scripts_file();
    if !path.exists() {
        let mut db = ScriptDb {
            scripts: super::builtin::default_scripts(),
        };
        let _ = migrate_from_legacy_command(&mut db.scripts);
        save_db(&db)?;
        return Ok(db);
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    match serde_json::from_str::<ScriptDb>(&data) {
        Ok(db) => finalize_db(db),
        Err(e) => {
            let bak = path.with_extension("json.bak");
            let _ = std::fs::copy(&path, &bak);
            eprintln!("scripts.json 已损坏，已恢复内置脚本（备份 scripts.json.bak）。原错误: {e}");
            let db = ScriptDb {
                scripts: super::builtin::default_scripts(),
            };
            save_db(&db)?;
            Ok(db)
        }
    }
}

pub fn list_scripts() -> Result<Vec<ScriptDef>, String> {
    let mut db = load_db()?;
    db.scripts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(db.scripts)
}

pub fn get_script(id: &str) -> Result<Option<ScriptDef>, String> {
    Ok(list_scripts()?.into_iter().find(|s| s.id == id))
}

pub fn get_by_name(name: &str) -> Result<Option<ScriptDef>, String> {
    let name = name.trim();
    Ok(list_scripts()?.into_iter().find(|s| s.name == name))
}

/// 解析「脚本」指令参数：最长脚本名匹配 + 剩余作为 ARGS
pub fn resolve_invocation(args: &str) -> Result<(ScriptDef, String), String> {
    let args = args.trim();
    if args.is_empty() {
        return Err("请指定脚本名称，如：脚本合并考勤数据".into());
    }
    let all = list_scripts()?;
    let mut best: Option<&ScriptDef> = None;
    for s in &all {
        if args == s.name || args.starts_with(&format!("{} ", s.name)) {
            if best.map(|b| b.name.chars().count()).unwrap_or(0) < s.name.chars().count() {
                best = Some(s);
            }
        }
    }
    let script = best
        .cloned()
        .ok_or_else(|| {
            let names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
            format!(
                "未找到脚本「{}」。已登记: {}",
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

pub fn save_script(mut script: ScriptDef) -> Result<ScriptDef, String> {
    if script.name.trim().is_empty() {
        return Err("脚本名称不能为空".into());
    }
    script.name = script.name.trim().to_string();
    script.updated_at = Utc::now();

    let mut db = load_db()?;
    let idx = db.scripts.iter().position(|s| s.id == script.id);
    if let Some(idx) = idx {
        let existing_builtin = db.scripts[idx].builtin;
        if existing_builtin {
            // 内置：允许改说明与脚本正文，名称保持
            script.name = db.scripts[idx].name.clone();
            script.builtin = true;
        } else if db
            .scripts
            .iter()
            .any(|s| s.id != script.id && s.name == script.name)
        {
            return Err(format!("脚本名称「{}」已存在", script.name));
        }
        db.scripts[idx] = script.clone();
    } else {
        if db.scripts.iter().any(|s| s.name == script.name) {
            return Err(format!("脚本名称「{}」已存在", script.name));
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

pub fn delete_script(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let s = db.scripts.iter().find(|s| s.id == id).ok_or("脚本不存在")?;
    if s.builtin {
        return Err("内置脚本不可删除，可编辑后保存或恢复默认".into());
    }
    db.scripts.retain(|s| s.id != id);
    save_db(&db)
}

pub fn reset_builtin_script(id: &str) -> Result<ScriptDef, String> {
    let def = super::builtin::default_scripts()
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("非内置脚本")?;
    save_script(def)
}

/// 前端从脚本接口拉取后传入的条目（用于运行时本地镜像）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScriptInput {
    pub script_id: String,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub remark: Option<String>,
}

/// 用云端脚本覆盖非内置条目，保留内置脚本供离线执行。
pub fn sync_remote_scripts(remote: Vec<RemoteScriptInput>) -> Result<Vec<ScriptDef>, String> {
    let mut db = load_db()?;
    db.scripts.retain(|s| s.builtin);
    let now = Utc::now();
    let mut seen = std::collections::HashSet::new();
    for r in remote {
        let id = r.script_id.trim().to_string();
        let name = r.name.trim().to_string();
        if id.is_empty() || name.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        db.scripts.push(ScriptDef {
            id,
            name,
            description: r.remark.unwrap_or_default(),
            builtin: false,
            script: r.content,
            updated_at: now,
        });
    }
    save_db(&db)?;
    list_scripts()
}
