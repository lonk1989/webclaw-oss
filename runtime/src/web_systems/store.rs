//! 网站账号：云端应用系统目录 + 本机按用户保存的登录凭据。
//! 不再内置各系统默认账号密码。

use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSystemKind {
    Internal,
    External,
}

impl WebSystemKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Internal => "内网",
            Self::External => "外网",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebSystemSource {
    #[default]
    Local,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSystem {
    pub id: String,
    pub name: String,
    pub kind: WebSystemKind,
    pub url: String,
    pub username: String,
    pub password: String,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub source: WebSystemSource,
    /// 备注（本地可编辑；云端同步暂不覆盖）
    #[serde(default)]
    pub remark: String,
    /// 云端 status：1 启用，0 关闭
    #[serde(default)]
    pub remote_status: Option<i32>,
    /// 仅用于前端展示：是否已保存密码（不落盘）
    #[serde(default, skip_serializing)]
    pub has_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSystemSummary {
    pub id: String,
    pub name: String,
    pub kind: WebSystemKind,
    pub url: String,
    pub username: String,
    pub builtin: bool,
    pub remote: bool,
    pub has_password: bool,
    pub updated_at: DateTime<Utc>,
}

/// 前端从应用系统接口拉取后传入的条目
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWebSystemInput {
    pub system_id: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub remark: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WebSystemDb {
    systems: Vec<WebSystem>,
}

fn kind_from_category(category: Option<&str>) -> WebSystemKind {
    match category.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("external") | Some("外网") => WebSystemKind::External,
        _ => WebSystemKind::Internal,
    }
}

fn load_db() -> Result<WebSystemDb, String> {
    let path = paths::web_systems_file();
    if !path.exists() {
        let db = WebSystemDb {
            systems: Vec::new(),
        };
        save_db(&db)?;
        // 新用户目录：显式标记，永不注入历史测试账号
        let _ = mark_creds_scrubbed();
        return Ok(db);
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut db: WebSystemDb =
        serde_json::from_str(&data).map_err(|e| format!("网站账号文件解析失败: {e}"))?;
    let mut need_resave = false;
    for s in &mut db.systems {
        if s.password.is_empty() {
            continue;
        }
        if !super::crypto::is_encrypted_password(&s.password) {
            need_resave = true;
        }
        s.password = super::crypto::decrypt_password(&s.password)?;
    }
    if scrub_legacy_seeded_credentials(&mut db) {
        need_resave = true;
    }
    if need_resave {
        save_db(&db)?;
    }
    Ok(db)
}

/// 旧版客户端内置的个人测试系统（分发版不得再带默认账号密码）
const LEGACY_SEEDED_SYSTEM_IDS: &[&str] = &["ehr", "oa", "mes", "old-mes"];

/// 伙伴空间常用系统：名称与登录地址已知，账号密码由用户提供。
#[derive(Debug, Clone, Copy)]
pub struct KnownLoginSystem {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
}

pub fn known_login_catalog() -> &'static [KnownLoginSystem] {
    // OSS: name/id slots only. Real login URLs belong in private DSH overlays
    // (`config.systems` → `accounts ensure`), not in this public catalog.
    &[
        KnownLoginSystem {
            id: "ehr",
            name: "EHR系统",
            url: "https://example.com/",
        },
        KnownLoginSystem {
            id: "old-mes",
            name: "旧MES系统",
            url: "https://example.com/",
        },
        KnownLoginSystem {
            id: "oa",
            name: "OA系统",
            url: "https://example.com/",
        },
    ]
}

pub fn lookup_known_login(query: &str) -> Option<KnownLoginSystem> {
    let q = query.trim().to_lowercase();
    let q = q.trim_end_matches("系统").trim();
    known_login_catalog().iter().copied().find(|k| {
        k.id.eq_ignore_ascii_case(query.trim())
            || k.name.eq_ignore_ascii_case(query.trim())
            || k.name.trim_end_matches("系统").eq_ignore_ascii_case(q)
            || k.id.replace('-', "").eq_ignore_ascii_case(&q.replace(['-', '_', ' '], ""))
            || (q.contains("mes") && k.id == "old-mes")
            || (q.contains("ehr") && k.id == "ehr")
            || (q == "oa" && k.id == "oa")
    })
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerAccountItem {
    pub id: String,
    pub name: String,
    pub url: String,
    pub username: String,
    pub has_password: bool,
}

pub fn list_worker_accounts() -> Result<Vec<WorkerAccountItem>, String> {
    let db = load_db()?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for k in known_login_catalog() {
        seen.insert(k.id.to_string());
        if let Some(s) = db.systems.iter().find(|s| s.id == k.id || s.name == k.name) {
            out.push(WorkerAccountItem {
                id: s.id.clone(),
                name: s.name.clone(),
                url: if s.url.trim().is_empty() {
                    k.url.to_string()
                } else {
                    s.url.clone()
                },
                username: s.username.clone(),
                has_password: !s.password.is_empty(),
            });
        } else {
            out.push(WorkerAccountItem {
                id: k.id.to_string(),
                name: k.name.to_string(),
                url: k.url.to_string(),
                username: String::new(),
                has_password: false,
            });
        }
    }
    for s in &db.systems {
        if seen.contains(&s.id) {
            continue;
        }
        out.push(WorkerAccountItem {
            id: s.id.clone(),
            name: s.name.clone(),
            url: s.url.clone(),
            username: s.username.clone(),
            has_password: !s.password.is_empty(),
        });
    }
    Ok(out)
}

/// 写入/更新网站账号（伙伴空间；密码落盘加密）。`--url` 可省略则用已知目录。
pub fn upsert_worker_account(
    id: Option<&str>,
    name: Option<&str>,
    url: Option<&str>,
    username: &str,
    password: &str,
) -> Result<WorkerAccountItem, String> {
    let username = username.trim();
    let password = password.trim();
    if username.is_empty() {
        return Err("账号不能为空".into());
    }
    if password.is_empty() {
        return Err("密码不能为空".into());
    }
    let hint = id.or(name).unwrap_or("").trim();
    let known = lookup_known_login(hint);
    let system_id = id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| known.map(|k| k.id.to_string()))
        .ok_or_else(|| "请指定系统，如 --id ehr 或 --name EHR系统".to_string())?;
    let system_name = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| known.map(|k| k.name.to_string()))
        .unwrap_or_else(|| system_id.clone());
    let system_url = url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| known.map(|k| k.url.to_string()))
        .ok_or_else(|| format!("系统「{system_name}」需要登录地址 --url"))?;

    let saved = save_web_system(
        Some(system_id),
        system_name,
        WebSystemKind::Internal,
        system_url,
        username.to_string(),
        password.to_string(),
        String::new(),
    )?;
    Ok(WorkerAccountItem {
        id: saved.id,
        name: saved.name,
        url: saved.url,
        username: saved.username,
        has_password: saved.has_password,
    })
}

/// 预制/更新系统入口（id + 名称 + 登录 URL），不要求也不覆盖账号密码。
/// 供 DSH `config.systems` / `accounts ensure` 使用；密码仍由用户本机 `accounts set`。
pub fn ensure_worker_system_entry(
    id: Option<&str>,
    name: Option<&str>,
    url: &str,
) -> Result<WorkerAccountItem, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("accounts ensure 需要 --url".into());
    }
    if url.chars().count() > 255 {
        return Err("应用地址不能超过 255 个字符".into());
    }
    let hint = id.or(name).unwrap_or("").trim();
    let known = lookup_known_login(hint);
    let system_id = id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| known.map(|k| k.id.to_string()))
        .ok_or_else(|| "请指定系统，如 --id ehr 或 --name EHR系统".to_string())?;
    let system_id = normalize_local_id(&system_id)?;
    let system_name = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| known.map(|k| k.name.to_string()))
        .unwrap_or_else(|| system_id.clone());
    if system_name.chars().count() > 64 {
        return Err("应用名称不能超过 64 个字符".into());
    }

    let mut db = load_db()?;
    let now = Utc::now();
    if let Some(existing) = db.systems.iter_mut().find(|s| s.id == system_id) {
        if existing.source == WebSystemSource::Local {
            existing.name = system_name;
            existing.url = url.to_string();
        } else if existing.url.trim().is_empty() {
            // Remote entries keep cloud name/url; only fill blank url as last resort.
            existing.url = url.to_string();
        }
        existing.updated_at = now;
        let saved = existing.clone();
        save_db(&db)?;
        return Ok(WorkerAccountItem {
            id: saved.id,
            name: saved.name,
            url: saved.url,
            username: saved.username,
            has_password: !saved.password.is_empty(),
        });
    }

    let mut system = WebSystem {
        id: system_id,
        name: system_name,
        kind: WebSystemKind::Internal,
        url: url.to_string(),
        username: String::new(),
        password: String::new(),
        updated_at: now,
        source: WebSystemSource::Local,
        remark: String::new(),
        remote_status: None,
        has_password: false,
    };
    db.systems.push(system.clone());
    save_db(&db)?;
    system.password.clear();
    Ok(WorkerAccountItem {
        id: system.id,
        name: system.name,
        url: system.url,
        username: system.username,
        has_password: false,
    })
}

/// 导出用：带明文密码的已配置账号（仅本机/worker 进程内使用）。
pub fn list_credentialed_systems() -> Result<Vec<WebSystem>, String> {
    let db = load_db()?;
    Ok(db
        .systems
        .into_iter()
        .filter(|s| !s.username.trim().is_empty() && !s.password.trim().is_empty())
        .collect())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingLoginAccount {
    pub query: String,
    pub id: String,
    pub name: String,
    pub url: String,
}

/// 菜谱「登录xxx」步缺少账号或密码。
pub fn missing_logins_for_queries(queries: &[String]) -> Result<Vec<MissingLoginAccount>, String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for q in queries {
        let q = q.trim();
        if q.is_empty() || !seen.insert(q.to_string()) {
            continue;
        }
        match find_web_system_by_query(q) {
            Ok(s) if !s.username.trim().is_empty() && !s.password.trim().is_empty() => {}
            Ok(s) => out.push(MissingLoginAccount {
                query: q.to_string(),
                id: s.id,
                name: s.name,
                url: s.url,
            }),
            Err(_) => {
                let known = lookup_known_login(q);
                out.push(MissingLoginAccount {
                    query: q.to_string(),
                    id: known.map(|k| k.id.to_string()).unwrap_or_default(),
                    name: known
                        .map(|k| k.name.to_string())
                        .unwrap_or_else(|| q.to_string()),
                    url: known.map(|k| k.url.to_string()).unwrap_or_default(),
                });
            }
        }
    }
    Ok(out)
}

fn creds_scrub_marker() -> std::path::PathBuf {
    paths::app_data_dir().join(".creds-scrubbed-v1")
}

fn mark_creds_scrubbed() -> Result<(), String> {
    let dir = paths::app_data_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(creds_scrub_marker(), b"1").map_err(|e| e.to_string())
}

/// 一次性清空历史内置测试账号的用户名/密码（保留名称与 URL，由用户自行填写）
fn scrub_legacy_seeded_credentials(db: &mut WebSystemDb) -> bool {
    if creds_scrub_marker().is_file() {
        return false;
    }
    let mut changed = false;
    for s in &mut db.systems {
        if !LEGACY_SEEDED_SYSTEM_IDS.contains(&s.id.as_str()) {
            continue;
        }
        if !s.username.is_empty() || !s.password.is_empty() {
            s.username.clear();
            s.password.clear();
            changed = true;
        }
    }
    let _ = mark_creds_scrubbed();
    changed
}

fn save_db(db: &WebSystemDb) -> Result<(), String> {
    let dir = paths::app_data_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut disk = WebSystemDb {
        systems: db.systems.clone(),
    };
    for s in &mut disk.systems {
        s.password = super::crypto::encrypt_password(&s.password)?;
    }
    let data = serde_json::to_string_pretty(&disk).map_err(|e| e.to_string())?;
    fs::write(paths::web_systems_file(), data).map_err(|e| e.to_string())
}

fn normalize_local_id(raw: &str) -> Result<String, String> {
    let id = raw.trim().to_lowercase();
    if id.is_empty() {
        return Err("系统标识不能为空".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("系统标识仅支持字母、数字、下划线与连字符".into());
    }
    Ok(id)
}

/// 本地系统 id：`YYYYMMDD` + 19 位随机数字
fn generate_local_system_id() -> String {
    let date = chrono::Local::now().format("%Y%m%d").to_string();
    let mut digits = String::with_capacity(19);
    while digits.len() < 19 {
        for b in *Uuid::new_v4().as_bytes() {
            digits.push(char::from(b'0' + (b % 10)));
            if digits.len() >= 19 {
                break;
            }
        }
    }
    format!("{date}{digits}")
}

fn to_summary(s: &WebSystem) -> WebSystemSummary {
    WebSystemSummary {
        id: s.id.clone(),
        name: s.name.clone(),
        kind: s.kind,
        url: s.url.clone(),
        username: s.username.clone(),
        builtin: false,
        remote: s.source == WebSystemSource::Remote,
        has_password: !s.password.is_empty(),
        updated_at: s.updated_at,
    }
}

pub fn list_web_systems() -> Result<Vec<WebSystemSummary>, String> {
    let db = load_db()?;
    let mut list: Vec<WebSystemSummary> = db
        .systems
        .iter()
        .filter(|s| {
            if s.source == WebSystemSource::Remote {
                s.remote_status.unwrap_or(1) == 1
            } else {
                true
            }
        })
        .map(to_summary)
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

pub fn get_web_system(id: &str) -> Result<Option<WebSystem>, String> {
    let db = load_db()?;
    Ok(db.systems.into_iter().find(|s| s.id == id).map(|mut s| {
        s.has_password = !s.password.is_empty();
        s
    }))
}

/// 用应用系统接口结果覆盖云端条目，保留本机账号密码；本地自定义条目不受影响。
pub fn sync_remote_web_systems(
    remote: Vec<RemoteWebSystemInput>,
) -> Result<Vec<WebSystemSummary>, String> {
    let mut db = load_db()?;
    let mut creds: HashMap<String, (String, String)> = HashMap::new();
    for s in &db.systems {
        if !s.username.is_empty() || !s.password.is_empty() {
            creds.insert(s.id.clone(), (s.username.clone(), s.password.clone()));
        }
    }

    db.systems
        .retain(|s| s.source == WebSystemSource::Local);

    let now = Utc::now();
    let mut seen = std::collections::HashSet::new();
    for r in remote {
        let id = r.system_id.trim().to_string();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let status = r.status.unwrap_or(1);
        let (username, password) = creds
            .remove(&id)
            .unwrap_or_else(|| (String::new(), String::new()));
        db.systems.push(WebSystem {
            id,
            name: r.name.trim().to_string(),
            kind: kind_from_category(r.category.as_deref()),
            url: r.url.trim().to_string(),
            username,
            password,
            updated_at: now,
            source: WebSystemSource::Remote,
            remark: r.remark.unwrap_or_default().trim().to_string(),
            remote_status: Some(status),
            has_password: false,
        });
    }

    save_db(&db)?;
    list_web_systems()
}

pub fn save_web_system(
    id: Option<String>,
    name: String,
    kind: WebSystemKind,
    url: String,
    username: String,
    password: String,
    remark: String,
) -> Result<WebSystem, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("系统名称不能为空".into());
    }
    if name.chars().count() > 64 {
        return Err("应用名称不能超过 64 个字符".into());
    }
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("系统地址不能为空".into());
    }
    if url.chars().count() > 255 {
        return Err("应用地址不能超过 255 个字符".into());
    }
    let remark = remark.trim().to_string();
    if remark.chars().count() > 255 {
        return Err("备注不能超过 255 个字符".into());
    }
    let username = username.trim().to_string();
    let password = password.trim().to_string();

    let mut db = load_db()?;
    let now = Utc::now();

    // 已有条目（含云端）：按原 id 更新，云端不可改 name/url/id
    if let Some(raw) = id.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Some(existing) = db.systems.iter_mut().find(|s| s.id == raw) {
            if existing.source == WebSystemSource::Local {
                existing.name = name;
                existing.kind = kind;
                existing.url = url;
            }
            existing.remark = remark;
            existing.username = username;
            if !password.is_empty() {
                existing.password = password;
            }
            existing.updated_at = now;
            let mut saved = existing.clone();
            save_db(&db)?;
            saved.has_password = !saved.password.is_empty();
            saved.password.clear();
            return Ok(saved);
        }
        // 新建本地，使用用户指定 id
        let system_id = normalize_local_id(raw)?;
        if db.systems.iter().any(|s| s.id == system_id) {
            return Err("系统标识已存在".into());
        }
        let mut system = WebSystem {
            id: system_id,
            name,
            kind,
            url,
            username,
            password,
            updated_at: now,
            source: WebSystemSource::Local,
            remark,
            remote_status: None,
            has_password: false,
        };
        system.has_password = !system.password.is_empty();
        db.systems.push(system.clone());
        save_db(&db)?;
        system.password.clear();
        return Ok(system);
    }

    let mut system = WebSystem {
        id: generate_local_system_id(),
        name,
        kind,
        url,
        username,
        password,
        updated_at: now,
        source: WebSystemSource::Local,
        remark,
        remote_status: None,
        has_password: false,
    };
    system.has_password = !system.password.is_empty();
    db.systems.push(system.clone());
    save_db(&db)?;
    system.password.clear();
    Ok(system)
}

pub fn delete_web_system(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let Some(existing) = db.systems.iter().find(|s| s.id == id) else {
        return Err("网站系统不存在".into());
    };
    if existing.source == WebSystemSource::Remote {
        return Err("云端应用系统不可删除，可在后台管理；此处仅可修改本机账号密码".into());
    }
    db.systems.retain(|s| s.id != id);
    save_db(&db)
}

pub fn all_systems_for_substitute() -> Result<Vec<WebSystem>, String> {
    let db = load_db()?;
    Ok(db
        .systems
        .into_iter()
        .filter(|s| {
            if s.source == WebSystemSource::Remote {
                s.remote_status.unwrap_or(1) == 1
            } else {
                true
            }
        })
        .collect())
}

/// 根据名称/标识模糊匹配网站系统（大小写不敏感），如「EHR系统」「ehr」「OA」
pub fn find_web_system_by_query(query: &str) -> Result<WebSystem, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("请指定要登录的系统，如：登录EHR系统".into());
    }
    let q_lower = q.to_lowercase();
    let systems = all_systems_for_substitute()?;

    if let Some(s) = systems
        .iter()
        .find(|s| s.id.eq_ignore_ascii_case(q) || s.name.eq_ignore_ascii_case(q))
    {
        return Ok(s.clone());
    }

    let mut matches: Vec<&WebSystem> = systems
        .iter()
        .filter(|s| {
            let name_lower = s.name.to_lowercase();
            let id_lower = s.id.to_lowercase();
            name_lower.contains(&q_lower)
                || q_lower.contains(&name_lower)
                || id_lower.contains(&q_lower)
                || q_lower.contains(&id_lower)
        })
        .collect();

    if matches.is_empty() {
        let stripped = q_lower.trim_end_matches("系统").trim();
        if !stripped.is_empty() && stripped != q_lower {
            matches = systems
                .iter()
                .filter(|s| {
                    let name_lower = s.name.to_lowercase();
                    let id_lower = s.id.to_lowercase();
                    name_lower.contains(stripped)
                        || stripped.contains(&name_lower)
                        || id_lower.contains(stripped)
                        || stripped.contains(&id_lower)
                })
                .collect();
        }
    }

    match matches.len() {
        0 => {
            if crate::run_sink::worker_mode() {
                // DSH / lite：密码不得进对话。引导本机「设置 → 网站账号」表单。
                Err(format!(
                    "还没有「{query}」的登录账号。请用户打开 DSH「设置 → 网站账号」或双击 %LOCALAPPDATA%\\webclaw\\bin\\webclaw-accounts.cmd 填写（勿把密码发给助手）；webclaw_accounts_status 确认 has_password=true 后再跑。菜谱只写「登录{query}」，不要写「输入密码=…」。"
                ))
            } else {
                Err(format!(
                    "未找到匹配的网站系统「{query}」，请在设置 → 网站账号中配置（可点失败提示中的「打开网站账号设置」）"
                ))
            }
        }
        1 => Ok(matches[0].clone()),
        _ => {
            let names: Vec<_> = matches.iter().map(|s| s.name.as_str()).collect();
            Err(format!("匹配到多个系统，请更明确指定：{}", names.join("、")))
        }
    }
}

/// 将「打开B站」或「B站」等查询解析为 URL：已是 URL 则原样返回，否则匹配网站账号配置
pub fn resolve_open_url_from_query(raw: &str) -> Option<String> {
    let mut q = raw.trim();
    if q.is_empty() {
        return None;
    }
    if q.starts_with("打开") {
        q = q.trim_start_matches("打开").trim();
    }
    if q.is_empty() {
        return None;
    }
    let lower = q.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
        return Some(q.to_string());
    }
    find_web_system_by_query(q)
        .ok()
        .map(|s| s.url.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// 推断登录页 URL：已含 login/signin/index.html/# 则原样使用，否则追加 /login
pub fn login_url_for(system: &WebSystem) -> String {
    let raw = system.url.trim();
    let lower = raw.to_lowercase();
    if lower.contains("login")
        || lower.contains("signin")
        || lower.contains("sign-in")
        || lower.ends_with("index.html")
        // Hash SPA（如旧 MES）：登录表单在 #/ 等路由上，勿再拼 path /login
        || lower.contains('#')
    {
        return raw.to_string();
    }
    let base = raw.trim_end_matches('/');
    format!("{base}/login")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_open_url_passes_through_http() {
        assert_eq!(
            resolve_open_url_from_query("https://example.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            resolve_open_url_from_query("打开www.bilibili.com").as_deref(),
            Some("www.bilibili.com")
        );
        assert!(resolve_open_url_from_query("").is_none());
        assert!(resolve_open_url_from_query("打开").is_none());
    }

    #[test]
    fn login_url_appends_login_path() {
        let sys = WebSystem {
            id: "ehr".into(),
            name: "EHR".into(),
            kind: WebSystemKind::Internal,
            url: "https://ehr.example.com/".into(),
            username: String::new(),
            password: String::new(),
            updated_at: chrono::Utc::now(),
            source: WebSystemSource::Local,
            remark: String::new(),
            remote_status: None,
            has_password: false,
        };
        assert_eq!(login_url_for(&sys), "https://ehr.example.com/login");
    }

    #[test]
    fn login_url_keeps_oa_index() {
        let sys = WebSystem {
            id: "oa".into(),
            name: "OA".into(),
            kind: WebSystemKind::Internal,
            url: "https://example.com/".into(),
            username: String::new(),
            password: String::new(),
            updated_at: chrono::Utc::now(),
            source: WebSystemSource::Local,
            remark: String::new(),
            remote_status: None,
            has_password: false,
        };
        assert_eq!(
            login_url_for(&sys),
            "https://example.com/"
        );
    }

    #[test]
    fn login_url_keeps_hash_spa() {
        let sys = WebSystem {
            id: "old-mes".into(),
            name: "旧MES".into(),
            kind: WebSystemKind::Internal,
            url: "https://example.com/".into(),
            username: String::new(),
            password: String::new(),
            updated_at: chrono::Utc::now(),
            source: WebSystemSource::Local,
            remark: String::new(),
            remote_status: None,
            has_password: false,
        };
        assert_eq!(login_url_for(&sys), "https://example.com/");
    }

    #[test]
    fn lookup_known_ehr_and_old_mes() {
        assert_eq!(lookup_known_login("EHR系统").unwrap().id, "ehr");
        assert_eq!(lookup_known_login("ehr").unwrap().id, "ehr");
        assert_eq!(lookup_known_login("旧MES系统").unwrap().id, "old-mes");
        assert_eq!(lookup_known_login("旧MES").unwrap().id, "old-mes");
    }
}

