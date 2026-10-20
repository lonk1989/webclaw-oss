//! 本机 → 伙伴空间远程任务包（`.webclawjob`）。
//!
//! 菜谱明文在包内；网站账号经 AES-256-GCM 加密。密钥在本机与伙伴空间各存一份
//! `remote-secret`，不经过助手对话。助手只负责把密文文件交给 `webclaw-worker job`。

use crate::paths;
use crate::recipe::{parse_recipe_steps, apply_recipe_params, OrBranch, Step};
use crate::web_systems::{self, WebSystemKind};
use regex::Regex;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const JOB_MAGIC: &[u8; 4] = b"WCJB";
const SECRET_MAGIC: &[u8; 4] = b"WCRS";
const ACCOUNTS_MAGIC: &[u8; 4] = b"WCAC";
const FORMAT_VERSION: u16 = 1;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

const COMPANION_SKILL_MD: &str = include_str!("../../skills/webclaw/companion-skill.md");
const COMPANION_SKILL_FILE_NAME: &str = "豆包伙伴webclaw远程服务初始化.md";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobAccount {
    pub id: String,
    pub name: String,
    pub kind: WebSystemKind,
    pub url: String,
    pub username: String,
    pub password: String,
}

/// 写入加密任务包内的运行中继信息（不含桌面 clientSecret）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRelayMeta {
    /// 完整 WS 地址，如 `wss://host/ws`
    pub ws_url: String,
    pub run_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteJobPlain {
    pub job_id: String,
    pub title: String,
    pub created_at: String,
    pub recipe: String,
    #[serde(default)]
    pub params: HashMap<String, String>,
    #[serde(default)]
    pub accounts: Vec<JobAccount>,
    /// 可选：伙伴 worker 启动后连中继上报进度
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<JobRelayMeta>,
    /// 继续任务：接上已有浏览器 / 恢复上次页面，不要从 about:blank 开跑
    #[serde(default)]
    pub resume_browser: bool,
    /// 整次远程执行（含多轮继续）共用的任务 ID；产物目录与浏览器 profile 都按它来
    #[serde(default)]
    pub run_task_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchResult {
    pub job_id: String,
    pub title: String,
    pub file_name: String,
    pub local_path: String,
    pub secret_path: String,
    pub needs_secret_install: bool,
    pub assistant_hint: String,
    pub accounts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_ws_url: Option<String>,
}

fn remote_secret_path() -> PathBuf {
    paths::app_data_dir().join("remote-secret")
}

fn remote_jobs_dir() -> PathBuf {
    let dir = paths::app_data_dir().join("remote-jobs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn secret_exported_marker() -> PathBuf {
    paths::app_data_dir().join(".remote-secret-exported")
}

fn fill_random(buf: &mut [u8]) -> Result<(), String> {
    getrandom::getrandom(buf).map_err(|e| format!("生成随机数失败: {e}"))
}

pub fn secret_file_bytes(key: &[u8; KEY_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 2 + KEY_LEN);
    out.extend_from_slice(SECRET_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(key);
    out
}

pub fn write_secret_bytes(path: &Path, key: &[u8; KEY_LEN]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, secret_file_bytes(key)).map_err(|e| format!("写入配对密钥失败: {e}"))
}

pub fn parse_secret_bytes(raw: &[u8]) -> Result<[u8; KEY_LEN], String> {
    if raw.len() == KEY_LEN {
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(raw);
        return Ok(key);
    }
    if raw.len() < 6 + KEY_LEN || &raw[0..4] != SECRET_MAGIC {
        return Err("配对密钥文件无效（不是 webclaw 远程密钥）".into());
    }
    let ver = u16::from_le_bytes([raw[4], raw[5]]);
    if ver != FORMAT_VERSION {
        return Err(format!("配对密钥版本不支持: {ver}"));
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&raw[6..6 + KEY_LEN]);
    Ok(key)
}

pub fn read_secret_bytes(path: &Path) -> Result<[u8; KEY_LEN], String> {
    let raw = std::fs::read(path).map_err(|_e| {
        format!(
            "找不到远程配对密钥（{}）。请先在本机导出配对文件，或把配对文件保存为该路径。",
            path.display()
        )
    })?;
    parse_secret_bytes(&raw)
}

/// 本机或 worker 数据目录下的配对密钥；没有则生成（仅本机首次）。
pub fn ensure_local_secret() -> Result<[u8; KEY_LEN], String> {
    let path = remote_secret_path();
    if path.is_file() {
        return read_secret_bytes(&path);
    }
    let mut key = [0u8; KEY_LEN];
    fill_random(&mut key)?;
    write_secret_bytes(&path, &key)?;
    Ok(key)
}

pub fn install_secret_from_bytes(raw: &[u8]) -> Result<PathBuf, String> {
    let key = parse_secret_bytes(raw)?;
    let dest = remote_secret_path();
    write_secret_bytes(&dest, &key)?;
    Ok(dest)
}

pub fn install_secret_from_file(src: &Path) -> Result<PathBuf, String> {
    let raw = std::fs::read(src).map_err(|e| format!("读配对密钥失败: {e}"))?;
    install_secret_from_bytes(&raw)
}

pub fn export_secret_copy(dest: &Path) -> Result<PathBuf, String> {
    let key = ensure_local_secret()?;
    write_secret_bytes(dest, &key)?;
    let _ = std::fs::write(secret_exported_marker(), b"1");
    Ok(dest.to_path_buf())
}

pub fn secret_path() -> PathBuf {
    remote_secret_path()
}

fn encrypt_payload(key: &[u8; KEY_LEN], plain: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("密钥无效: {e}"))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    fill_random(&mut nonce_bytes)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plain)
        .map_err(|e| format!("加密任务包失败: {e}"))?;
    let mut out = Vec::with_capacity(4 + 2 + NONCE_LEN + ct.len());
    out.extend_from_slice(JOB_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn decrypt_payload(key: &[u8; KEY_LEN], packed: &[u8]) -> Result<Vec<u8>, String> {
    if packed.len() < 4 + 2 + NONCE_LEN + 16 {
        return Err("任务包太小或已损坏".into());
    }
    if &packed[0..4] != JOB_MAGIC {
        return Err("不是 webclaw 远程任务包（.webclawjob）".into());
    }
    let ver = u16::from_le_bytes([packed[4], packed[5]]);
    if ver != FORMAT_VERSION {
        return Err(format!("任务包版本不支持: {ver}"));
    }
    let nonce = Nonce::from_slice(&packed[6..6 + NONCE_LEN]);
    let ct = &packed[6 + NONCE_LEN..];
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("密钥无效: {e}"))?;
    cipher
        .decrypt(nonce, ct)
        .map_err(|_| "无法打开任务包：配对密钥不一致，或文件已损坏。请在本机重新导出配对文件并重新发给助手。".into())
}

pub fn pack_job(job: &RemoteJobPlain, key: &[u8; KEY_LEN]) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(job).map_err(|e| format!("序列化任务失败: {e}"))?;
    encrypt_payload(key, &json)
}

pub fn unpack_job(packed: &[u8], key: &[u8; KEY_LEN]) -> Result<RemoteJobPlain, String> {
    let json = decrypt_payload(key, packed)?;
    serde_json::from_slice(&json).map_err(|e| format!("任务包内容无效: {e}"))
}

pub fn collect_accounts_for_recipe(recipe: &str) -> Result<Vec<JobAccount>, String> {
    let (content, _, _) = apply_recipe_params(recipe, &HashMap::new())?;
    let steps = if content.trim().is_empty() {
        Vec::new()
    } else {
        parse_recipe_steps(&content)?
    };
    let queries: Vec<String> = steps
        .iter()
        .filter(|s| s.keyword == "登录")
        .map(|s| s.args.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let missing = web_systems::missing_logins_for_queries(&queries)?;
    if !missing.is_empty() {
        let names: Vec<_> = missing.iter().map(|m| m.name.as_str()).collect();
        return Err(format!(
            "本机尚未配置网站账号：{}。请先在「设置 → 网站账号」填写账号密码，再生成助手运行包。",
            names.join("、")
        ));
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for q in queries {
        if !seen.insert(q.clone()) {
            continue;
        }
        let s = web_systems::find_web_system_by_query(&q)?;
        if s.username.trim().is_empty() || s.password.trim().is_empty() {
            return Err(format!(
                "「{}」还没有账号或密码。请先在「设置 → 网站账号」填写后，再生成助手运行包。",
                s.name
            ));
        }
        out.push(JobAccount {
            id: s.id,
            name: s.name,
            kind: s.kind,
            url: s.url,
            username: s.username,
            password: s.password,
        });
    }
    Ok(out)
}

pub fn import_job_accounts(accounts: &[JobAccount]) -> Result<(), String> {
    for a in accounts {
        web_systems::save_web_system(
            Some(a.id.clone()),
            a.name.clone(),
            a.kind,
            a.url.clone(),
            a.username.clone(),
            a.password.clone(),
            String::new(),
        )?;
    }
    Ok(())
}

fn job_file_name(job_id: &str, title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else if c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(24)
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "job" } else { slug };
    format!("webclaw-job-{slug}-{}.webclawjob", &job_id[..8.min(job_id.len())])
}

pub fn write_job_file(job: &RemoteJobPlain, key: &[u8; KEY_LEN]) -> Result<PathBuf, String> {
    let bytes = pack_job(job, key)?;
    let name = job_file_name(&job.job_id, &job.title);
    let path = remote_jobs_dir().join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("写入任务包失败: {e}"))?;
    Ok(path)
}

pub fn build_job(title: &str, recipe: &str, params: HashMap<String, String>) -> Result<RemoteJobPlain, String> {
    let recipe = recipe.trim();
    if recipe.is_empty() {
        return Err("没有可远程执行的步骤".into());
    }
    let accounts = collect_accounts_for_recipe(recipe)?;
    let mut params = params;
    crate::public_params::fill_missing_from_public_for(recipe, &mut params);
    Ok(RemoteJobPlain {
        job_id: uuid::Uuid::new_v4().to_string(),
        title: {
            let t = title.trim();
            if t.is_empty() {
                "远程任务".into()
            } else {
                t.to_string()
            }
        },
        created_at: chrono::Utc::now().to_rfc3339(),
        recipe: recipe.to_string(),
        params,
        accounts,
        relay: None,
        resume_browser: false,
        run_task_id: String::new(),
    })
}

/// 远程执行：账号尽量打入包内；本机缺账号不阻断（宿主机可能已 accounts-install）。
pub fn build_job_for_remote_execute(
    title: &str,
    recipe: &str,
    params: HashMap<String, String>,
) -> Result<RemoteJobPlain, String> {
    let recipe = recipe.trim();
    if recipe.is_empty() {
        return Err("没有可远程执行的步骤".into());
    }
    let accounts = collect_accounts_for_recipe(recipe).unwrap_or_else(|_| {
        // 尽力收集已配置的登录账号
        collect_accounts_best_effort(recipe).unwrap_or_default()
    });
    let mut params = params;
    crate::public_params::fill_missing_from_public_for(recipe, &mut params);
    Ok(RemoteJobPlain {
        job_id: uuid::Uuid::new_v4().to_string(),
        title: {
            let t = title.trim();
            if t.is_empty() {
                "远程任务".into()
            } else {
                t.to_string()
            }
        },
        created_at: chrono::Utc::now().to_rfc3339(),
        recipe: recipe.to_string(),
        params,
        accounts,
        relay: None,
        resume_browser: false,
        run_task_id: String::new(),
    })
}

fn collect_accounts_best_effort(recipe: &str) -> Result<Vec<JobAccount>, String> {
    let (content, _, _) = apply_recipe_params(recipe, &HashMap::new())?;
    let steps = if content.trim().is_empty() {
        Vec::new()
    } else {
        parse_recipe_steps(&content).unwrap_or_default()
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for q in steps
        .iter()
        .filter(|s| s.keyword == "登录")
        .map(|s| s.args.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        if !seen.insert(q.clone()) {
            continue;
        }
        if let Ok(s) = web_systems::find_web_system_by_query(&q) {
            if s.username.trim().is_empty() || s.password.trim().is_empty() {
                continue;
            }
            out.push(JobAccount {
                id: s.id,
                name: s.name,
                kind: s.kind,
                url: s.url,
                username: s.username,
                password: s.password,
            });
        }
    }
    Ok(out)
}

pub fn pack_job_bytes(job: &RemoteJobPlain) -> Result<Vec<u8>, String> {
    let key = ensure_local_secret()?;
    pack_job(job, &key)
}

/// 伙伴沙箱撑不住整包长任务：每轮最多下发这么多步（剩余下次用继续任务码接着跑）。
/// OOPIF 热路径加快后，单轮可从 40 提到 80 可执行步。
pub const REMOTE_SLICE_MAX_STEPS: usize = 80;

static RE_STEP_PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"步骤\s+(\d+)\s*/\s*(\d+)").expect("step progress regex")
});

/// 与执行器一致：先剥静态「参数」再解析，切片/续跑按可执行步骤计数。
pub fn executable_remote_steps(
    recipe: &str,
    params: &HashMap<String, String>,
) -> Result<(Vec<Step>, HashMap<String, String>), String> {
    let recipe = recipe.trim();
    if recipe.is_empty() {
        return Err("没有可远程执行的步骤".into());
    }
    let (content, params, _) = apply_recipe_params(recipe, params)?;
    let content = web_systems::substitute_web_system_vars(&content).unwrap_or(content);
    let steps = if content.trim().is_empty() {
        Vec::new()
    } else {
        parse_recipe_steps(&content)?
    };
    if steps.is_empty() {
        return Err("没有可远程执行的步骤".into());
    }
    Ok((steps, params))
}

/// 日志「步骤 34/34 成功」→ (index, total, Some(ok))；「── 步骤 1/40 ──」第三元为 None。
pub fn parse_step_progress_line(line: &str) -> Option<(usize, usize, Option<bool>)> {
    let caps = RE_STEP_PROGRESS.captures(line)?;
    let index: usize = caps.get(1)?.as_str().parse().ok()?;
    let total: usize = caps.get(2)?.as_str().parse().ok()?;
    if index == 0 || total == 0 {
        return None;
    }
    let after = &line[caps.get(0)?.end()..];
    let ok = if after.contains("失败") {
        Some(false)
    } else if after.contains("成功") || after.contains("已跳过") {
        Some(true)
    } else {
        None
    };
    Some((index, total, ok))
}

/// `completed_through`：原菜谱里已成功（含已跳过备选）的步数。返回尚未执行的步骤。
pub fn remaining_steps_after(steps: &[Step], completed_through: usize) -> Vec<Step> {
    let mut i = completed_through.min(steps.len());
    while i < steps.len() {
        let Some(og) = steps[i].or_group.as_ref() else {
            break;
        };
        if og.branch != OrBranch::Fallback {
            break;
        }
        let try_already_ran = steps[..i].iter().any(|s| {
            s.or_group.as_ref().is_some_and(|g| g.id == og.id && g.branch == OrBranch::Try)
        });
        if !try_already_ran {
            break;
        }
        i += 1;
    }
    steps[i..].to_vec()
}

fn escape_step_args(args: &str) -> String {
    let mut out = String::new();
    for c in args.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            '，' => out.push_str("\\，"),
            '.' => out.push_str("\\."),
            '。' => out.push_str("\\。"),
            _ => out.push(c),
        }
    }
    out
}

fn format_step_src(step: &Step) -> String {
    if step.args.trim().is_empty() {
        step.keyword.clone()
    } else {
        format!("{}{}", step.keyword, escape_step_args(&step.args))
    }
}

/// 把步骤列表还原成可再解析的菜谱正文（用于只下发剩余片段，而不是整包）。
pub fn join_steps_recipe(steps: &[Step]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while i < steps.len() {
        if let Some(og) = steps[i].or_group.clone() {
            let mut try_parts = Vec::new();
            let mut fb_parts = Vec::new();
            let mut j = i;
            while j < steps.len() {
                match steps[j].or_group.as_ref() {
                    Some(g) if g.id == og.id && g.branch == OrBranch::Try => {
                        try_parts.push(format_step_src(&steps[j]));
                        j += 1;
                    }
                    Some(g) if g.id == og.id && g.branch == OrBranch::Fallback => {
                        fb_parts.push(format_step_src(&steps[j]));
                        j += 1;
                    }
                    _ => break,
                }
            }
            if try_parts.is_empty() {
                for p in fb_parts {
                    if !out.is_empty() {
                        out.push('，');
                    }
                    out.push_str(&p);
                }
                i = j;
                continue;
            }
            if !out.is_empty() {
                out.push('，');
            }
            if fb_parts.is_empty() {
                out.push_str(&format!("（{}？）", try_parts.join("，")));
            } else {
                out.push_str(&format!(
                    "（{}？{}）",
                    try_parts.join("，"),
                    fb_parts.join("，")
                ));
            }
            i = j;
            continue;
        }
        if !out.is_empty() {
            out.push('，');
        }
        out.push_str(&format_step_src(&steps[i]));
        i += 1;
    }
    out
}

/// 从已完成步数起截取下一包。返回 (片段, 原菜谱 1-based 起点)。
pub fn take_remote_slice(
    steps: &[Step],
    completed_through: usize,
    max_len: usize,
) -> (Vec<Step>, usize) {
    let remaining = remaining_steps_after(steps, completed_through);
    let skipped = steps.len().saturating_sub(remaining.len());
    let origin = skipped + 1;
    let cap = max_len.max(1);
    let slice = remaining.into_iter().take(cap).collect();
    (slice, origin)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsPackPlain {
    pub exported_at: String,
    pub accounts: Vec<JobAccount>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportAccountsResult {
    pub local_path: String,
    pub file_name: String,
    pub account_names: Vec<String>,
}

pub fn pack_accounts(pack: &AccountsPackPlain, key: &[u8; KEY_LEN]) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(pack).map_err(|e| format!("序列化账号包失败: {e}"))?;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("密钥无效: {e}"))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    fill_random(&mut nonce_bytes)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, json.as_ref())
        .map_err(|e| format!("加密账号包失败: {e}"))?;
    let mut out = Vec::with_capacity(4 + 2 + NONCE_LEN + ct.len());
    out.extend_from_slice(ACCOUNTS_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn unpack_accounts(packed: &[u8], key: &[u8; KEY_LEN]) -> Result<AccountsPackPlain, String> {
    if packed.len() < 4 + 2 + NONCE_LEN + 16 {
        return Err("账号包太小或已损坏".into());
    }
    if &packed[0..4] != ACCOUNTS_MAGIC {
        return Err("不是 webclaw 网站账号包（.webclawacct）".into());
    }
    let ver = u16::from_le_bytes([packed[4], packed[5]]);
    if ver != FORMAT_VERSION {
        return Err(format!("账号包版本不支持: {ver}"));
    }
    let nonce = Nonce::from_slice(&packed[6..6 + NONCE_LEN]);
    let ct = &packed[6 + NONCE_LEN..];
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("密钥无效: {e}"))?;
    let json = cipher
        .decrypt(nonce, ct)
        .map_err(|_| "无法打开账号包：配对密钥不一致，或文件已损坏。".to_string())?;
    serde_json::from_slice(&json).map_err(|e| format!("账号包内容无效: {e}"))
}

fn credentialed_accounts() -> Result<Vec<JobAccount>, String> {
    let systems = web_systems::list_credentialed_systems()?;
    Ok(systems
        .into_iter()
        .map(|s| JobAccount {
            id: s.id,
            name: s.name,
            kind: s.kind,
            url: s.url,
            username: s.username,
            password: s.password,
        })
        .collect())
}

fn accounts_pack_bytes() -> Result<(Vec<u8>, Vec<String>), String> {
    let key = ensure_local_secret()?;
    let accounts = credentialed_accounts()?;
    if accounts.is_empty() {
        return Err("请先在「网站账号」里填好登录信息，再导出账号包。".into());
    }
    let names: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
    let pack = AccountsPackPlain {
        exported_at: chrono::Utc::now().to_rfc3339(),
        accounts,
    };
    Ok((pack_accounts(&pack, &key)?, names))
}

pub fn export_accounts_pack() -> Result<ExportAccountsResult, String> {
    let (bytes, names) = accounts_pack_bytes()?;
    let name = format!(
        "webclaw-accounts-{}.webclawacct",
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let path = remote_jobs_dir().join(&name);
    std::fs::write(&path, bytes).map_err(|e| format!("写入账号包失败: {e}"))?;
    Ok(ExportAccountsResult {
        local_path: path.display().to_string(),
        file_name: name,
        account_names: names,
    })
}

pub fn install_accounts_from_bytes(packed: &[u8]) -> Result<usize, String> {
    let key = read_secret_bytes(&secret_path())?;
    let pack = unpack_accounts(packed, &key)?;
    let n = pack.accounts.len();
    import_job_accounts(&pack.accounts)?;
    Ok(n)
}

pub fn install_accounts_from_file(path: &Path) -> Result<usize, String> {
    let packed = std::fs::read(path).map_err(|e| format!("读账号包失败: {e}"))?;
    install_accounts_from_bytes(&packed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPairFile {
    pub version: u16,
    pub relay_ws_url: String,
    pub owner_client_id: String,
    pub host_token: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHostPairResult {
    pub local_path: String,
    pub file_name: String,
    pub relay_ws_url: String,
    pub owner_client_id: String,
}

pub fn host_pair_current() -> Result<HostPairFile, String> {
    let s = crate::settings::load_settings()?;
    let owner = s.run_relay_client_id.trim();
    let token = s.run_relay_host_token.trim();
    if owner.is_empty() || token.is_empty() {
        return Err("工作伙伴尚未连上。请稍等几秒后再导出宿主机配对。".into());
    }
    let ws = crate::settings::resolve_run_relay_ws_url(&s);
    Ok(HostPairFile {
        version: 1,
        relay_ws_url: crate::run_relay::normalize_ws_url(&ws),
        owner_client_id: owner.to_string(),
        host_token: token.to_string(),
    })
}

/// 导出宿主机配对文件（不含网站密码）；需已有中继凭证与 hostToken。
pub fn export_host_pair() -> Result<ExportHostPairResult, String> {
    let pair = host_pair_current()?;
    let name = "webclaw-host-pair.json".to_string();
    let path = remote_jobs_dir().join(&name);
    let json = serde_json::to_vec_pretty(&pair).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写入宿主机配对失败: {e}"))?;
    Ok(ExportHostPairResult {
        local_path: path.display().to_string(),
        file_name: name,
        relay_ws_url: pair.relay_ws_url,
        owner_client_id: pair.owner_client_id,
    })
}

pub fn read_host_pair(path: &Path) -> Result<HostPairFile, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("读宿主机配对失败: {e}"))?;
    serde_json::from_slice(raw.as_bytes()).or_else(|_| {
        serde_json::from_str(&raw).map_err(|e| format!("宿主机配对文件无效: {e}"))
    })
}

pub fn install_host_pair_from_bytes(raw: &[u8]) -> Result<PathBuf, String> {
    let pair: HostPairFile =
        serde_json::from_slice(raw).map_err(|e| format!("宿主机配对文件无效: {e}"))?;
    let home = paths::app_data_dir();
    let dest = home.join("host-pair.json");
    let json = serde_json::to_vec_pretty(&pair).map_err(|e| e.to_string())?;
    std::fs::write(&dest, json).map_err(|e| format!("安装宿主机配对失败: {e}"))?;
    Ok(dest)
}

pub fn install_host_pair_from_file(src: &Path) -> Result<PathBuf, String> {
    let raw = std::fs::read(src).map_err(|e| format!("读宿主机配对失败: {e}"))?;
    install_host_pair_from_bytes(&raw)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSkillResult {
    pub local_path: String,
    pub file_name: String,
    /// 全文，供本机「一键复制」发到飞书对话
    pub content: String,
}

pub fn export_companion_skill() -> Result<ExportSkillResult, String> {
    let name = COMPANION_SKILL_FILE_NAME.to_string();
    let path = remote_jobs_dir().join(&name);
    std::fs::write(&path, COMPANION_SKILL_MD).map_err(|e| format!("写出伙伴初始化说明失败: {e}"))?;
    Ok(ExportSkillResult {
        local_path: path.display().to_string(),
        file_name: name,
        content: COMPANION_SKILL_MD.to_string(),
    })
}

pub fn assistant_hint(file_name: &str, secret_name: &str, first_secret: bool) -> String {
    let mut s = format!(
        "【webclaw 助手运行包】我已附上任务包文件 `{file_name}`。\n\
请直接用命令：webclaw-worker job --file 该文件路径\n\
不要打开、不要转发附件内容，不要把账号密码发到对话里。"
    );
    if first_secret {
        s.push_str(&format!(
            "\n若当前空间还没配对，请先安装我附上的配对文件 `{secret_name}`：可用 webclaw-worker secret-install --file 该文件。不要查看内容。"
        ));
    }
    s
}

/// 本机生成给智能助手直接运行的任务包。
pub fn dispatch_remote_job(
    title: &str,
    recipe: &str,
    params: HashMap<String, String>,
) -> Result<DispatchResult, String> {
    let key = ensure_local_secret()?;
    let mut job = build_job(title, recipe, params)?;

    let mut run_token_out = None;
    let mut relay_ws_out = None;
    #[cfg(feature = "desktop")]
    if let Some(ws) = crate::run_relay::resolve_ws_url() {
        match crate::run_relay::generate_run_token() {
            Ok(token) => match crate::run_relay::desktop_expect_run_blocking(
                &ws,
                &job.job_id,
                &token,
                &job.title,
            ) {
                Ok(hinted) => {
                    job.relay = Some(JobRelayMeta {
                        ws_url: hinted.clone(),
                        run_token: token.clone(),
                    });
                    run_token_out = Some(token);
                    relay_ws_out = Some(hinted);
                }
                Err(e) => {
                    eprintln!("[webclaw] 运行中继 expect_run 失败（任务包仍可生成）: {e}");
                }
            },
            Err(e) => eprintln!("[webclaw] 生成 runToken 失败: {e}"),
        }
    }

    let local_path = write_job_file(&job, &key)?;
    let file_name = local_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("job.webclawjob")
        .to_string();

    let mention_secret = !secret_exported_marker().is_file();
    let secret_local = remote_jobs_dir().join("webclaw-remote-secret");
    write_secret_bytes(&secret_local, &key)?;

    Ok(DispatchResult {
        job_id: job.job_id,
        title: job.title,
        file_name: file_name.clone(),
        local_path: local_path.display().to_string(),
        secret_path: secret_local.display().to_string(),
        needs_secret_install: mention_secret,
        assistant_hint: assistant_hint(&file_name, "webclaw-remote-secret", mention_secret),
        accounts: job.accounts.iter().map(|a| a.name.clone()).collect(),
        run_token: run_token_out,
        relay_ws_url: relay_ws_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let mut key = [0u8; KEY_LEN];
        key[0] = 7;
        key[31] = 9;
        let job = RemoteJobPlain {
            job_id: "abc".into(),
            title: "考勤".into(),
            created_at: "t".into(),
            recipe: "登录EHR系统，等待2".into(),
            params: HashMap::new(),
            accounts: vec![JobAccount {
                id: "ehr".into(),
                name: "EHR系统".into(),
                kind: WebSystemKind::Internal,
                url: "https://example.com/".into(),
                username: "u1".into(),
                password: "p1".into(),
            }],
            relay: Some(JobRelayMeta {
                ws_url: "ws://127.0.0.1:1301/ws".into(),
                run_token: "deadbeef".into(),
            }),
            resume_browser: false,
            run_task_id: String::new(),
        };
        let packed = pack_job(&job, &key).unwrap();
        assert!(packed.starts_with(JOB_MAGIC));
        let back = unpack_job(&packed, &key).unwrap();
        assert_eq!(back.recipe, job.recipe);
        assert_eq!(back.accounts[0].password, "p1");
        assert_eq!(
            back.relay.as_ref().map(|r| r.run_token.as_str()),
            Some("deadbeef")
        );
        let mut bad = key;
        bad[0] = 1;
        assert!(unpack_job(&packed, &bad).is_err());
    }

    #[test]
    fn secret_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("webclaw-secret-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("remote-secret");
        let mut key = [0u8; KEY_LEN];
        key[3] = 42;
        write_secret_bytes(&path, &key).unwrap();
        let got = read_secret_bytes(&path).unwrap();
        assert_eq!(got, key);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remaining_steps_skip_consumed_fallback() {
        let steps = parse_recipe_steps("点击甲，（点击乙？点击丙），点击丁").unwrap();
        assert!(steps.len() >= 4);
        // 完成「点击甲」+ 试探「点击乙」后，不应再下发已跳过的备选「点击丙」
        let try_idx = steps
            .iter()
            .position(|s| s.or_group.as_ref().is_some_and(|g| g.branch == crate::recipe::OrBranch::Try))
            .unwrap();
        let remaining = remaining_steps_after(&steps, try_idx + 1);
        assert!(remaining.iter().all(|s| {
            !s.or_group
                .as_ref()
                .is_some_and(|g| g.branch == crate::recipe::OrBranch::Fallback)
        }));
        assert_eq!(remaining.last().map(|s| s.args.as_str()), Some("丁"));
    }

    #[test]
    fn take_remote_slice_caps_length() {
        let steps = parse_recipe_steps("点击1，点击2，点击3，点击4，点击5").unwrap();
        let (slice, origin) = take_remote_slice(&steps, 0, 2);
        assert_eq!(origin, 1);
        assert_eq!(slice.len(), 2);
        let (slice2, origin2) = take_remote_slice(&steps, 2, 2);
        assert_eq!(origin2, 3);
        assert_eq!(slice2.len(), 2);
        assert_eq!(join_steps_recipe(&slice2), "点击3，点击4");
    }

    #[test]
    fn executable_steps_strip_static_params() {
        let _ = crate::commands::list_commands();
        let (steps, params) = executable_remote_steps(
            "参数公司=山西阳泉，参数月份=2026-07，登录EHR系统，点击时间管理",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(params.get("公司").map(String::as_str), Some("山西阳泉"));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "登录");
        assert_eq!(steps[1].keyword, "点击");
        assert!(join_steps_recipe(&steps).starts_with("登录"));
    }

    #[test]
    fn parse_step_progress_from_user_log() {
        assert_eq!(
            parse_step_progress_line("步骤 34/34 成功 · 输入 月份=2***"),
            Some((34, 34, Some(true)))
        );
        assert_eq!(
            parse_step_progress_line("── 步骤 1/34 ── 登录 EHR系统"),
            Some((1, 34, None))
        );
        assert_eq!(
            parse_step_progress_line("步骤 8/40 失败 · 点击 提交"),
            Some((8, 40, Some(false)))
        );
    }

    #[test]
    fn assistant_hint_has_no_secrets() {
        let s = assistant_hint("webclaw-job-x-abcd.webclawjob", "webclaw-remote-secret", true);
        assert!(s.contains("job --file"));
        assert!(s.contains("secret-install"));
        assert!(s.contains("不要把账号密码"));
        assert!(!s.to_lowercase().contains("password="));
    }
}
