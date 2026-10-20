//! 浏览器内存采样：写入执行日志，用于排查 Chrome OOM。
//! 系统侧看进程树工作集/私有字节；CDP 侧看当前页 JS 堆与 DOM 规模。

use crate::cdp::CdpClient;
use crate::chrome;
use crate::task_run::TaskControl;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct ProcessTreeSample {
    pub root_pid: u32,
    pub process_count: usize,
    /// 工作集合计（字节）
    pub working_set_bytes: u64,
    /// 私有字节合计（Commit / PrivateUsage）
    pub private_bytes: u64,
    /// 树内单进程最大工作集
    pub max_proc_ws_bytes: u64,
    pub max_proc_pid: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PageHeapSample {
    pub js_used_bytes: u64,
    pub js_total_bytes: u64,
    pub documents: Option<u64>,
    pub nodes: Option<u64>,
    pub js_event_listeners: Option<u64>,
}

/// `Performance.getMetrics` 中与内存相关的计数/字节（轻量，适合每分钟采样）。
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetricsSample {
    pub resources: Option<u64>,
    pub array_buffer_contents: Option<u64>,
    pub layout_objects: Option<u64>,
    pub frames: Option<u64>,
    pub js_heap_used: Option<u64>,
    pub js_heap_total: Option<u64>,
    pub err: Option<String>,
}

/// `performance.measureUserAgentSpecificMemory()` 汇总（较重，定时采样中间隔调用）。
#[derive(Debug, Clone, Default)]
pub struct UaMemoryBreakdown {
    pub total_bytes: Option<u64>,
    /// 按 breakdown 条目：`types` 拼接为标签，值为该条 bytes（条目互斥，可相加）。
    pub parts: Vec<(String, u64)>,
    pub unavailable: bool,
    pub err: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BrowserMemorySnapshot {
    pub port: u16,
    pub page_count: Option<usize>,
    pub process: Option<ProcessTreeSample>,
    pub page: Option<PageHeapSample>,
    pub performance: Option<PerformanceMetricsSample>,
    pub ua_memory: Option<UaMemoryBreakdown>,
    pub process_err: Option<String>,
    pub page_err: Option<String>,
}

impl BrowserMemorySnapshot {
    pub fn format_line(&self, phase: &str, prev_ws: Option<u64>) -> String {
        let mut parts = vec![format!("浏览器内存 | {phase}")];
        parts.push(format!("端口 {}", self.port));
        if let Some(n) = self.page_count {
            parts.push(format!("标签 {n}"));
        }
        if let Some(ref p) = self.process {
            let mut s = format!(
                "Chrome进程树 pid={} ×{} 工作集 {} 私有 {} 最大进程 pid={} {}",
                p.root_pid,
                p.process_count,
                fmt_bytes(p.working_set_bytes),
                fmt_bytes(p.private_bytes),
                p.max_proc_pid,
                fmt_bytes(p.max_proc_ws_bytes),
            );
            if let Some(prev) = prev_ws {
                s.push_str(&format!(" Δ{}", fmt_delta(p.working_set_bytes, prev)));
            }
            parts.push(s);
        } else if let Some(ref e) = self.process_err {
            parts.push(format!("进程采样失败: {e}"));
        }
        if let Some(ref h) = self.page {
            let mut s = format!(
                "当前页JS堆 {}/{}",
                fmt_bytes(h.js_used_bytes),
                fmt_bytes(h.js_total_bytes)
            );
            if let Some(n) = h.nodes {
                s.push_str(&format!(" DOM节点 {n}"));
            }
            if let Some(d) = h.documents {
                s.push_str(&format!(" documents {d}"));
            }
            if let Some(l) = h.js_event_listeners {
                s.push_str(&format!(" 监听器 {l}"));
            }
            parts.push(s);
        } else if let Some(ref e) = self.page_err {
            parts.push(format!("页采样失败: {e}"));
        }
        if let Some(ref perf) = self.performance {
            if let Some(ref e) = perf.err {
                parts.push(format!("Perf指标失败: {e}"));
            } else {
                let mut s = String::from("Perf指标");
                if let Some(n) = perf.resources {
                    s.push_str(&format!(" Resources={n}"));
                }
                if let Some(n) = perf.array_buffer_contents {
                    s.push_str(&format!(" ArrayBufferContents={n}"));
                }
                if let Some(n) = perf.layout_objects {
                    s.push_str(&format!(" LayoutObjects={n}"));
                }
                if let Some(n) = perf.frames {
                    s.push_str(&format!(" Frames={n}"));
                }
                if let Some(n) = perf.js_heap_used {
                    s.push_str(&format!(" JSHeapUsed={}", fmt_bytes(n)));
                }
                parts.push(s);
            }
        }
        if let Some(ref ua) = self.ua_memory {
            if ua.unavailable {
                parts.push("UA内存估算 不可用".into());
            } else if let Some(ref e) = ua.err {
                parts.push(format!("UA内存估算失败: {e}"));
            } else {
                let mut s = String::from("UA内存估算");
                if let Some(b) = ua.total_bytes {
                    s.push_str(&format!(" 合计 {}", fmt_bytes(b)));
                }
                if !ua.parts.is_empty() {
                    s.push_str(" |");
                    for (label, bytes) in &ua.parts {
                        s.push_str(&format!(" {label} {}", fmt_bytes(*bytes)));
                    }
                }
                parts.push(s);
            }
        }
        parts.join(" | ")
    }

    pub fn working_set_bytes(&self) -> Option<u64> {
        self.process.as_ref().map(|p| p.working_set_bytes)
    }
}

pub fn fmt_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2}GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1}MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.0}KiB", b / KIB)
    } else {
        format!("{bytes}B")
    }
}

fn fmt_delta(now: u64, prev: u64) -> String {
    if now >= prev {
        format!("+{}", fmt_bytes(now - prev))
    } else {
        format!("-{}", fmt_bytes(prev - now))
    }
}

/// 采集调试端口上的 Chrome 进程树 +（可选）当前附着页堆/DOM。
pub async fn sample(port: u16, client: Option<&CdpClient>) -> BrowserMemorySnapshot {
    let mut snap = BrowserMemorySnapshot {
        port,
        ..Default::default()
    };

    match chrome::list_page_targets_on(port).await {
        Ok(pages) => snap.page_count = Some(pages.len()),
        Err(_) => {}
    }

    match sample_process_tree_for_port(port) {
        Ok(p) => snap.process = Some(p),
        Err(e) => snap.process_err = Some(e),
    }

    if let Some(c) = client {
        match sample_page_heap(c).await {
            Ok(h) => snap.page = Some(h),
            Err(e) => snap.page_err = Some(e),
        }
    }

    snap
}

/// 定时采样：进程树 + 页堆 + `Performance.getMetrics`；可选 UA 内存 breakdown。
pub async fn sample_timed(
    port: u16,
    client: Option<&CdpClient>,
    ua_breakdown: bool,
) -> BrowserMemorySnapshot {
    let mut snap = sample(port, client).await;
    if let Some(c) = client {
        snap.performance = Some(sample_performance_metrics(c).await);
        if ua_breakdown {
            snap.ua_memory = Some(sample_ua_memory(c).await);
        }
    }
    snap
}

/// 任务执行期间每 60 秒写入 run-log 的定时采样间隔（秒）。
pub const MEMORY_HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// 定时采样中调用 `measureUserAgentSpecificMemory` 的间隔（心跳 tick 数，默认每 5 分钟）。
pub const MEMORY_UA_BREAKDOWN_EVERY_TICKS: u64 = 5;

const UA_MEMORY_SCRIPT: &str = r#"
(async () => {
  if (!performance.measureUserAgentSpecificMemory) {
    return { unavailable: true };
  }
  const r = await performance.measureUserAgentSpecificMemory();
  const parts = [];
  for (const e of r.breakdown || []) {
    const bytes = e.bytes || 0;
    if (!bytes) continue;
    const types = e.types || [];
    const label = types.length ? types.join("+") : "(untyped)";
    parts.push({ label, bytes });
  }
  return { bytes: r.bytes, parts };
})()
"#;

/// 后台定时采样；在验证码等待、长「等待」等阻塞期间仍会写入执行日志。
pub struct MemoryHeartbeat {
    stop: Arc<AtomicBool>,
    join: tokio::task::JoinHandle<()>,
}

impl MemoryHeartbeat {
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.join.abort();
    }
}

pub fn spawn_memory_heartbeat(
    debug_port: u16,
    task_id: String,
    active_browser: crate::browser_session::ActiveBrowserSlot,
    recipe_cancel: Arc<AtomicBool>,
    control: TaskControl,
) -> MemoryHeartbeat {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let prev_ws = Arc::new(parking_lot::Mutex::new(None::<u64>));
    let tick_secs = MEMORY_HEARTBEAT_INTERVAL_SECS;

    let join = tokio::spawn(async move {
        let mut tick: u64 = 0;
        loop {
            for _ in 0..tick_secs {
                if stop_flag.load(Ordering::SeqCst) || recipe_cancel.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            if stop_flag.load(Ordering::SeqCst) || recipe_cancel.load(Ordering::SeqCst) {
                return;
            }
            tick += 1;
            let preferred = {
                let guard = active_browser.lock();
                guard
                    .as_ref()
                    .filter(|s| s.task_id == task_id && s.port == debug_port)
                    .map(|s| s.target_id.clone())
            };
            let ua_breakdown =
                tick == 1 || tick % MEMORY_UA_BREAKDOWN_EVERY_TICKS == 0;
            let snap = match connect_for_sample(debug_port, preferred.as_deref()).await {
                Ok(client) => sample_timed(debug_port, Some(&client), ua_breakdown).await,
                Err(_) => sample_timed(debug_port, None, false).await,
            };
            let mut prev = prev_ws.lock();
            let phase = format!("定时 #{tick} · 每{tick_secs}s");
            let line = snap.format_line(&phase, *prev);
            if let Some(ws) = snap.working_set_bytes() {
                *prev = Some(ws);
            }
            control.push_log(line);
        }
    });

    MemoryHeartbeat { stop, join }
}

async fn connect_for_sample(port: u16, preferred_target_id: Option<&str>) -> Result<CdpClient, String> {
    if let Some(tid) = preferred_target_id.filter(|s| !s.is_empty()) {
        if let Ok(c) = CdpClient::connect_to_target_on(port, tid).await {
            return Ok(c);
        }
    }
    CdpClient::connect_on(port).await
}

pub fn sample_process_tree_for_port(port: u16) -> Result<ProcessTreeSample, String> {
    let root = chrome::find_listener_pid(port)
        .ok_or_else(|| format!("未找到监听端口 {port} 的进程"))?;
    sample_process_tree(root)
}

fn sample_process_tree(root_pid: u32) -> Result<ProcessTreeSample, String> {
    let pids = collect_descendant_pids(root_pid)?;
    if pids.is_empty() {
        return Err(format!("进程树为空 (pid={root_pid})"));
    }
    let mut working_set_bytes = 0u64;
    let mut private_bytes = 0u64;
    let mut max_proc_ws_bytes = 0u64;
    let mut max_proc_pid = root_pid;
    let mut counted = 0usize;
    for pid in &pids {
        if let Some((ws, priv_b)) = process_memory_bytes(*pid) {
            working_set_bytes = working_set_bytes.saturating_add(ws);
            private_bytes = private_bytes.saturating_add(priv_b);
            if ws > max_proc_ws_bytes {
                max_proc_ws_bytes = ws;
                max_proc_pid = *pid;
            }
            counted += 1;
        }
    }
    if counted == 0 {
        return Err(format!("无法读取进程内存 (root={root_pid})"));
    }
    Ok(ProcessTreeSample {
        root_pid,
        process_count: counted,
        working_set_bytes,
        private_bytes,
        max_proc_ws_bytes,
        max_proc_pid,
    })
}

async fn sample_performance_metrics(client: &CdpClient) -> PerformanceMetricsSample {
    let mut out = PerformanceMetricsSample::default();
    let _ = client.call_result("Performance.enable", json!({})).await;
    match client.call_result("Performance.getMetrics", json!({})).await {
        Ok(v) => {
            let metrics = v["metrics"].as_array().cloned().unwrap_or_default();
            out.resources = metric_u64(&metrics, "Resources");
            out.array_buffer_contents = metric_u64(&metrics, "ArrayBufferContents");
            out.layout_objects = metric_u64(&metrics, "LayoutObjects");
            out.frames = metric_u64(&metrics, "Frames");
            out.js_heap_used = metric_u64(&metrics, "JSHeapUsedSize");
            out.js_heap_total = metric_u64(&metrics, "JSHeapTotalSize");
        }
        Err(e) => out.err = Some(e),
    }
    out
}

async fn sample_ua_memory(client: &CdpClient) -> UaMemoryBreakdown {
    let mut out = UaMemoryBreakdown::default();
    match client
        .evaluate_in_context_with_timeout(
            UA_MEMORY_SCRIPT,
            None,
            Duration::from_secs(90),
        )
        .await
    {
        Ok(v) => {
            if v.get("unavailable").and_then(|x| x.as_bool()) == Some(true) {
                out.unavailable = true;
                return out;
            }
            if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
                out.err = Some(err.to_string());
                return out;
            }
            out.total_bytes = v["bytes"].as_u64().or_else(|| {
                v["bytes"].as_f64().map(|f| f as u64)
            });
            if let Some(parts) = v["parts"].as_array() {
                for p in parts {
                    let label = p["label"].as_str().unwrap_or("(untyped)").to_string();
                    let bytes = p["bytes"].as_u64().or_else(|| {
                        p["bytes"].as_f64().map(|f| f as u64)
                    }).unwrap_or(0);
                    if bytes > 0 {
                        out.parts.push((label, bytes));
                    }
                }
                out.parts.sort_by(|a, b| b.1.cmp(&a.1));
                if out.parts.len() > 6 {
                    out.parts.truncate(6);
                }
            }
        }
        Err(e) => out.err = Some(e),
    }
    out
}

fn metric_u64(metrics: &[serde_json::Value], name: &str) -> Option<u64> {
    metrics.iter().find_map(|m| {
        if m["name"].as_str() != Some(name) {
            return None;
        }
        m["value"]
            .as_u64()
            .or_else(|| m["value"].as_f64().map(|f| f as u64))
    })
}

async fn sample_page_heap(client: &CdpClient) -> Result<PageHeapSample, String> {
    let mut out = PageHeapSample::default();
    match client.call_result("Runtime.getHeapUsage", json!({})).await {
        Ok(v) => {
            out.js_used_bytes = v["usedSize"].as_u64().or_else(|| {
                v["usedSize"].as_f64().map(|f| f as u64)
            }).unwrap_or(0);
            out.js_total_bytes = v["totalSize"].as_u64().or_else(|| {
                v["totalSize"].as_f64().map(|f| f as u64)
            }).unwrap_or(0);
        }
        Err(e) => return Err(format!("Runtime.getHeapUsage: {e}")),
    }
    if let Ok(v) = client.call_result("Memory.getDOMCounters", json!({})).await {
        out.documents = v["documents"].as_u64();
        out.nodes = v["nodes"].as_u64();
        out.js_event_listeners = v["jsEventListeners"].as_u64();
    }
    Ok(out)
}

#[cfg(windows)]
fn collect_descendant_pids(root_pid: u32) -> Result<Vec<u32>, String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| format!("CreateToolhelp32Snapshot: {e}"))?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut parent_of: Vec<(u32, u32)> = Vec::new();
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                parent_of.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);

        let mut selected = std::collections::HashSet::new();
        selected.insert(root_pid);
        let mut grew = true;
        while grew {
            grew = false;
            for &(pid, ppid) in &parent_of {
                if selected.contains(&ppid) && selected.insert(pid) {
                    grew = true;
                }
            }
        }
        Ok(selected.into_iter().collect())
    }
}

#[cfg(windows)]
fn process_memory_bytes(pid: u32) -> Option<(u64, u64)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let ok = GetProcessMemoryInfo(
            handle,
            &mut counters as *mut _ as *mut _,
            counters.cb,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return None;
        }
        Some((
            counters.WorkingSetSize as u64,
            counters.PrivateUsage as u64,
        ))
    }
}

#[cfg(target_os = "linux")]
fn collect_descendant_pids(root_pid: u32) -> Result<Vec<u32>, String> {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::fs;

    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let proc = fs::read_dir("/proc").map_err(|e| format!("read /proc: {e}"))?;
    for ent in proc.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let status = fs::read_to_string(ent.path().join("status")).unwrap_or_default();
        let mut ppid = None;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("PPid:") {
                ppid = rest.trim().parse().ok();
                break;
            }
        }
        if let Some(ppid) = ppid {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut out = HashSet::new();
    let mut q = VecDeque::from([root_pid]);
    while let Some(pid) = q.pop_front() {
        if out.insert(pid) {
            if let Some(kids) = children.get(&pid) {
                q.extend(kids.iter().copied());
            }
        }
    }
    Ok(out.into_iter().collect())
}

#[cfg(target_os = "linux")]
fn process_memory_bytes(pid: u32) -> Option<(u64, u64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut rss_kb = None;
    let mut data_kb = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kb = rest.split_whitespace().next()?.parse::<u64>().ok();
        } else if let Some(rest) = line.strip_prefix("VmData:") {
            data_kb = rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    let rss = rss_kb? * 1024;
    let priv_b = data_kb.unwrap_or(rss_kb.unwrap_or(0)) * 1024;
    Some((rss, priv_b))
}

#[cfg(target_os = "macos")]
fn collect_descendant_pids(root_pid: u32) -> Result<Vec<u32>, String> {
    use std::process::Command;
    // pstree 风格：用 ps 列出 ppid/pid，再在 Rust 里展开
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .map_err(|e| format!("ps: {e}"))?;
    if !output.status.success() {
        return Err("ps 失败".into());
    }
    let mut parent_of: Vec<(u32, u32)> = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        parent_of.push((pid, ppid));
    }
    let mut selected = std::collections::HashSet::new();
    selected.insert(root_pid);
    let mut grew = true;
    while grew {
        grew = false;
        for &(pid, ppid) in &parent_of {
            if selected.contains(&ppid) && selected.insert(pid) {
                grew = true;
            }
        }
    }
    Ok(selected.into_iter().collect())
}

#[cfg(target_os = "macos")]
fn process_memory_bytes(pid: u32) -> Option<(u64, u64)> {
    use std::process::Command;
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rss_kb: u64 = String::from_utf8_lossy(&output.stdout).trim().parse().ok()?;
    let rss = rss_kb * 1024;
    // macOS 无廉价 private bytes，用 RSS 占位
    Some((rss, rss))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn collect_descendant_pids(_root_pid: u32) -> Result<Vec<u32>, String> {
    Err("当前平台不支持进程树内存采样".into())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn process_memory_bytes(_pid: u32) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_and_line() {
        assert_eq!(fmt_bytes(1536 * 1024 * 1024), "1.50GiB");
        assert!(fmt_bytes(25 * 1024 * 1024).contains("MiB"));
        let snap = BrowserMemorySnapshot {
            port: 9222,
            page_count: Some(2),
            process: Some(ProcessTreeSample {
                root_pid: 100,
                process_count: 5,
                working_set_bytes: 800 * 1024 * 1024,
                private_bytes: 600 * 1024 * 1024,
                max_proc_ws_bytes: 400 * 1024 * 1024,
                max_proc_pid: 101,
            }),
            page: Some(PageHeapSample {
                js_used_bytes: 80 * 1024 * 1024,
                js_total_bytes: 120 * 1024 * 1024,
                documents: Some(3),
                nodes: Some(12_000),
                js_event_listeners: Some(400),
            }),
            performance: Some(PerformanceMetricsSample {
                resources: Some(56),
                array_buffer_contents: Some(42),
                layout_objects: Some(1745),
                frames: Some(4),
                js_heap_used: Some(80 * 1024 * 1024),
                js_heap_total: None,
                err: None,
            }),
            ua_memory: Some(UaMemoryBreakdown {
                total_bytes: Some(60_100_000),
                parts: vec![
                    ("JavaScript".to_string(), 40_000_000),
                    ("DOM".to_string(), 100_000),
                ],
                unavailable: false,
                err: None,
            }),
            process_err: None,
            page_err: None,
        };
        let line = snap.format_line("步骤前", Some(700 * 1024 * 1024));
        assert!(line.contains("浏览器内存 | 步骤前"));
        assert!(line.contains("Δ+"));
        assert!(line.contains("DOM节点 12000"));
        assert!(line.contains("Perf指标"));
        assert!(line.contains("ArrayBufferContents=42"));
        assert!(line.contains("UA内存估算"));
        assert!(line.contains("JavaScript"));
    }
}
