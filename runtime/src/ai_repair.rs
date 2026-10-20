//! 步骤失败后的 AI 自动修复：观察页面 → 结构化补丁 → 执行器落地。

use crate::ark::{
    self, RepairOp, RepairOpKind, RepairPlan, RepairProbeItem, StepRepairContext,
};
use crate::browser_context;
use crate::cdp::CdpClient;
use crate::page_probe::{self, ProbeCandidate};
use crate::recipe::{norm_ui_text, parse_recipe_steps, OrBranch, Step};
use crate::task_progress::TaskStepRecord;
use crate::task_run::TaskControl;
use crate::web_systems;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::ui_host::{AppHandle, Emitter, Runtime};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub const MAX_AI_ROUNDS_PER_STEP: u32 = 2;
pub const MAX_AI_CALLS_PER_RUN: u32 = 6;
pub const MAX_INSERT_STEPS: usize = 5;
pub const MAX_PROBE_ITEMS: usize = 40;
pub const MAX_UPCOMING_STEPS: usize = 8;
const PROBE_TIMEOUT_SECS: u64 = 5;
const REPAIR_TIMEOUT_SECS: u64 = 45;
const MAX_CACHE_ENTRIES: usize = 200;

const ALLOWED_KEYWORDS: &[&str] = &[
        "打开", "点击", "输入", "等待", "按", "刷新", "滚动", "截图", "采集", "AI", "打开标签", "关闭标签", "页签", "登录",
    "人机验证",
    "剪切板",
];

const FORBIDDEN_KEYWORDS: &[&str] = &[
    "飞书新增",
    "飞书更新",
    "飞书删除",
    "飞书查询",
    "飞书同步",
    "飞书上传",
    "上传",
    "脚本",
    "断言",
    "技能",
    "宏",
    "参数",
    "动态参数",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Cancelled,
    DeadPage,
    UiFailure,
}

#[derive(Debug, Clone)]
pub enum RepairApply {
    /// 当前下标步骤已改写，就地重试
    RetryCurrent,
    /// 已插入前置步骤，外层循环应从该下标重新开始
    RestartFrom(usize),
    /// 视为成功并继续后续步骤
    Skip { detail: String },
    /// 先走页面刷新，再重试当前步
    ReloadThenRetry,
    /// 不自动修，交给人工
    NeedHuman { reason: String },
    /// 计划无效，本轮放弃
    Noop { reason: String },
}

pub fn classify_error(err: &str) -> ErrorClass {
    let e = err.trim();
    if e.contains("已取消") || e.contains("已停止") {
        return ErrorClass::Cancelled;
    }
    let el = e.to_lowercase();
    if CdpClient::is_browser_dead_error(e)
        || CdpClient::is_session_lost_error(e)
        || e.contains("刷新后页面加载超时")
        || e.contains("等待页面锚点超时")
        || e.contains("浏览器错误页")
        || e.contains("页面内容为空")
        || e.contains("chrome-error")
        || el.contains("502")
        || el.contains("503")
        || el.contains("504")
        || e.contains("无法访问")
        || e.contains("neterror")
        || e.contains("CDP")
        || e.contains("未附着")
    {
        return ErrorClass::DeadPage;
    }
    ErrorClass::UiFailure
}

/// 需要重启浏览器进程（而非仅刷新页面）的失败。
pub fn needs_browser_restart(err: &str) -> bool {
    CdpClient::is_browser_dead_error(err)
}

pub fn is_repairable_step(step: &Step) -> bool {
    if step.is_or_try() {
        return false;
    }
    match step.keyword.as_str() {
        "参数" | "动态参数" | "技能" | "宏" | "上传" | "飞书上传" | "飞书新增" | "飞书更新"
        | "飞书删除" | "飞书查询" | "飞书同步" | "脚本" | "断言" => false,
        "打开" | "点击" | "输入" | "等待" | "打开标签" | "关闭标签" | "登录" | "按" | "刷新" | "滚动"
        | "截图" | "采集" | "AI" | "页签" | "剪切板" | "人机验证" => true,
        _ => false,
    }
}

pub fn format_step_text(step: &Step) -> String {
    if step.args.trim().is_empty() {
        step.keyword.clone()
    } else {
        format!("{}{}", step.keyword, step.args)
    }
}

pub fn steps_to_recipe_text(steps: &[Step]) -> String {
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
                        try_parts.push(format_step_text(&steps[j]));
                        j += 1;
                    }
                    Some(g) if g.id == og.id && g.branch == OrBranch::Fallback => {
                        fb_parts.push(format_step_text(&steps[j]));
                        j += 1;
                    }
                    _ => break,
                }
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
        out.push_str(&format_step_text(&steps[i]));
        i += 1;
    }
    out
}

fn probe_cmd(item: &ProbeCandidate) -> String {
    let text = item
        .text_cmd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let index = item
        .index_cmd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let click = item.click_cmd.trim();
    text.or(index)
        .unwrap_or(click)
        .to_string()
}

pub fn format_probe_summary(items: &[ProbeCandidate], limit: usize) -> String {
    items
        .iter()
        .take(limit)
        .filter_map(|it| {
            let cmd = probe_cmd(it);
            if cmd.is_empty() {
                None
            } else {
                Some(cmd)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn looks_like_login_page(url: &str, title: &str, probe_cmds: &[String]) -> bool {
    let u = url.to_lowercase();
    let t = title.to_lowercase();
    if u.contains("login")
        || u.contains("signin")
        || u.contains("sign-in")
        || u.contains("/sso")
        || u.contains("/cas")
        || t.contains("登录")
        || t.contains("login")
    {
        return true;
    }
    let joined = probe_cmds.join(" ");
    let n = norm_ui_text(&joined);
    (n.contains("密码") || n.contains("验证码")) && n.contains("登录")
}

fn match_web_system_name(url: &str) -> Option<String> {
    let host = browser_context::hostname_from_url(url).to_lowercase();
    if host.is_empty() {
        return None;
    }
    let systems = web_systems::list_web_systems().ok()?;
    let mut hits: Vec<&web_systems::WebSystemSummary> = systems
        .iter()
        .filter(|s| {
            let sh = browser_context::hostname_from_url(&s.url).to_lowercase();
            !sh.is_empty()
                && (host == sh
                    || host.ends_with(&format!(".{sh}"))
                    || sh.ends_with(&format!(".{host}")))
        })
        .collect();
    if hits.is_empty() {
        return None;
    }
    hits.sort_by_key(|s| browser_context::hostname_from_url(&s.url).len());
    hits.last().map(|s| s.name.clone())
}

pub fn login_heuristic_plan(ctx: &StepRepairContext) -> Option<RepairPlan> {
    let snap = ctx.browser.as_ref()?;
    if ctx.step_label.trim().starts_with("登录") {
        return None;
    }
    let cmds: Vec<String> = ctx.probe_items.iter().map(|p| p.cmd.clone()).collect();
    if !looks_like_login_page(&snap.url, &snap.title, &cmds) {
        return None;
    }
    let name = match_web_system_name(&snap.url)?;
    Some(RepairPlan {
        confidence: "high".into(),
        need_human: false,
        reason: format!("当前已跳到登录页，先登录{name}"),
        ops: vec![RepairOp {
            op: RepairOpKind::InsertBefore,
            step: None,
            steps: vec![format!("登录{name}")],
        }],
    })
}

async fn capture_png(client: &mut CdpClient) -> Option<Vec<u8>> {
    if client.ensure_attached().await.is_err() {
        return None;
    }
    client.capture_screenshot_png().await.ok()
}

pub async fn gather_repair_context(
    client: &mut CdpClient,
    control: &TaskControl,
    steps: &[Step],
    step_index: usize,
    step_desc: &str,
    err: &str,
    recovery_attempted: bool,
    previous_notes: &[String],
    probe_dump_at_fail: Option<&str>,
    probe_dump_after_recovery: Option<&str>,
    reuse_probe: Option<&page_probe::ProbePageResult>,
) -> (StepRepairContext, Option<Vec<u8>>) {
    let _ = client.sync_page_meta().await;
    let snap = browser_context::snapshot_from_client(client);
    // 截图在探针前：避免偶发残留 overlay 进图；也不在扫描期叠加截图内存
    let png = capture_png(client).await;

    let probe = if let Some(p) = reuse_probe {
        Some(p.clone())
    } else {
        let scanned = tokio::time::timeout(
            Duration::from_secs(PROBE_TIMEOUT_SECS),
            page_probe::scan_attached_page_for_repair(client, MAX_PROBE_ITEMS),
        )
        .await
        .ok()
        .and_then(|r| r.ok());
        // 超时或失败也清一次，防止 evaluate 仍在页内跑完后留下图层
        page_probe::clear_probe_residue(client).await;
        scanned
    };

    let probe_items: Vec<RepairProbeItem> = probe
        .as_ref()
        .map(|p| {
            p.items
                .iter()
                .filter_map(|it| {
                    let cmd = probe_cmd(it);
                    if cmd.is_empty() {
                        None
                    } else {
                        Some(RepairProbeItem {
                            cmd,
                            desc: it.desc.clone(),
                            frame: it.frame_label.clone(),
                        })
                    }
                })
                .take(MAX_PROBE_ITEMS)
                .collect()
        })
        .unwrap_or_default();

    let upcoming_steps: Vec<String> = steps
        .iter()
        .skip(step_index + 1)
        .take(MAX_UPCOMING_STEPS)
        .map(format_step_text)
        .collect();

    let recent_logs: Vec<String> = {
        let logs = control.logs.lock();
        let n = logs.len();
        let start = n.saturating_sub(20);
        logs[start..].to_vec()
    };

    let ctx = StepRepairContext {
        step_label: step_desc.to_string(),
        error: err.to_string(),
        recovery_attempted,
        browser: Some(snap),
        upcoming_steps,
        probe_items,
        recent_logs,
        previous_notes: previous_notes.to_vec(),
        probe_dump_at_fail: probe_dump_at_fail
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        probe_dump_after_recovery: probe_dump_after_recovery
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    (ctx, png)
}

pub fn cache_fingerprint(hostname: &str, step: &Step, err: &str) -> String {
    let class = match classify_error(err) {
        ErrorClass::DeadPage => "dead",
        ErrorClass::UiFailure => "ui",
        ErrorClass::Cancelled => "cancel",
    };
    format!(
        "{}|{}|{}|{}",
        hostname.trim().to_lowercase(),
        step.keyword,
        norm_ui_text(&step.args),
        class
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepairCacheFile {
    #[serde(default)]
    entries: Vec<RepairCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepairCacheEntry {
    key: String,
    plan: RepairPlan,
    #[serde(default)]
    hits: u32,
    updated_at: DateTime<Utc>,
}

fn cache_path() -> std::path::PathBuf {
    crate::paths::app_data_dir().join("ai-repair-cache.json")
}

fn load_cache() -> RepairCacheFile {
    let p = cache_path();
    let raw = std::fs::read_to_string(p).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_else(|_| RepairCacheFile { entries: Vec::new() })
}

fn save_cache(file: &RepairCacheFile) {
    let p = cache_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(file) {
        let _ = std::fs::write(p, s);
    }
}

pub fn lookup_cached_plan(key: &str) -> Option<RepairPlan> {
    let file = load_cache();
    file.entries
        .iter()
        .find(|e| e.key == key)
        .map(|e| e.plan.clone())
}

pub fn remember_successful_plan(key: &str, plan: &RepairPlan) {
    if key.is_empty() || !plan.is_auto_applicable() {
        return;
    }
    let mut file = load_cache();
    if let Some(existing) = file.entries.iter_mut().find(|e| e.key == key) {
        existing.plan = plan.clone();
        existing.hits = existing.hits.saturating_add(1);
        existing.updated_at = Utc::now();
    } else {
        file.entries.push(RepairCacheEntry {
            key: key.to_string(),
            plan: plan.clone(),
            hits: 1,
            updated_at: Utc::now(),
        });
    }
    file.entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    if file.entries.len() > MAX_CACHE_ENTRIES {
        file.entries.truncate(MAX_CACHE_ENTRIES);
    }
    save_cache(&file);
}

pub enum ProposeSource {
    LoginHeuristic,
    Cache,
    Model,
}

pub struct ProposeOutcome {
    pub plan: RepairPlan,
    pub source: ProposeSource,
}

/// 采集观察并生成修复计划：登录启发 → 缓存 → 模型
pub async fn propose_repair(
    client: &mut CdpClient,
    control: &TaskControl,
    steps: &[Step],
    step_index: usize,
    step: &Step,
    step_desc: &str,
    err: &str,
    recovery_attempted: bool,
    previous_notes: &[String],
    probe_dump_at_fail: Option<&str>,
    probe_dump_after_recovery: Option<&str>,
    reuse_probe: Option<&page_probe::ProbePageResult>,
) -> Result<ProposeOutcome, String> {
    if control.cancel.load(Ordering::SeqCst) {
        return Err("已取消".into());
    }
    let (ctx, png) = gather_repair_context(
        client,
        control,
        steps,
        step_index,
        step_desc,
        err,
        recovery_attempted,
        previous_notes,
        probe_dump_at_fail,
        probe_dump_after_recovery,
        reuse_probe,
    )
    .await;
    if control.cancel.load(Ordering::SeqCst) {
        return Err("已取消".into());
    }

    if let Some(plan) = login_heuristic_plan(&ctx) {
        return Ok(ProposeOutcome {
            plan,
            source: ProposeSource::LoginHeuristic,
        });
    }

    let host = ctx
        .browser
        .as_ref()
        .map(|b| b.hostname.clone())
        .unwrap_or_default();
    let key = cache_fingerprint(&host, step, err);
    if let Some(plan) = lookup_cached_plan(&key) {
        if plan.is_auto_applicable() {
            return Ok(ProposeOutcome {
                plan,
                source: ProposeSource::Cache,
            });
        }
    }

    let cancel = control.cancel.clone();
    let plan = tokio::time::timeout(
        Duration::from_secs(REPAIR_TIMEOUT_SECS),
        ark::propose_step_repair(&ctx, png.as_deref(), Some(cancel)),
    )
    .await
    .map_err(|_| "AI 修复超时".to_string())?
    .map_err(|e| e)?;
    Ok(ProposeOutcome {
        plan,
        source: ProposeSource::Model,
    })
}

fn parse_single_repair_step(text: &str) -> Result<Step, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("空步骤".into());
    }
    let parsed = parse_recipe_steps(t)?;
    if parsed.len() != 1 {
        return Err(format!("「{t}」解析为 {} 步，期望 1 步", parsed.len()));
    }
    let step = parsed.into_iter().next().unwrap();
    validate_repair_step(&step)?;
    Ok(step)
}

fn validate_repair_step(step: &Step) -> Result<(), String> {
    let kw = step.keyword.as_str();
    if FORBIDDEN_KEYWORDS.contains(&kw) {
        return Err(format!("不允许自动插入「{kw}」"));
    }
    if !ALLOWED_KEYWORDS.contains(&kw) {
        return Err(format!("不支持的修复步骤「{kw}」"));
    }
    Ok(())
}

pub fn summarize_plan(plan: &RepairPlan) -> String {
    let mut parts = Vec::new();
    for op in &plan.ops {
        match op.op {
            RepairOpKind::InsertBefore => {
                if !op.steps.is_empty() {
                    parts.push(format!("插入「{}」", op.steps.join("，")));
                }
            }
            RepairOpKind::RetryAs => {
                if let Some(s) = op.step.as_deref() {
                    parts.push(format!("改写为「{s}」"));
                }
            }
            RepairOpKind::Skip => parts.push("跳过当前步".into()),
            RepairOpKind::ReloadThenRetry => parts.push("刷新后重试".into()),
            RepairOpKind::AskHuman => parts.push("需人工处理".into()),
        }
    }
    if parts.is_empty() {
        plan.reason.clone()
    } else if plan.reason.trim().is_empty() {
        parts.join("；")
    } else {
        format!("{}：{}", plan.reason.trim(), parts.join("；"))
    }
}

pub fn apply_repair_plan(
    plan: &RepairPlan,
    steps: &mut Vec<Step>,
    i: usize,
    control: &TaskControl,
) -> RepairApply {
    if let Some(reason) = plan.ask_human_reason() {
        return RepairApply::NeedHuman { reason };
    }
    if !plan.is_auto_applicable() {
        return RepairApply::Noop {
            reason: plan.reason.clone(),
        };
    }

    let mut insert_count = 0usize;
    let mut did_replace = false;
    let mut did_skip = false;
    let mut did_reload = false;

    for op in &plan.ops {
        match op.op {
            RepairOpKind::AskHuman => {
                return RepairApply::NeedHuman {
                    reason: plan.reason.clone(),
                };
            }
            RepairOpKind::Skip => {
                did_skip = true;
            }
            RepairOpKind::ReloadThenRetry => {
                did_reload = true;
            }
            RepairOpKind::InsertBefore => {
                if op.steps.is_empty() {
                    return RepairApply::Noop {
                        reason: "insert_before 为空".into(),
                    };
                }
                if op.steps.len() > MAX_INSERT_STEPS {
                    return RepairApply::Noop {
                        reason: format!("一次最多插入 {MAX_INSERT_STEPS} 步"),
                    };
                }
                let mut parsed = Vec::new();
                for s in &op.steps {
                    match parse_single_repair_step(s) {
                        Ok(st) => parsed.push(st),
                        Err(e) => {
                            return RepairApply::Noop {
                                reason: format!("无法插入「{s}」: {e}"),
                            };
                        }
                    }
                }
                let n = parsed.len();
                let at = i.min(steps.len());
                steps.splice(at..at, parsed);
                rebuild_records_after_insert(control, steps, at, n);
                insert_count += n;
            }
            RepairOpKind::RetryAs => {
                let Some(text) = op.step.as_deref() else {
                    return RepairApply::Noop {
                        reason: "retry_as 缺少 step".into(),
                    };
                };
                match parse_single_repair_step(text) {
                    Ok(mut new_step) => {
                        let target = if insert_count > 0 {
                            i + insert_count
                        } else {
                            i
                        };
                        if target >= steps.len() {
                            return RepairApply::Noop {
                                reason: "retry_as 目标步骤越界".into(),
                            };
                        }
                        new_step.or_group = steps[target].or_group.clone();
                        steps[target] = new_step;
                        update_record_label(control, steps, target);
                        did_replace = true;
                    }
                    Err(e) => {
                        return RepairApply::Noop {
                            reason: format!("无法改写为「{text}」: {e}"),
                        };
                    }
                }
            }
        }
    }

    *control.live_steps.lock() = steps.clone();

    if did_skip && insert_count == 0 && !did_replace {
        return RepairApply::Skip {
            detail: if plan.reason.trim().is_empty() {
                "AI 判定目标已达成".into()
            } else {
                plan.reason.clone()
            },
        };
    }
    if insert_count > 0 {
        return RepairApply::RestartFrom(i);
    }
    if did_reload {
        return RepairApply::ReloadThenRetry;
    }
    if did_replace {
        return RepairApply::RetryCurrent;
    }
    RepairApply::Noop {
        reason: "修复计划无有效操作".into(),
    }
}

fn rebuild_records_after_insert(
    control: &TaskControl,
    steps: &[Step],
    insert_at: usize,
    insert_count: usize,
) {
    let old = control.step_records.lock().clone();
    let mut new_recs = Vec::with_capacity(steps.len());
    let params = control.recipe_params.lock().clone();
    for (idx, step) in steps.iter().enumerate() {
        let (status, detail) = if idx < insert_at {
            old.get(idx)
                .map(|r| (r.status.clone(), r.detail.clone()))
                .unwrap_or_else(|| ("pending".into(), String::new()))
        } else if idx >= insert_at + insert_count {
            old.get(idx - insert_count)
                .map(|r| (r.status.clone(), r.detail.clone()))
                .unwrap_or_else(|| ("pending".into(), String::new()))
        } else {
            ("pending".into(), String::new())
        };
        new_recs.push(TaskStepRecord {
            index: (idx + 1) as u32,
            label: describe_for_record(step, &params),
            status,
            detail,
        });
    }
    *control.step_records.lock() = new_recs;
    *control.live_steps.lock() = steps.to_vec();
    {
        let mut skips = control.skip_set.lock();
        *skips = skips
            .iter()
            .map(|&s| if s >= insert_at { s + insert_count } else { s })
            .collect();
    }
}

fn update_record_label(control: &TaskControl, steps: &[Step], idx: usize) {
    let params = control.recipe_params.lock().clone();
    if let Some(rec) = control.step_records.lock().get_mut(idx) {
        rec.label = describe_for_record(&steps[idx], &params);
        rec.status = "pending".into();
        rec.detail.clear();
    }
}

fn describe_for_record(step: &Step, params: &std::collections::HashMap<String, String>) -> String {
    let args = crate::recipe::resolve_step_args(&step.args, params);
    if step.keyword == "输入" {
        format!("{} {}", step.keyword, args)
    } else if args.is_empty() {
        step.keyword.clone()
    } else {
        format!("{} {}", step.keyword, args)
    }
}

pub fn emit_live_recipe<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    steps: &[Step],
) {
    let content = steps_to_recipe_text(steps);
    let _ = app.emit(
        "live-recipe-update",
        serde_json::json!({
            "taskId": task_id,
            "content": content,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_ui_vs_dead() {
        assert_eq!(
            classify_error("等待 30s 后仍未找到「查询」"),
            ErrorClass::UiFailure
        );
        assert_eq!(
            classify_error("输入框只读，请用点击"),
            ErrorClass::UiFailure
        );
        assert_eq!(
            classify_error("刷新后页面加载超时（20s）"),
            ErrorClass::DeadPage
        );
        assert_eq!(classify_error("已取消"), ErrorClass::Cancelled);
        assert_eq!(
            classify_error("session closed: target closed"),
            ErrorClass::DeadPage
        );
    }

    #[test]
    fn serialize_or_group_steps() {
        let steps = vec![
            Step::new("打开", "https://a.com"),
            Step {
                keyword: "点击".into(),
                args: "2026-06".into(),
                or_group: Some(crate::recipe::OrGroupMeta {
                    id: 1,
                    branch: OrBranch::Try,
                }),
            },
            Step {
                keyword: "点击".into(),
                args: "新建".into(),
                or_group: Some(crate::recipe::OrGroupMeta {
                    id: 1,
                    branch: OrBranch::Fallback,
                }),
            },
            Step::new("点击", "确定"),
        ];
        let text = steps_to_recipe_text(&steps);
        assert_eq!(
            text,
            "打开https://a.com，（点击2026-06？点击新建），点击确定"
        );
    }

    #[test]
    fn reject_forbidden_repair_step() {
        assert!(validate_repair_step(&Step::new("飞书查询", "")).is_err());
        assert!(validate_repair_step(&Step::new("点击", "关闭")).is_ok());
    }

    #[test]
    fn parse_single_click() {
        let s = parse_single_repair_step("点击关闭").unwrap();
        assert_eq!(s.keyword, "点击");
        assert_eq!(s.args, "关闭");
    }

    #[test]
    fn login_page_detect() {
        assert!(looks_like_login_page(
            "https://ehr.example.com/login",
            "登录",
            &[]
        ));
        assert!(looks_like_login_page(
            "https://ehr.example.com/home",
            "首页",
            &["输入密码=".into(), "点击登录".into()]
        ));
        assert!(!looks_like_login_page(
            "https://ehr.example.com/home",
            "时间管理",
            &["点击查询".into()]
        ));
    }

    #[test]
    fn fingerprint_stable() {
        let a = cache_fingerprint(
            "EHR.Example.com",
            &Step::new("点击", "查 询"),
            "等待 30s 后仍未找到「查询」",
        );
        let b = cache_fingerprint(
            "ehr.example.com",
            &Step::new("点击", "查询"),
            "未找到可点击目标",
        );
        assert_eq!(a, b);
    }
}
