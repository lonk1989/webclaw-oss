use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::fs;
use std::sync::Mutex;
use uuid::Uuid;

thread_local! {
    static SKILLS_CACHE_LOCK_HELD: Cell<bool> = const { Cell::new(false) };
}

/// 串行化 skills-cache.json 读写；同线程可重入（便于测试包住「写入+解析」）。
fn with_skills_cache_lock<R>(f: impl FnOnce() -> R) -> R {
    SKILLS_CACHE_LOCK_HELD.with(|held| {
        if held.get() {
            return f();
        }
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        held.set(true);
        let out = f();
        held.set(false);
        out
    })
}

/// 在独占锁内执行（测试用：避免并行用例互相清空 skills-cache.json）
pub fn with_skills_cache_exclusive<R>(f: impl FnOnce() -> R) -> R {
    with_skills_cache_lock(f)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub id: String,
    pub name: String,
    pub content: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeSummary {
    pub id: String,
    pub name: String,
    pub preview: String,
    pub content: String,
    pub updated_at: DateTime<Utc>,
}

fn recipe_preview(content: &str, max_chars: usize) -> String {
    let flat: String = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("，");
    let flat = flat.trim();
    if flat.chars().count() <= max_chars {
        flat.to_string()
    } else {
        format!(
            "{}…",
            flat.chars().take(max_chars).collect::<String>()
        )
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecipeDb {
    recipes: Vec<Recipe>,
}

fn merge_missing_builtins(db: &mut RecipeDb) -> bool {
    let mut changed = false;
    for def in super::builtin::default_recipes() {
        if db.recipes.iter().any(|r| r.id == def.id) {
            continue;
        }
        db.recipes.push(def);
        changed = true;
    }
    changed
}

/// 合并考勤已迁至「脚本」指令：旧写法「合并考勤数据」→「脚本合并考勤数据」
fn migrate_merge_attendance_keyword(db: &mut RecipeDb) -> bool {
    let mut changed = false;
    for r in &mut db.recipes {
        // 占位避免把已迁移的「脚本合并考勤数据」再加一层「脚本」
        let mut next = r.content.replace("脚本合并考勤数据", "\u{E000}");
        next = next.replace("合并考勤数据", "脚本合并考勤数据");
        next = next.replace('\u{E000}', "脚本合并考勤数据");
        if next != r.content {
            r.content = next;
            changed = true;
        }
    }
    changed
}

/// 输入语法迁移：输入内容到字段 → 输入字段=内容
fn migrate_input_field_syntax(content: &str) -> String {
    if !content.contains('到') {
        return content.to_string();
    }
    let Ok(steps) = super::parser::parse_recipe_steps(content) else {
        return content.to_string();
    };
    let mut changed = false;
    let new_parts: Vec<String> = steps
        .iter()
        .map(|s| {
            if s.keyword != "输入" || !s.args.contains('到') {
                return format!("{}{}", s.keyword, s.args);
            }
            let (text, field) = crate::commands::parse_input_step_args(&s.args);
            if field.is_empty() {
                return format!("{}{}", s.keyword, s.args);
            }
            changed = true;
            if text.is_empty() {
                format!("输入{field}=")
            } else {
                format!("输入{field}={text}")
            }
        })
        .collect();
    if changed {
        new_parts.join("，")
    } else {
        content.to_string()
    }
}

fn migrate_input_field_syntax_db(db: &mut RecipeDb) -> bool {
    let mut changed = false;
    for r in &mut db.recipes {
        let next = migrate_input_field_syntax(&r.content);
        if next != r.content {
            r.content = next;
            changed = true;
        }
    }
    changed
}

/// 容忍文件尾部垃圾（曾出现 recipes.json 被截断拼接后 from_str 报 trailing characters）
fn parse_json_first<T: for<'de> Deserialize<'de>>(data: &str, label: &str) -> Result<T, String> {
    let trimmed = data.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(format!("{label}: 文件为空"));
    }
    let mut de = serde_json::Deserializer::from_str(trimmed);
    T::deserialize(&mut de).map_err(|e| format!("{label}: {e}"))
}

fn atomic_write_json(path: &std::path::Path, data: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data).map_err(|e| e.to_string())?;
    if let Err(e) = fs::rename(&tmp, path) {
        // Windows 上目标存在时 rename 可能失败：回退直写
        let _ = fs::remove_file(&tmp);
        fs::write(path, data).map_err(|e2| format!("写入失败: {e}; 回退直写: {e2}"))?;
    }
    Ok(())
}

fn load_db() -> Result<RecipeDb, String> {
    let path = paths::recipes_file();
    if !path.exists() {
        let db = RecipeDb {
            recipes: super::builtin::default_recipes(),
        };
        save_db(&db)?;
        return Ok(db);
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut db: RecipeDb = parse_json_first(&data, "快捷指令文件解析失败")?;
    // 若原文有 trailing 垃圾，立刻重写干净文件
    let mut changed = serde_json::from_str::<RecipeDb>(&data).is_err();
    if db.recipes.is_empty() {
        db.recipes = super::builtin::default_recipes();
        changed = true;
    } else {
        if merge_missing_builtins(&mut db) {
            changed = true;
        }
        if migrate_merge_attendance_keyword(&mut db) {
            changed = true;
        }
        if migrate_input_field_syntax_db(&mut db) {
            changed = true;
        }
    }
    if changed {
        save_db(&db)?;
    }
    Ok(db)
}

fn save_db(db: &RecipeDb) -> Result<(), String> {
    let data = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    atomic_write_json(&paths::recipes_file(), &data)
}

pub fn list_recipes() -> Result<Vec<RecipeSummary>, String> {
    let db = load_db()?;
    let mut list: Vec<RecipeSummary> = db
        .recipes
        .iter()
        .map(|r| RecipeSummary {
            id: r.id.clone(),
            name: r.name.clone(),
            preview: recipe_preview(&r.content, 96),
            content: r.content.clone(),
            updated_at: r.updated_at,
        })
        .collect();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(list)
}

pub fn get_recipe(id: &str) -> Result<Option<Recipe>, String> {
    let id = id.trim();
    if id.is_empty() {
        return Ok(None);
    }
    let db = load_db()?;
    if let Some(r) = db.recipes.into_iter().find(|r| r.id == id) {
        return Ok(Some(r));
    }
    find_in_skills_cache_by_id(id)
}

fn find_in_skills_cache_by_id(id: &str) -> Result<Option<Recipe>, String> {
    let db = load_skills_cache()?;
    Ok(db
        .skills
        .into_iter()
        .find(|s| !s.id.is_empty() && s.id == id)
        .map(|s| Recipe {
            id: s.id,
            name: s.name,
            content: s.content,
            updated_at: Utc::now(),
        }))
}

/// 按技能名称查找（同名取最近更新的一条）
/// 优先查云端技能本地镜像（skills-cache.json），再回退 recipes.json。
pub fn find_recipe_by_name(name: &str) -> Result<Option<Recipe>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if let Some(r) = find_in_skills_cache(name)? {
        return Ok(Some(r));
    }
    let db = load_db()?;
    let mut matches: Vec<Recipe> = db
        .recipes
        .into_iter()
        .filter(|r| r.name.trim() == name)
        .collect();
    matches.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(matches.into_iter().next())
}

/// 云端技能条目（UI 同步写入，供「技能」指令展开）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCacheEntry {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub content: String,
    /// 1 启用 / 0 关闭（缺省视为启用）
    #[serde(default)]
    pub status: Option<i32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SkillsCacheDb {
    #[serde(default)]
    skills: Vec<SkillCacheEntry>,
}

fn load_skills_cache() -> Result<SkillsCacheDb, String> {
    with_skills_cache_lock(load_skills_cache_unlocked)
}

fn load_skills_cache_unlocked() -> Result<SkillsCacheDb, String> {
    let path = paths::skills_cache_file();
    if !path.exists() {
        return Ok(SkillsCacheDb::default());
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if data.trim().is_empty() {
        return Ok(SkillsCacheDb::default());
    }
    let db: SkillsCacheDb = parse_json_first(&data, "skills-cache.json 解析失败")?;
    // 尾部损坏时写回干净副本
    if serde_json::from_str::<SkillsCacheDb>(&data).is_err() {
        let pretty = serde_json::to_string_pretty(&db).map_err(|e| e.to_string())?;
        let _ = atomic_write_json(&path, &pretty);
    }
    Ok(db)
}

/// 读取云端技能本地镜像（供 AI 上下文 / 调试）
pub fn list_skills_cache() -> Result<Vec<SkillCacheEntry>, String> {
    Ok(load_skills_cache()?.skills)
}

fn find_in_skills_cache(name: &str) -> Result<Option<Recipe>, String> {
    let db = load_skills_cache()?;
    let mut found: Option<SkillCacheEntry> = None;
    for s in db.skills {
        if s.name.trim() == name {
            found = Some(s);
        }
    }
    Ok(found.map(|s| Recipe {
        id: if s.id.is_empty() {
            format!("skill-cache:{name}")
        } else {
            s.id
        },
        name: s.name,
        content: s.content,
        updated_at: Utc::now(),
    }))
}

/// 若本地镜像中该技能 `status=0`，返回禁止执行文案。
pub fn ensure_skill_executable(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(());
    }
    let db = load_skills_cache()?;
    for s in db.skills {
        if s.name.trim() == name && s.status == Some(0) {
            return Err(format!(
                "目前技能{name}被禁止操作，如有需要请联系公司管理员。"
            ));
        }
    }
    Ok(())
}

/// 用当前账号可见的技能列表覆盖本地镜像（侧栏同步后调用）
pub fn replace_skills_cache(skills: Vec<SkillCacheEntry>) -> Result<(), String> {
    with_skills_cache_lock(|| {
        let dir = paths::app_data_dir();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let db = SkillsCacheDb {
            skills: skills
                .into_iter()
                .map(|mut s| {
                    s.name = s.name.trim().to_string();
                    s.content = s.content.trim().to_string();
                    s.id = s.id.trim().to_string();
                    s
                })
                .filter(|s| !s.name.is_empty())
                .collect(),
        };
        let data = serde_json::to_string_pretty(&db).map_err(|e| e.to_string())?;
        atomic_write_json(&paths::skills_cache_file(), &data)
    })
}

pub fn save_recipe(id: Option<String>, name: String, content: String) -> Result<Recipe, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("快捷指令名称不能为空".into());
    }
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err("快捷指令内容不能为空".into());
    }

    let mut db = load_db()?;
    let now = Utc::now();
    let recipe_id = id.unwrap_or_else(|| Uuid::new_v4().to_string());

    if let Some(existing) = db.recipes.iter_mut().find(|r| r.id == recipe_id) {
        existing.name = name;
        existing.content = content;
        existing.updated_at = now;
        let saved = existing.clone();
        save_db(&db)?;
        return Ok(saved);
    }

    let recipe = Recipe {
        id: recipe_id,
        name,
        content,
        updated_at: now,
    };
    db.recipes.push(recipe.clone());
    save_db(&db)?;
    Ok(recipe)
}

pub fn delete_recipe(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let before = db.recipes.len();
    db.recipes.retain(|r| r.id != id);
    if db.recipes.len() == before {
        return Err("快捷指令不存在".into());
    }
    save_db(&db)
}
