use crate::paths;
use crate::recipe::{self, RecipeParamKv};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use uuid::Uuid;

use super::calc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Success,
    Failed,
    Skipped,
    Queued,
    RecipeMissing,
    /// 程序未运行期间错过计划时间，等待用户确认是否补跑
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpec {
    Once {
        at: DateTime<Utc>,
    },
    Daily {
        hour: u8,
        minute: u8,
    },
    Weekly {
        weekday: u8,
        hour: u8,
        minute: u8,
    },
    /// 每月固定日（1–31）；当月天数不足时落在当月最后一天
    Monthly {
        day: u8,
        hour: u8,
        minute: u8,
    },
    Cron {
        expr: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub recipe_id: String,
    #[serde(default)]
    pub recipe_params: Vec<RecipeParamKv>,
    pub schedule: ScheduleSpec,
    pub enabled: bool,
    pub next_run_at: DateTime<Utc>,
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_run_status: Option<RunStatus>,
    #[serde(default)]
    pub last_run_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledJobSummary {
    pub id: String,
    pub name: String,
    pub recipe_id: String,
    pub schedule: ScheduleSpec,
    pub enabled: bool,
    pub next_run_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_status: Option<RunStatus>,
    pub last_run_summary: Option<String>,
    pub recipe_exists: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScheduledJobDb {
    jobs: Vec<ScheduledJob>,
}

fn load_db() -> Result<ScheduledJobDb, String> {
    let path = paths::scheduled_jobs_file();
    if !path.exists() {
        return Ok(ScheduledJobDb::default());
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if data.trim().is_empty() {
        return Ok(ScheduledJobDb::default());
    }
    serde_json::from_str(&data).map_err(|e| format!("定时任务文件解析失败: {e}"))
}

fn save_db(db: &ScheduledJobDb) -> Result<(), String> {
    let dir = paths::app_data_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    fs::write(paths::scheduled_jobs_file(), data).map_err(|e| e.to_string())
}

pub fn recipe_exists(recipe_id: &str) -> bool {
    recipe::get_recipe(recipe_id)
        .ok()
        .flatten()
        .is_some()
}

fn to_summary(job: &ScheduledJob) -> ScheduledJobSummary {
    ScheduledJobSummary {
        id: job.id.clone(),
        name: job.name.clone(),
        recipe_id: job.recipe_id.clone(),
        schedule: job.schedule.clone(),
        enabled: job.enabled,
        next_run_at: job.next_run_at,
        last_run_at: job.last_run_at,
        last_run_status: job.last_run_status,
        last_run_summary: job.last_run_summary.clone(),
        recipe_exists: recipe_exists(&job.recipe_id),
    }
}

pub fn list_scheduled_jobs() -> Result<Vec<ScheduledJobSummary>, String> {
    let db = load_db()?;
    let mut list: Vec<ScheduledJobSummary> = db.jobs.iter().map(to_summary).collect();
    list.sort_by(|a, b| a.next_run_at.cmp(&b.next_run_at));
    Ok(list)
}

pub fn get_scheduled_job(id: &str) -> Result<Option<ScheduledJob>, String> {
    let db = load_db()?;
    Ok(db.jobs.into_iter().find(|j| j.id == id))
}

pub fn delete_scheduled_job(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let before = db.jobs.len();
    db.jobs.retain(|j| j.id != id);
    if db.jobs.len() == before {
        return Err("定时任务不存在".into());
    }
    save_db(&db)
}

pub fn toggle_scheduled_job(id: &str, enabled: bool) -> Result<ScheduledJob, String> {
    let mut db = load_db()?;
    let job = db
        .jobs
        .iter_mut()
        .find(|j| j.id == id)
        .ok_or_else(|| "定时任务不存在".to_string())?;
    job.enabled = enabled;
    if enabled {
        job.next_run_at = calc::compute_next_run_at(&job.schedule, Utc::now())?;
    }
    job.updated_at = Utc::now();
    let saved = job.clone();
    save_db(&db)?;
    Ok(saved)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveScheduledJobInput {
    pub id: Option<String>,
    pub name: String,
    pub recipe_id: String,
    #[serde(default)]
    pub recipe_params: Vec<RecipeParamKv>,
    pub schedule: ScheduleSpec,
    #[serde(default)]
    pub enabled: bool,
}

pub fn save_scheduled_job(input: SaveScheduledJobInput) -> Result<ScheduledJob, String> {
    let recipe_id = input.recipe_id.trim();
    if recipe_id.is_empty() {
        return Err("请选择技能".into());
    }
    if !recipe_exists(recipe_id) {
        return Err("绑定的技能不存在（请先在主页同步技能列表）".into());
    }
    calc::validate_schedule(&input.schedule)?;

    let recipe = recipe::get_recipe(recipe_id)?
        .ok_or_else(|| "绑定的技能不存在".to_string())?;
    let name = if input.name.trim().is_empty() {
        recipe.name.clone()
    } else {
        input.name.trim().to_string()
    };

    let now = Utc::now();
    let next_run_at = calc::compute_next_run_at(&input.schedule, now)?;
    if input.enabled && next_run_at <= now {
        return Err("执行时间不能早于当前时间".into());
    }

    let mut db = load_db()?;
    let job_id = input.id.unwrap_or_else(|| Uuid::new_v4().to_string());

    if let Some(existing) = db.jobs.iter_mut().find(|j| j.id == job_id) {
        existing.name = name;
        existing.recipe_id = recipe_id.to_string();
        existing.recipe_params = input.recipe_params;
        existing.schedule = input.schedule;
        existing.enabled = input.enabled;
        existing.next_run_at = next_run_at;
        existing.updated_at = now;
        let saved = existing.clone();
        save_db(&db)?;
        return Ok(saved);
    }

    let job = ScheduledJob {
        id: job_id,
        name,
        recipe_id: recipe_id.to_string(),
        recipe_params: input.recipe_params,
        schedule: input.schedule,
        enabled: input.enabled,
        next_run_at,
        last_run_at: None,
        last_run_status: None,
        last_run_summary: None,
        created_at: now,
        updated_at: now,
    };
    db.jobs.push(job.clone());
    save_db(&db)?;
    Ok(job)
}

pub fn update_job_after_run(
    id: &str,
    success: bool,
    summary: &str,
    ran_at: DateTime<Utc>,
) -> Result<(), String> {
    let mut db = load_db()?;
    let job = db
        .jobs
        .iter_mut()
        .find(|j| j.id == id)
        .ok_or_else(|| "定时任务不存在".to_string())?;

    job.last_run_at = Some(ran_at);
    job.last_run_status = Some(if success {
        RunStatus::Success
    } else {
        RunStatus::Failed
    });
    job.last_run_summary = Some(summary.to_string());
    job.updated_at = Utc::now();

    match &job.schedule {
        ScheduleSpec::Once { .. } => {
            job.enabled = false;
        }
        _ => {
            job.next_run_at = calc::compute_next_run_at_after(&job.schedule, ran_at)?;
        }
    }
    save_db(&db)
}

pub fn mark_job_recipe_missing(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let job = db
        .jobs
        .iter_mut()
        .find(|j| j.id == id)
        .ok_or_else(|| "定时任务不存在".to_string())?;
    job.enabled = false;
    job.last_run_status = Some(RunStatus::RecipeMissing);
    job.last_run_summary = Some("绑定的快捷指令已删除".into());
    job.updated_at = Utc::now();
    save_db(&db)
}

pub fn mark_job_queued(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    if let Some(job) = db.jobs.iter_mut().find(|j| j.id == id) {
        job.last_run_status = Some(RunStatus::Queued);
        job.updated_at = Utc::now();
        save_db(&db)?;
    }
    Ok(())
}

pub fn advance_job_after_enqueue(id: &str, now: DateTime<Utc>) -> Result<(), String> {
    let mut db = load_db()?;
    let job = db
        .jobs
        .iter_mut()
        .find(|j| j.id == id)
        .ok_or_else(|| "定时任务不存在".to_string())?;
    match &job.schedule {
        ScheduleSpec::Once { .. } => {
            job.enabled = false;
        }
        _ => {
            job.next_run_at = calc::compute_next_run_at_after(&job.schedule, now)?;
        }
    }
    job.updated_at = now;
    save_db(&db)
}

pub fn due_jobs(now: DateTime<Utc>) -> Result<Vec<ScheduledJob>, String> {
    let db = load_db()?;
    Ok(db
        .jobs
        .into_iter()
        .filter(|j| {
            j.enabled
                && j.next_run_at <= now
                && j.last_run_status != Some(RunStatus::Expired)
                && recipe_exists(&j.recipe_id)
        })
        .collect())
}

/// 启动时扫描错过的任务，标记为「已过期」并返回摘要（不自动入队）。
pub fn scan_and_mark_expired_on_startup(
    now: DateTime<Utc>,
) -> Result<Vec<ScheduledJobSummary>, String> {
    let missed = missed_jobs_on_startup(now)?;
    if missed.is_empty() {
        return Ok(vec![]);
    }
    let mut db = load_db()?;
    let mut summaries = Vec::with_capacity(missed.len());
    for m in missed {
        let Some(job) = db.jobs.iter_mut().find(|j| j.id == m.id) else {
            continue;
        };
        if job.last_run_status != Some(RunStatus::Expired) {
            job.last_run_status = Some(RunStatus::Expired);
            job.last_run_summary = Some("计划时间已过，等待确认是否补跑".into());
            job.updated_at = now;
        }
        summaries.push(to_summary(job));
    }
    save_db(&db)?;
    Ok(summaries)
}

pub fn list_expired_jobs() -> Result<Vec<ScheduledJobSummary>, String> {
    let db = load_db()?;
    Ok(db
        .jobs
        .iter()
        .filter(|j| {
            j.enabled
                && j.last_run_status == Some(RunStatus::Expired)
                && recipe_exists(&j.recipe_id)
        })
        .map(to_summary)
        .collect())
}

/// 用户确认过期任务：`run_ids` 中的任务入队补跑，其余标记为跳过并推进下次时间。
pub fn resolve_expired_jobs(run_ids: &[String], now: DateTime<Utc>) -> Result<Vec<String>, String> {
    let mut db = load_db()?;
    let run_set: std::collections::HashSet<&str> = run_ids.iter().map(String::as_str).collect();
    let mut enqueued = Vec::new();
    for job in &mut db.jobs {
        if job.last_run_status != Some(RunStatus::Expired) {
            continue;
        }
        if run_set.contains(job.id.as_str()) {
            match &job.schedule {
                ScheduleSpec::Once { .. } => {
                    job.enabled = false;
                }
                _ => {
                    job.next_run_at = calc::compute_next_run_at_after(&job.schedule, now)?;
                }
            }
            job.last_run_status = Some(RunStatus::Queued);
            job.updated_at = now;
            enqueued.push(job.id.clone());
        } else {
            skip_expired_job_fields(job, now)?;
        }
    }
    save_db(&db)?;
    Ok(enqueued)
}

fn skip_expired_job_fields(job: &mut ScheduledJob, now: DateTime<Utc>) -> Result<(), String> {
    match &job.schedule {
        ScheduleSpec::Once { .. } => {
            job.enabled = false;
        }
        _ => {
            job.next_run_at = calc::compute_next_run_at_after(&job.schedule, now)?;
        }
    }
    job.last_run_status = Some(RunStatus::Skipped);
    job.last_run_summary = Some("已跳过过期执行".into());
    job.updated_at = now;
    Ok(())
}

pub fn missed_jobs_on_startup(now: DateTime<Utc>) -> Result<Vec<ScheduledJob>, String> {
    let db = load_db()?;
    let mut missed = Vec::new();
    for job in db.jobs {
        if !job.enabled {
            continue;
        }
        if !recipe_exists(&job.recipe_id) {
            continue;
        }
        if job.next_run_at >= now {
            continue;
        }
        match &job.schedule {
            ScheduleSpec::Once { at } => {
                let grace = chrono::Duration::hours(24);
                if now - *at <= grace {
                    missed.push(job);
                }
            }
            _ => {
                missed.push(job);
            }
        }
    }
    Ok(missed)
}
