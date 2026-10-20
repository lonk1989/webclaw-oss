//! 运行日志：完整版写入产物目录，精简版推送到前端对话区。

use crate::task_run::TaskControl;
use crate::ui_host::{AppHandle, Emitter, Runtime};
use chrono::Local;
use std::time::Instant;

#[derive(Clone, serde::Serialize)]
struct LogPayload {
    lines: Vec<String>,
}

/// 本地时钟（与前端 `[HH:MM:SS.mmm]` 同精度），写入产物日志正文便于对照。
pub fn clock_stamp() -> String {
    Local::now().format("%H:%M:%S%.3f").to_string()
}

/// 为本步中间态日志追加时间戳与已耗时（成功/失败行已有「耗时」，中间节点原先没有）。
pub fn with_step_timing(line: impl AsRef<str>, step_started: Instant) -> String {
    format!(
        "{} | @ {} · 本步已耗时 {} ms",
        line.as_ref().trim_end(),
        clock_stamp(),
        step_started.elapsed().as_millis()
    )
}

/// 完整日志（写入 task control，并实时追加到内部目录 executions/）
pub fn log_full(control: &TaskControl, line: impl Into<String>) {
    let line = line.into();
    control.push_log(&line);
    crate::run_sink::emit_log(&line);
}

/// 精简日志（同时写入完整日志与全局 log-update）
pub fn log_user<R: Runtime>(
    control: &TaskControl,
    state: &crate::AppState,
    app: &AppHandle<R>,
    line: impl Into<String>,
) {
    let line = line.into();
    control.push_log(&line);
    crate::run_sink::emit_log(&line);
    state.push_log(&line);
    if !crate::run_sink::worker_mode() {
        emit_logs(app, state);
    }
}

/// 完整 + 精简各写一行（前端只见 user 行）
pub fn log_dual<R: Runtime>(
    control: &TaskControl,
    state: &crate::AppState,
    app: &AppHandle<R>,
    full: impl Into<String>,
    user: impl Into<String>,
) {
    control.push_log(full.into());
    log_user(control, state, app, user);
}

fn emit_logs<R: Runtime>(app: &AppHandle<R>, state: &crate::AppState) {
    let lines = state.logs.lock().clone();
    let _ = app.emit("log-update", LogPayload { lines });
}

pub fn format_user_header(title: &str, total: usize) -> String {
    format!("════ 开始 · 「{title}」共 {total} 步 ════")
}

pub fn format_user_footer(ok: u32, fail: u32, total_ms: u128) -> String {
    let secs = ((total_ms as f64) / 1000.0).round() as u64;
    if fail > 0 {
        format!("════ 结束 · 成功 {ok} 步，失败 {fail} 步 · 约 {secs}s ════")
    } else {
        format!("════ 完成 · 成功 {ok} 步 · 约 {secs}s ════")
    }
}

pub fn format_user_step_ok(
    idx: u32,
    total: u32,
    step_desc: &str,
    detail: &str,
    recovered: bool,
    ai_repaired: bool,
) -> String {
    let mut line = format!("步骤 {idx}/{total} 成功 · {step_desc}");
    if let Some(extra) = user_detail_hint(detail) {
        line.push_str(&format!(" · {extra}"));
    }
    if recovered {
        line.push_str(" · 已自动恢复");
    }
    if ai_repaired {
        line.push_str(" · 已 AI 修复");
    }
    line
}

pub fn format_user_step_fail(idx: u32, total: u32, step_desc: &str, err: &str) -> String {
    let err = err.trim();
    if err.is_empty() {
        format!("步骤 {idx}/{total} 失败 · {step_desc}")
    } else {
        format!("步骤 {idx}/{total} 失败 · {step_desc} · {err}")
    }
}

/// 用户可见的 AI 失败分析摘要（多行时缩进后续行）
pub fn format_user_failure_analysis(analysis: &str, usage_line: Option<&str>) -> String {
    let text = analysis.trim();
    if text.is_empty() {
        let mut out = "AI 失败分析：（空）".to_string();
        if let Some(u) = usage_line.map(str::trim).filter(|s| !s.is_empty()) {
            out.push('\n');
            out.push_str(u);
        }
        return out;
    }
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return "AI 失败分析：（空）".into();
    };
    let mut out = format!("AI 失败分析：{first}");
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    if let Some(u) = usage_line.map(str::trim).filter(|s| !s.is_empty()) {
        out.push('\n');
        out.push_str(u);
    }
    out
}

/// 写入步骤 detail 的短摘要（单行）
pub fn failure_analysis_detail_hint(analysis: &str) -> String {
    let one = analysis
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    const MAX: usize = 180;
    if one.chars().count() <= MAX {
        one
    } else {
        let truncated: String = one.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}

/// 任务进度面板用的短说明
pub fn user_step_summary(detail: &str) -> String {
    user_detail_hint(detail).unwrap_or_default()
}

fn user_detail_hint(detail: &str) -> Option<String> {
    if let Some(rest) = detail.split("客户端已下载:").nth(1) {
        let names = rest.split('|').next()?.trim();
        if !names.is_empty() {
            return Some(format!("已下载 {names}"));
        }
    }
    if let Some(rest) = detail.split("检测到文件:").nth(1) {
        let names = rest.split('|').next()?.trim();
        if !names.is_empty() {
            return Some(format!("已下载 {names}"));
        }
    }
    if let Some(rest) = detail.split("已下载:").nth(1) {
        let names = rest.split('|').next()?.trim();
        if !names.is_empty() {
            return Some(format!("已下载 {names}"));
        }
    }
    if detail.contains("已登录") || detail.contains("已处于登录态") {
        return Some("已登录".into());
    }
    if detail.contains("已打开") {
        return detail
            .split('|')
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() <= 48);
    }
    if detail.contains("产物:") {
        return detail
            .split("产物:")
            .nth(1)
            .map(|s| format!("产物 {}", s.trim()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn user_header_and_footer() {
        assert!(format_user_header("测试", 3).contains("共 3 步"));
        assert!(format_user_footer(3, 0, 14567).contains("约 15s"));
        assert!(format_user_footer(2, 1, 1000).contains("失败 1 步"));
    }

    #[test]
    fn step_ok_with_download() {
        let detail = "等待下载完成（最多 200s） | 已下载: 2026-07月报.xls | 耗时 316 ms";
        let line = format_user_step_ok(3, 3, "等待 下载", detail, false, true);
        assert!(line.contains("已下载 2026-07月报.xls"));
        assert!(line.contains("已 AI 修复"));
        assert!(!line.contains("score"));
    }

    #[test]
    fn hint_extracts_download() {
        assert_eq!(
            user_detail_hint("foo | 已下载: a.xls, b.xls | bar").as_deref(),
            Some("已下载 a.xls, b.xls")
        );
    }

    #[test]
    fn failure_analysis_user_format() {
        let line = format_user_failure_analysis(
            "可能原因：按钮不存在\n建议处理：手工点击后继续",
            Some("Token 消耗：输入 100 / 输出 20 / 合计 120"),
        );
        assert!(line.starts_with("AI 失败分析：可能原因：按钮不存在"));
        assert!(line.contains("建议处理：手工点击后继续"));
        assert!(line.contains("Token 消耗：输入 100 / 输出 20 / 合计 120"));
        let hint = failure_analysis_detail_hint("可能原因：x\n建议处理：y");
        assert!(hint.contains("可能原因：x"));
        assert!(hint.contains("建议处理：y"));
    }

    #[test]
    fn with_step_timing_includes_clock_and_elapsed() {
        let started = Instant::now() - Duration::from_millis(1200);
        let line = with_step_timing("步骤 1 本次未命中 | 等待 30s 后仍未找到", started);
        assert!(line.contains("本步已耗时"));
        assert!(line.contains("ms"));
        assert!(line.contains(" @ "));
    }
}
