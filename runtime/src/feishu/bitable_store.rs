use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use url::Url;
use uuid::Uuid;

pub const BUILTIN_PROJECT_REPORT_ID: &str = "builtin_project_report";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitableTable {
    pub id: String,
    pub name: String,
    pub url: String,
    pub app_token: String,
    pub table_id: String,
    /// 用于更新/删除/同步时的匹配键；当前页解析时若登记表 tokens 匹配会合并过来
    #[serde(default = "default_unique_keys")]
    pub unique_keys: Vec<String>,
    pub builtin: bool,
    pub updated_at: DateTime<Utc>,
}

fn default_unique_keys() -> Vec<String> {
    vec![]
}

fn project_report_unique_keys() -> Vec<String> {
    vec![
        "项目名称".into(),
        "归属月份".into(),
        "项目类别".into(),
    ]
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BitableDb {
    tables: Vec<BitableTable>,
}

/// 可选登记：仅保存别名与唯一键，不再作为「写死目标地址」的来源。
/// 执行时优先用浏览器当前页的 base/table。
fn default_tables() -> Vec<BitableTable> {
    let now = Utc::now();
    vec![BitableTable {
        id: BUILTIN_PROJECT_REPORT_ID.into(),
        name: "项目提报台账".into(),
        url: String::new(),
        app_token: String::new(),
        table_id: String::new(),
        unique_keys: project_report_unique_keys(),
        builtin: true,
        updated_at: now,
    }]
}

fn merge_missing_builtins(db: &mut BitableDb) -> bool {
    let mut changed = false;
    for def in default_tables() {
        if db.tables.iter().any(|t| t.id == def.id) {
            continue;
        }
        db.tables.push(def);
        changed = true;
    }
    changed
}

fn patch_builtin_metadata(db: &mut BitableDb) -> bool {
    let mut changed = false;
    for def in default_tables() {
        if let Some(existing) = db.tables.iter_mut().find(|t| t.id == def.id) {
            // 内置项改为「别名 + 唯一键」模板，清空写死的旧 URL，避免误打到错误的表
            if existing.id == BUILTIN_PROJECT_REPORT_ID {
                if !existing.url.is_empty()
                    || !existing.app_token.is_empty()
                    || !existing.table_id.is_empty()
                {
                    existing.url.clear();
                    existing.app_token.clear();
                    existing.table_id.clear();
                    changed = true;
                }
            }
            if existing.unique_keys.is_empty() && !def.unique_keys.is_empty() {
                existing.unique_keys = def.unique_keys.clone();
                changed = true;
            }
            if !existing.builtin {
                existing.builtin = true;
                changed = true;
            }
        }
    }
    changed
}

fn load_db() -> Result<BitableDb, String> {
    let path = paths::bitables_file();
    if !path.exists() {
        let db = BitableDb {
            tables: default_tables(),
        };
        save_db(&db)?;
        return Ok(db);
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut db: BitableDb =
        serde_json::from_str(&data).map_err(|e| format!("bitables.json 解析失败: {e}"))?;
    let mut changed = false;
    if db.tables.is_empty() {
        db.tables = default_tables();
        changed = true;
    } else {
        if merge_missing_builtins(&mut db) {
            changed = true;
        }
        if patch_builtin_metadata(&mut db) {
            changed = true;
        }
    }
    if changed {
        save_db(&db)?;
    }
    Ok(db)
}

fn save_db(db: &BitableDb) -> Result<(), String> {
    fs::create_dir_all(paths::app_data_dir()).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    fs::write(paths::bitables_file(), data).map_err(|e| e.to_string())
}

pub fn list_bitables() -> Result<Vec<BitableTable>, String> {
    Ok(load_db()?.tables)
}

pub fn get_by_name(name: &str) -> Result<BitableTable, String> {
    let name = name.trim();
    let db = load_db()?;
    db.tables
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("未找到多维表格「{name}」的登记信息"))
}

/// 从飞书多维表格页面 URL 解析 app_token 与 table_id。
/// 支持：
/// - `https://xxx.feishu.cn/base/{app_token}?table={table_id}`
/// - `https://xxx.larksuite.com/base/{app_token}?table=...`
/// - 路径中额外段（部分分享链接）
pub fn parse_bitable_url(raw: &str) -> Result<(String, String), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("页面地址为空".into());
    }
    let parsed = Url::parse(raw).map_err(|e| format!("无法解析页面地址: {e}"))?;
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if !(host.contains("feishu.cn") || host.contains("larksuite.com") || host.contains("feishu.net"))
    {
        return Err(format!("当前页不是飞书域名：{host}"));
    }

    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();

    let app_token = if let Some(idx) = segments.iter().position(|s| *s == "base") {
        segments
            .get(idx + 1)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "URL 中未找到 base/{{app_token}}".to_string())?
    } else {
        return Err(
            "当前页不是多维表格（URL 需包含 /base/{{app_token}}）。请先在浏览器打开目标多维表。"
                .into(),
        );
    };

    let mut table_id = parsed
        .query_pairs()
        .find(|(k, _)| k == "table")
        .map(|(_, v)| v.to_string())
        .unwrap_or_default();

    // 少数链接把 table_id 放在路径下一段
    if table_id.is_empty() {
        if let Some(idx) = segments.iter().position(|s| *s == "base") {
            if let Some(seg) = segments.get(idx + 2) {
                if seg.starts_with("tbl") {
                    table_id = seg.to_string();
                }
            }
        }
    }

    if table_id.is_empty() {
        return Err(
            "当前多维表 URL 缺少 table= 参数。请切换到具体数据表视图后再执行。".into(),
        );
    }

    Ok((app_token, table_id))
}

fn find_registered_overlay(
    app_token: &str,
    table_id: &str,
    alias: Option<&str>,
) -> Option<BitableTable> {
    let db = load_db().ok()?;
    // 1) tokens 全匹配
    if let Some(t) = db
        .tables
        .iter()
        .find(|t| !t.app_token.is_empty() && t.app_token == app_token && t.table_id == table_id)
    {
        return Some(t.clone());
    }
    // 2) 仅 app_token 匹配（登记了 base 但未写 table）
    if let Some(t) = db.tables.iter().find(|t| {
        !t.app_token.is_empty() && t.app_token == app_token && t.table_id.is_empty()
    }) {
        return Some(t.clone());
    }
    // 3) 指令里写了 @别名，用别名上的唯一键（即使登记 URL 已清空）
    if let Some(name) = alias.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(t) = db.tables.iter().find(|t| t.name == name) {
            return Some(t.clone());
        }
    }
    None
}

/// 由浏览器当前页构造操作目标；可用登记表补充 name / unique_keys。
pub fn table_from_page_url(page_url: &str, alias: Option<&str>) -> Result<BitableTable, String> {
    let (app_token, table_id) = parse_bitable_url(page_url)?;
    let overlay = find_registered_overlay(&app_token, &table_id, alias);

    let name = overlay
        .as_ref()
        .map(|t| t.name.clone())
        .or_else(|| alias.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "当前多维表".into());

    let unique_keys = overlay
        .as_ref()
        .map(|t| t.unique_keys.clone())
        .unwrap_or_default();

    let id = overlay
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| format!("page:{app_token}:{table_id}"));

    Ok(BitableTable {
        id,
        name,
        url: page_url.trim().to_string(),
        app_token,
        table_id,
        unique_keys,
        builtin: overlay.as_ref().map(|t| t.builtin).unwrap_or(false),
        updated_at: Utc::now(),
    })
}

/// 解析执行目标：优先浏览器当前多维表页。
/// `@别名` 只用于补充唯一键/显示名，不能代替「打开表格」。
pub fn resolve_for_command(
    page_url: Option<&str>,
    table_name: Option<&str>,
) -> Result<BitableTable, String> {
    const HOW_TO: &str = "正确用法：\n\
1. 用 webclaw 控制的浏览器打开目标多维表格（地址栏需含 /base/…?table=…）\n\
2. 再执行：飞书查询   或   飞书新增,真实列名=值\n\
说明：@别名 不是网址，不会打开或切换表格；列名必须与表头完全一致。";

    if let Some(url) = page_url.map(str::trim).filter(|s| !s.is_empty()) {
        match table_from_page_url(url, table_name) {
            Ok(t) => return Ok(t),
            Err(page_err) => {
                if let Some(name) = table_name.map(str::trim).filter(|s| !s.is_empty()) {
                    if let Ok(registered) = get_by_name(name) {
                        if !registered.app_token.is_empty() && !registered.table_id.is_empty() {
                            return Ok(registered);
                        }
                    }
                    return Err(format!(
                        "{page_err}\n你写的 @{name} 只是别名，不会自动打开表格。\n{HOW_TO}\n当前页：{url}"
                    ));
                }
                return Err(format!("{page_err}\n{HOW_TO}\n当前页：{url}"));
            }
        }
    }

    if let Some(name) = table_name.map(str::trim).filter(|s| !s.is_empty()) {
        if let Ok(registered) = get_by_name(name) {
            if !registered.app_token.is_empty() && !registered.table_id.is_empty() {
                return Ok(registered);
            }
        }
        return Err(format!(
            "浏览器还没有可用的多维表格页面。\n@{name} 不能代替打开页面。\n{HOW_TO}"
        ));
    }

    Err(format!("浏览器还没有可用的多维表格页面。\n{HOW_TO}"))
}

/// 兼容旧调用：仅按名称（需登记项含完整 tokens）
pub fn resolve_table(table_name: Option<&str>) -> Result<BitableTable, String> {
    resolve_for_command(None, table_name)
}

pub fn save_table(mut table: BitableTable) -> Result<BitableTable, String> {
    if table.name.trim().is_empty() {
        return Err("表名不能为空".into());
    }
    table.name = table.name.trim().to_string();
    table.updated_at = Utc::now();

    let mut db = load_db()?;
    if let Some(existing) = db.tables.iter_mut().find(|t| t.id == table.id) {
        *existing = table.clone();
    } else {
        if db.tables.iter().any(|t| t.name == table.name) {
            return Err(format!("表名「{}」已存在", table.name));
        }
        if table.id.is_empty() {
            table.id = Uuid::new_v4().to_string();
        }
        db.tables.push(table.clone());
    }
    save_db(&db)?;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_base_url() {
        let (app, table) = parse_bitable_url(
            "https://pcnzwk7m6zlv.feishu.cn/base/YgFubhAnealJQWshXe9cR3JSnhe?table=tbllenR6ukDA40xl&view=vew39qr6ZS",
        )
        .expect("parse");
        assert_eq!(app, "YgFubhAnealJQWshXe9cR3JSnhe");
        assert_eq!(table, "tbllenR6ukDA40xl");
    }

    #[test]
    fn parse_rejects_non_bitable() {
        assert!(parse_bitable_url("https://www.feishu.cn/").is_err());
        assert!(parse_bitable_url("https://example.com/base/xxx").is_err());
    }
}
