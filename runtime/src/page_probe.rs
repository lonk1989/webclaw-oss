//! 页面探针：扫描并可视化结构化可点击元素与文字匹配（含跨域 OOPIF iframe）

use crate::browser_context;
use crate::browser_memory;
use crate::browser_session;
use crate::cdp::CdpClient;
use crate::chrome;
use crate::commands::scripts;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};
use crate::ui_host::{AppHandle, Emitter, Runtime, State};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// 串行化探针 CDP：UI 悬停/筛选会并发 clear+apply，多会话同时附着同一 target 会撑原生内存。
static PROBE_CDP_GATE: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// 任务执行期间禁止画 overlay（挡住开跑前已发出、稍后才返回的 set_*_overlay）。
static PROBE_OVERLAY_SUPPRESSED: AtomicBool = AtomicBool::new(false);
static PROBE_OVERLAY_GEN: AtomicU64 = AtomicU64::new(1);

struct ClickTextRemapCache {
    search: String,
    parent_id: String,
    remaps: Vec<HashMap<u32, u32>>,
}

static CLICK_TEXT_REMAP_CACHE: LazyLock<StdMutex<Option<ClickTextRemapCache>>> =
    LazyLock::new(|| StdMutex::new(None));

static LAST_PROBE_MEM_LOG: LazyLock<StdMutex<(String, Instant)>> =
    LazyLock::new(|| StdMutex::new((String::new(), Instant::now() - Duration::from_secs(60))));

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRect {
    pub page_x: f64,
    pub page_y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeCandidate {
    pub index: u32,
    #[serde(rename = "type")]
    pub element_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// 元素 class（诊断用，完整空格分隔）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cls: Option<String>,
    /// 可见短文案
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    pub desc: String,
    #[serde(default)]
    pub click_cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_cmd: Option<String>,
    /// 语义指令（如 输入开始时间= / 点击查询），可能为空
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_cmd: Option<String>,
    /// 结构序号指令（如 输入输入框#3= / 点击按钮#2）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_score: Option<f64>,
    /// 点击文字探针：综合分（仅解释用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// 点击文字探针：viewport / offscreen / excluded
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// 点击文字探针：为何计入/未计入
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 点击文字探针：硬可点击 vs 软命中（仅坐标）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_label: Option<String>,
    pub rect: ProbeRect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbePageResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default)]
    pub frame_count: u32,
    #[serde(default)]
    pub items: Vec<ProbeCandidate>,
    #[serde(default)]
    pub counts_by_type: HashMap<String, u32>,
    /// 点击文字探针：viewport | displayed（各 frame 可能不同，取主文档）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_mode: Option<String>,
    /// 点击文字探针：规则说明
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logic_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeHighlight {
    #[serde(rename = "type")]
    pub element_type: String,
    pub index: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameScanRaw {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    frame_label: Option<String>,
    #[serde(default)]
    frame_url: Option<String>,
    #[serde(default)]
    items: Vec<ProbeCandidate>,
    #[serde(default)]
    counts_by_type: HashMap<String, u32>,
    #[serde(default)]
    scan_mode: Option<String>,
    #[serde(default)]
    logic_hint: Option<String>,
}

/// 始终附着当前浏览器前台聚焦标签（不使用上次任务残留 target）。
/// `activate`：是否 `activate_tab` 置前该页签——打开/扫描探针时应为 true（与临时步骤执行前一致）；
/// overlay 悬停重绘传 false，避免每次高亮抢焦点。
async fn connect_foreground_client<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    activate: bool,
) -> Result<(CdpClient, String), String> {
    // 与临时步骤一致：先确保共用调试 Chrome 就绪
    let _ = chrome::ensure_debug_chrome_ready(&state.chrome_child, false).await?;
    if !chrome::is_debug_port_ready_on(chrome::DEBUG_PORT).await {
        return Err("请先启动 Debug Chrome 并打开目标页".into());
    }
    // macOS：允许短暂置前以突破窗口遮挡导致的 visibility 全灭
    let page = chrome::find_foreground_page_on_ex(chrome::DEBUG_PORT, true).await?;
    let client = CdpClient::connect_to_any_target_on(chrome::DEBUG_PORT, &page.id).await?;
    if activate {
        // 与「页签」收尾 / 临时步骤前台附着一致：激活该页签，避免扫到旧标签
        let _ = client.activate_tab().await;
        let _ = client.unblock_debugger().await;
    }

    // 同步状态栏与 AI 快照到真实活动页签（不占用执行中的 active 槽）
    *state.last_main_target.lock() = Some(page.id.clone());
    browser_context::store_snapshot(
        &state.last_browser_snapshot,
        browser_context::snapshot_from_client(&client),
    );
    if state.active_browser.lock().is_none() {
        browser_session::emit_browser_session(
            app,
            Some(browser_session::session_from_client("idle", &client)),
        );
    }

    Ok((client, page.id))
}

/// 探针操作后采样浏览器内存，写入活跃任务 run-log + 全局日志（便于对照 overlay 是否撑爆）。
async fn log_probe_memory_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    client: &CdpClient,
    phase: &str,
) {
    let is_clear = phase.contains("清除");
    {
        let mut last = LAST_PROBE_MEM_LOG.lock().unwrap_or_else(|e| e.into_inner());
        let min = if is_clear {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(2)
        };
        if last.0 == phase && last.1.elapsed() < min {
            return;
        }
        *last = (phase.to_string(), Instant::now());
    }
    // 清除路径不跑 UA breakdown（贵且易与「清 overlay」伪相关）；开/扫仍带 Perf。
    let snap = browser_memory::sample_timed(client.port, Some(client), false).await;
    let line = snap.format_line(&format!("探针快照 | {phase}"), None);
    state.task_registry.for_each_active(|_id, ctrl| {
        ctrl.push_log(&line);
    });
    crate::run_sink::emit_log(&line);
    state.push_log(&line);
    if !crate::run_sink::worker_mode() {
        #[derive(Clone, serde::Serialize)]
        struct LogPayload {
            lines: Vec<String>,
        }
        let lines = state.logs.lock().clone();
        let _ = app.emit("log-update", LogPayload { lines });
    }
}

fn store_click_text_remaps(search: &str, parent_id: &str, remaps: Vec<HashMap<u32, u32>>) {
    if let Ok(mut g) = CLICK_TEXT_REMAP_CACHE.lock() {
        *g = Some(ClickTextRemapCache {
            search: search.to_string(),
            parent_id: parent_id.to_string(),
            remaps,
        });
    }
}

fn take_cached_click_text_remaps(search: &str, parent_id: &str) -> Option<Vec<HashMap<u32, u32>>> {
    let g = CLICK_TEXT_REMAP_CACHE.lock().ok()?;
    let c = g.as_ref()?;
    if c.search == search && c.parent_id == parent_id {
        Some(c.remaps.clone())
    } else {
        None
    }
}

fn clear_click_text_remap_cache() {
    if let Ok(mut g) = CLICK_TEXT_REMAP_CACHE.lock() {
        *g = None;
    }
}

fn format_probe_types_phase(prefix: &str, visible_types: &[String]) -> String {
    if visible_types.is_empty() {
        format!("{prefix} | 显示:(无)")
    } else {
        format!("{prefix} | 显示:{}", visible_types.join(","))
    }
}

fn short_frame_url(url: &str) -> String {
    let u = url.trim();
    if u.is_empty() {
        return "iframe".into();
    }
    if let Ok(parsed) = url::Url::parse(u) {
        let host = parsed.host_str().unwrap_or("");
        let path = parsed.path();
        let frag = parsed.fragment().unwrap_or("");
        let mut s = if host.is_empty() {
            path.to_string()
        } else if frag.is_empty() {
            format!("{host}{path}")
        } else {
            format!("{host}{path}#{frag}")
        };
        if s.len() > 48 {
            s.truncate(45);
            s.push_str("…");
        }
        return s;
    }
    u.chars().take(48).collect()
}

fn annotate_frame_items(mut items: Vec<ProbeCandidate>, frame_hint: &str) -> Vec<ProbeCandidate> {
    for item in &mut items {
        item.frame_label = Some(frame_hint.to_string());
        if !item.desc.contains("[iframe") {
            item.desc = format!("[{frame_hint}] {}", item.desc);
        }
    }
    items
}

fn merge_counts(into: &mut HashMap<String, u32>, from: &HashMap<String, u32>) {
    for (k, v) in from {
        *into.entry(k.clone()).or_insert(0) += *v;
    }
}

fn parse_frame_scan(v: Value) -> Result<FrameScanRaw, String> {
    serde_json::from_value(v).map_err(|e| format!("探针返回格式异常: {e}"))
}

fn value_needs_probe_install(v: &Value) -> bool {
    v.get("needInstall")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

async fn eval_in_frame_ctx(
    client: &CdpClient,
    expr: &str,
    ctx: Option<u64>,
) -> Result<Value, String> {
    match ctx {
        Some(id) => client.evaluate_in_context(expr, Some(id)).await,
        None => client.evaluate(expr).await,
    }
}

/// 短调用；若页内未安装探针模块则注入一次后再调（避免每次 Runtime.evaluate 编译 ~50KB）。
async fn eval_structure_probe_call(
    client: &CdpClient,
    ctx: Option<u64>,
    call_expr: &str,
) -> Result<Value, String> {
    let v = eval_in_frame_ctx(client, call_expr, ctx).await?;
    if !value_needs_probe_install(&v) {
        return Ok(v);
    }
    let _ = eval_in_frame_ctx(client, &scripts::compose_ensure_probe_installed(), ctx).await?;
    eval_in_frame_ctx(client, call_expr, ctx).await
}

async fn eval_click_text_probe_call(
    client: &CdpClient,
    ctx: Option<u64>,
    call_expr: &str,
) -> Result<Value, String> {
    let v = eval_in_frame_ctx(client, call_expr, ctx).await?;
    if !value_needs_probe_install(&v) {
        return Ok(v);
    }
    let _ = eval_in_frame_ctx(
        client,
        &scripts::compose_ensure_click_text_probe_installed(),
        ctx,
    )
    .await?;
    eval_in_frame_ctx(client, call_expr, ctx).await
}

fn bump_offsets(offsets: &mut HashMap<String, u32>, counts: &HashMap<String, u32>) {
    for (k, v) in counts {
        *offsets.entry(k.clone()).or_insert(0) += *v;
    }
}

/// 在单个已附着客户端上扫描：先主文档连续编号，再按 FrameTree 顺序给 iframe 接续编号
async fn scan_client_contexts(
    client: &CdpClient,
    scope: Option<&str>,
    search: Option<&str>,
    offsets: &mut HashMap<String, u32>,
    types: Option<&[&str]>,
) -> Vec<FrameScanRaw> {
    let mut frames = Vec::new();
    let mut ctx_ids = client.refresh_frame_contexts().await;
    if ctx_ids.is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        ctx_ids = client.refresh_frame_contexts().await;
    }

    let scan_one = |offs: &HashMap<String, u32>| {
        scripts::compose_probe_page_filtered(scope, search, Some(offs), types)
    };

    if ctx_ids.is_empty() {
        let expr = scan_one(offsets);
        if let Ok(v) = eval_structure_probe_call(client, None, &expr).await {
            if let Ok(raw) = parse_frame_scan(v) {
                bump_offsets(offsets, &raw.counts_by_type);
                frames.push(raw);
            }
        }
        return frames;
    }

    // ctx_ids：主 frame → 子 iframe；每扫完一个文档就累加 offsets，供下一 frame 接续
    for ctx_id in ctx_ids {
        let expr = scan_one(offsets);
        if let Ok(v) = eval_structure_probe_call(client, Some(ctx_id), &expr).await {
            if let Ok(raw) = parse_frame_scan(v) {
                if raw.ok || raw.error.is_some() || !raw.items.is_empty() {
                    bump_offsets(offsets, &raw.counts_by_type);
                    frames.push(raw);
                }
            }
        }
    }

    if frames.is_empty() {
        let expr = scan_one(offsets);
        if let Ok(v) = eval_structure_probe_call(client, None, &expr).await {
            if let Ok(raw) = parse_frame_scan(v) {
                bump_offsets(offsets, &raw.counts_by_type);
                frames.push(raw);
            }
        }
    }
    frames
}

/// 按累加 offsets 依次在各 context 绘制 overlay，返回成功 frame 数
async fn apply_expr_on_client(
    client: &CdpClient,
    scope: Option<&str>,
    search: Option<&str>,
    visible_types: &[String],
    highlight: &Option<ProbeHighlight>,
    offsets: &mut HashMap<String, u32>,
    reuse_items: bool,
) -> u32 {
    let ctx_ids = if reuse_items {
        client.ordered_frame_context_ids().await
    } else {
        let mut ids = client.ordered_frame_context_ids().await;
        if ids.is_empty() {
            ids = client.refresh_frame_contexts().await;
        }
        ids
    };
    let build = |offs: &HashMap<String, u32>| {
        let payload = json!({
            "scope": scope.unwrap_or(""),
            "search": search.unwrap_or(""),
            "visibleTypes": visible_types,
            "highlight": highlight,
            "indexOffsets": offs,
            "reuseItems": reuse_items,
        });
        scripts::compose_probe_overlay(&payload)
    };

    if ctx_ids.is_empty() {
        let expr = build(offsets);
        if let Ok(v) = eval_structure_probe_call(client, None, &expr).await {
            if let Ok(raw) = parse_frame_scan(v) {
                bump_offsets(offsets, &raw.counts_by_type);
            }
            return 1;
        }
        return 0;
    }

    let mut n = 0u32;
    for ctx_id in ctx_ids {
        let expr = build(offsets);
        if let Ok(v) = eval_structure_probe_call(client, Some(ctx_id), &expr).await {
            n += 1;
            if let Ok(raw) = parse_frame_scan(v) {
                bump_offsets(offsets, &raw.counts_by_type);
            }
        }
    }
    if n == 0 {
        let expr = build(offsets);
        if let Ok(v) = eval_structure_probe_call(client, None, &expr).await {
            if let Ok(raw) = parse_frame_scan(v) {
                bump_offsets(offsets, &raw.counts_by_type);
            }
            n = 1;
        }
    }
    n
}

/// OOPIF 按父页 FrameTree URL 顺序排列，与页面 DOM 中 iframe 出现顺序对齐。
/// FrameTree 有子 frame 却 list 为空时短轮询 + Browser CDP（Linux 无头常见）。
async fn list_oopifs_stable(parent: &CdpClient, parent_id: &str) -> Vec<chrome::TargetInfo> {
    let mut iframes = chrome::discover_oopif_targets_for_parent(
        parent.port,
        parent_id,
        Some(parent),
        chrome::DiscoverOopifMode::Wait,
    )
    .await;
    if iframes.len() <= 1 {
        return iframes;
    }
    let tree_urls = parent.frame_tree_urls_dfs().await;
    iframes.sort_by_key(|t| {
        tree_urls
            .iter()
            .position(|u| u == &t.url)
            .unwrap_or(usize::MAX)
    });
    iframes
}

/// 扫描前台 page + 其 OOPIF 子 iframe target（跨域 iframe 的关键路径）
async fn scan_page_and_oopifs(
    parent: &CdpClient,
    parent_id: &str,
    scope: Option<&str>,
    search: Option<&str>,
) -> Result<ProbePageResult, String> {
    let mut offsets = HashMap::new();
    // 1) 主文档 + 同源 iframe（主先编号，iframe 接续）
    let mut all_frames = scan_client_contexts(parent, scope, search, &mut offsets, None).await;

    // 2) OOPIF 继续接续同一套 offsets
    let iframes = list_oopifs_stable(parent, parent_id).await;
    for (i, iframe) in iframes.iter().enumerate() {
        match CdpClient::connect_to_any_target_on(parent.port, &iframe.id).await {
            Ok(child) => {
                // target 出现 ≠ 可扫：等 SPA 正文/控件起来
                let _ = child
                    .wait_spa_content_ready(std::time::Duration::from_secs(3))
                    .await;
                let mut child_frames =
                    scan_client_contexts(&child, scope, search, &mut offsets, None).await;
                for raw in &mut child_frames {
                    if raw.frame_url.is_none() {
                        raw.frame_url = Some(iframe.url.clone());
                    }
                    if raw.url.is_none() {
                        raw.url = Some(iframe.url.clone());
                    }
                    raw.frame_label = Some(format!("oopif#{}", i + 1));
                }
                all_frames.extend(child_frames);
            }
            Err(_) => continue,
        }
    }

    merge_frame_scans(all_frames)
}

/// 扫描当前已附着标签（含 OOPIF），供 UI 探针面板使用
pub async fn scan_attached_page(client: &CdpClient) -> Result<ProbePageResult, String> {
    scan_page_and_oopifs(client, &client.target_id, None, None).await
}

/// AI 修复用轻量扫描：不画 overlay、不扫跨域 OOPIF、结果截断，结束后清残留
pub async fn scan_attached_page_for_repair(
    client: &CdpClient,
    max_items: usize,
) -> Result<ProbePageResult, String> {
    // 摘掉可能残留的人工探针图层（整页 absolute 层是 OOM 元凶）
    let _ = client
        .evaluate(&scripts::compose_clear_probe_overlay())
        .await;
    let _ = client
        .evaluate(&scripts::compose_clear_click_text_probe_overlay())
        .await;

    let mut offsets = HashMap::new();
    // 仅主文档 + 同源 iframe；跨域 OOPIF 另连 target，老系统上又慢又吃内存
    let frames = scan_client_contexts(client, None, None, &mut offsets, None).await;
    let mut result = merge_frame_scans(frames)?;
    if result.items.len() > max_items {
        result.items.truncate(max_items);
    }

    // 清 overlay + 打标缓存，避免修复等待 LLM 期间图层/属性常驻
    let _ = client
        .evaluate(&scripts::compose_clear_probe_overlay())
        .await;
    Ok(result)
}

/// 步骤失败落盘用：只扫图标按钮/按钮/输入框（含同源 iframe），带 class 与序号
pub const DUMP_PROBE_TYPES: &[&str] = &["图标按钮", "按钮", "输入框"];
const DUMP_PROBE_MAX_ITEMS: usize = 400;
const DUMP_PROBE_PER_TYPE: usize = 80;

pub async fn scan_attached_page_for_dump(client: &CdpClient) -> Result<ProbePageResult, String> {
    let _ = client
        .evaluate(&scripts::compose_clear_probe_overlay())
        .await;
    let _ = client
        .evaluate(&scripts::compose_clear_click_text_probe_overlay())
        .await;

    let mut offsets = HashMap::new();
    let frames =
        scan_client_contexts(client, None, None, &mut offsets, Some(DUMP_PROBE_TYPES)).await;
    let mut result = merge_frame_scans(frames)?;
    if result.items.len() > DUMP_PROBE_MAX_ITEMS {
        result.items.truncate(DUMP_PROBE_MAX_ITEMS);
    }
    Ok(result)
}

/// Authoring probe (webclaw-lite): type-filtered, optional search, truncated; no overlay / OOPIF.
pub async fn scan_attached_page_for_authoring(
    client: &CdpClient,
    types: &[&str],
    search: Option<&str>,
    max_items: usize,
) -> Result<ProbePageResult, String> {
    let _ = client
        .evaluate(&scripts::compose_clear_probe_overlay())
        .await;
    let _ = client
        .evaluate(&scripts::compose_clear_click_text_probe_overlay())
        .await;

    let mut offsets = HashMap::new();
    let type_filter = if types.is_empty() {
        None
    } else {
        Some(types)
    };
    let frames = scan_client_contexts(client, None, search, &mut offsets, type_filter).await;
    let mut result = merge_frame_scans(frames)?;
    if let Ok(meta) = client
        .evaluate("({url: location.href, title: document.title})")
        .await
    {
        if result.url.is_none() {
            result.url = meta.get("url").and_then(|v| v.as_str()).map(str::to_string);
        }
        if result.title.is_none() {
            result.title = meta
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
    }
    if result.items.len() > max_items {
        result.items.truncate(max_items);
    }
    let _ = client
        .evaluate(&scripts::compose_clear_probe_overlay())
        .await;
    Ok(result)
}

fn format_dump_item(it: &ProbeCandidate) -> String {
    let tag = it
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("-");
    let cls = it.cls.as_deref().map(str::trim).unwrap_or("");
    let class_part = if cls.is_empty() {
        "(无class)".to_string()
    } else {
        format!("class=\"{cls}\"")
    };
    let snippet = it
        .snippet
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!(" 「{s}」"))
        .unwrap_or_default();
    let frame = it
        .frame_label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("  [{s}]"))
        .unwrap_or_default();
    format!(
        "  {}#{}  {tag}  {class_part}{snippet}{frame}",
        it.element_type, it.index
    )
}

/// 将探针序号与 class 格式化为 run-log / AI 段落（仅按钮 / 图标按钮 / 输入框）
/// `phase` 例：`首次失败时` / `刷新重试后` / `最终中断时`
pub fn format_probe_index_dump(items: &[ProbeCandidate]) -> String {
    format_probe_index_dump_phase(items, "步骤失败时")
}

pub fn format_probe_index_dump_phase(items: &[ProbeCandidate], phase: &str) -> String {
    let phase = phase.trim();
    let phase = if phase.is_empty() { "步骤失败时" } else { phase };
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for it in items {
        *counts.entry(it.element_type.as_str()).or_insert(0) += 1;
    }
    let summary = DUMP_PROBE_TYPES
        .iter()
        .map(|t| format!("{t} {}", counts.get(t).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(" · ");
    let mut out = format!("探针快照 | {phase} | {summary}");
    for ty in DUMP_PROBE_TYPES {
        let typed: Vec<&ProbeCandidate> = items
            .iter()
            .filter(|it| it.element_type == *ty)
            .collect();
        if typed.is_empty() {
            continue;
        }
        out.push_str(&format!("\n── {ty} ──"));
        let shown = typed.len().min(DUMP_PROBE_PER_TYPE);
        for it in &typed[..shown] {
            out.push('\n');
            out.push_str(&format_dump_item(it));
        }
        if typed.len() > shown {
            out.push_str(&format!("\n  … 另有 {} 个未列出", typed.len() - shown));
        }
    }
    out
}

/// 扫描并格式化某一阶段的探针快照；调用方负责落盘与清理残留。
pub async fn capture_probe_phase_dump(
    client: &CdpClient,
    phase: &str,
) -> Result<(String, ProbePageResult), String> {
    let page = scan_attached_page_for_dump(client).await?;
    let dump = format_probe_index_dump_phase(&page.items, phase);
    Ok((dump, page))
}

/// 尽力清除页内探针残留（修复超时或失败后调用）
pub async fn clear_probe_residue(client: &CdpClient) {
    let _ = client
        .evaluate(&scripts::compose_clear_probe_overlay())
        .await;
    let _ = client
        .evaluate(&scripts::compose_clear_click_text_probe_overlay())
        .await;
    client.release_heavy_domains().await;
}

fn merge_frame_scans(frames: Vec<FrameScanRaw>) -> Result<ProbePageResult, String> {
    if frames.is_empty() {
        return Err("探针扫描失败：无可用执行上下文".into());
    }

    let frame_count = frames.len() as u32;
    let mut items = Vec::new();
    let mut counts_by_type = HashMap::new();
    let mut top_url = None;
    let mut top_title = None;
    let mut scope = None;
    let mut search = None;
    let mut iframe_n = 0u32;

    // 防御：先合并主文档，再合并 iframe（序号已在扫描时连续分配）
    let (main_frames, iframe_frames): (Vec<_>, Vec<_>) = frames.into_iter().partition(|raw| {
        !raw
            .frame_label
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    });

    for raw in main_frames.into_iter().chain(iframe_frames.into_iter()) {
        if !raw.ok {
            if let Some(err) = raw.error {
                if raw.frame_label.is_none() {
                    return Err(err);
                }
            }
            continue;
        }
        if top_url.is_none() && raw.frame_label.is_none() {
            top_url = raw.url.clone();
            top_title = raw.title.clone();
        }
        if scope.is_none() {
            scope = raw.scope.clone();
        }
        if search.is_none() {
            search = raw.search.clone();
        }
        merge_counts(&mut counts_by_type, &raw.counts_by_type);

        let is_iframe = raw
            .frame_label
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if is_iframe {
            iframe_n += 1;
            let hint = format!(
                "iframe#{} · {}",
                iframe_n,
                short_frame_url(raw.frame_url.as_deref().or(raw.url.as_deref()).unwrap_or(""))
            );
            items.extend(annotate_frame_items(raw.items, &hint));
        } else {
            items.extend(raw.items);
            if top_url.is_none() {
                top_url = raw.url;
                top_title = raw.title;
            }
        }
    }

    Ok(ProbePageResult {
        ok: true,
        error: None,
        url: top_url,
        title: top_title,
        scope,
        search,
        frame_count,
        items,
        counts_by_type,
        scan_mode: None,
        logic_hint: None,
    })
}

async fn apply_overlay_page_and_oopifs(
    parent: &CdpClient,
    parent_id: &str,
    scope: Option<&str>,
    search: Option<&str>,
    visible_types: &[String],
    highlight: &Option<ProbeHighlight>,
    reuse_items: bool,
) -> Result<Value, String> {
    let mut offsets = HashMap::new();
    let mut n = apply_expr_on_client(
        parent,
        scope,
        search,
        visible_types,
        highlight,
        &mut offsets,
        reuse_items,
    )
    .await;

    // reuse（悬停/筛选）：只重画已附着页，禁止再连 OOPIF（长会话累加主因之一）
    if reuse_items {
        return Ok(json!({ "ok": true, "frames": n, "oopifSkipped": true }));
    }

    let iframes = list_oopifs_stable(parent, parent_id).await;
    for iframe in iframes {
        if let Ok(child) = CdpClient::connect_to_any_target_on(parent.port, &iframe.id).await {
            n += apply_expr_on_client(
                &child,
                scope,
                search,
                visible_types,
                highlight,
                &mut offsets,
                false,
            )
            .await;
        }
    }
    Ok(json!({ "ok": true, "frames": n }))
}

async fn clear_overlay_page_and_oopifs(
    parent: &CdpClient,
    parent_id: &str,
) -> Result<Value, String> {
    let expr = scripts::compose_clear_probe_overlay();
    let n = clear_expr_light(parent, parent_id, &expr).await;
    Ok(json!({ "ok": true, "frames": n }))
}

/// 轻量清 overlay：主文档一次 evaluate（脚本内 walk 同源 iframe）；OOPIF 用 light 附着。
/// 不再 refresh/createIsolatedWorld；避免清不干净导致 scroll 监听残留闪烁。
async fn clear_expr_light(parent: &CdpClient, parent_id: &str, expr: &str) -> u32 {
    let mut n = u32::from(parent.evaluate(expr).await.is_ok());
    let iframes = list_oopifs_stable(parent, parent_id).await;
    for iframe in iframes {
        if let Ok(child) =
            CdpClient::connect_to_any_target_light_on(parent.port, &iframe.id).await
        {
            n += u32::from(child.evaluate(expr).await.is_ok());
        }
    }
    n
}

/// 任务开跑前调用：抑制后续 overlay，并轻量摘掉主文档图层（脚本内 walk 同源 iframe）。
/// 不再为 quiet-clear 去连 OOPIF：每次开跑连一遍是长会话慢爬的重要来源。
pub async fn clear_all_probe_overlays_quiet(client: &CdpClient) {
    suppress_probe_overlays(true);
    clear_click_text_remap_cache();
    let struct_expr = scripts::compose_clear_probe_overlay();
    let click_expr = scripts::compose_clear_click_text_probe_overlay();
    let _ = client.evaluate(&struct_expr).await;
    let _ = client.evaluate(&click_expr).await;
}

/// 任务结束后恢复允许探针画层（打开探针弹窗仍可扫）。
pub fn suppress_probe_overlays(suppressed: bool) {
    PROBE_OVERLAY_SUPPRESSED.store(suppressed, Ordering::SeqCst);
    if suppressed {
        PROBE_OVERLAY_GEN.fetch_add(1, Ordering::SeqCst);
    }
}

fn probe_overlays_suppressed() -> bool {
    PROBE_OVERLAY_SUPPRESSED.load(Ordering::SeqCst)
}

fn probe_overlay_gen() -> u64 {
    PROBE_OVERLAY_GEN.load(Ordering::SeqCst)
}

fn overlay_skipped_json(reason: &str) -> Value {
    json!({ "ok": true, "skipped": true, "reason": reason })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn probe_page_cmd<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    scope: Option<String>,
    search: Option<String>,
) -> Result<ProbePageResult, String> {
    let (client, parent_id) = connect_foreground_client(&app, &state, true).await?;
    let result =
        scan_page_and_oopifs(&client, &parent_id, scope.as_deref(), search.as_deref()).await?;
    let n = result.items.len();
    let frames = result.frame_count;
    log_probe_memory_snapshot(
        &app,
        &state,
        &client,
        &format!("结构扫描 | items={n} frames={frames}"),
    )
    .await;
    Ok(result)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn set_probe_overlay_cmd<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    scope: Option<String>,
    search: Option<String>,
    visible_types: Vec<String>,
    highlight: Option<ProbeHighlight>,
    reuse_items: Option<bool>,
) -> Result<Value, String> {
    if probe_overlays_suppressed() {
        return Ok(overlay_skipped_json("run-suppressed"));
    }
    let _gate = PROBE_CDP_GATE.lock().await;
    if probe_overlays_suppressed() {
        return Ok(overlay_skipped_json("run-suppressed"));
    }
    let gen = probe_overlay_gen();
    let reuse = reuse_items.unwrap_or(false);
    let (client, parent_id) = if reuse {
        connect_foreground_client_light(chrome::DEBUG_PORT).await?
    } else {
        connect_foreground_client(&app, &state, false).await?
    };
    if probe_overlays_suppressed() || probe_overlay_gen() != gen {
        let _ = clear_overlay_page_and_oopifs(&client, &parent_id).await;
        return Ok(overlay_skipped_json("stale-gen"));
    }
    let result = apply_overlay_page_and_oopifs(
        &client,
        &parent_id,
        scope.as_deref(),
        search.as_deref(),
        &visible_types,
        &highlight,
        reuse,
    )
    .await?;
    if probe_overlays_suppressed() || probe_overlay_gen() != gen {
        let _ = clear_overlay_page_and_oopifs(&client, &parent_id).await;
        return Ok(overlay_skipped_json("stale-after-apply"));
    }
    // 悬停高亮不记；reuse 重绘不记；仅 fresh 记一条
    if highlight.is_none() && !reuse {
        log_probe_memory_snapshot(
            &app,
            &state,
            &client,
            &format_probe_types_phase("结构overlay | fresh", &visible_types),
        )
        .await;
    }
    Ok(result)
}

async fn connect_foreground_client_light(
    port: u16,
) -> Result<(CdpClient, String), String> {
    let page = chrome::find_foreground_page_on_ex(port, true).await?;
    let client = CdpClient::connect_to_any_target_light_on(port, &page.id).await?;
    Ok((client, page.id))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn clear_probe_overlay_cmd<R: Runtime>(
    _app: AppHandle<R>,
    _state: State<'_, AppState>,
) -> Result<Value, String> {
    if !chrome::is_debug_port_ready_on(chrome::DEBUG_PORT).await {
        return Ok(json!({ "ok": true }));
    }
    let _gate = PROBE_CDP_GATE.lock().await;
    // 清除用 light 附着，且不打内存快照（采样本身曾与「清 overlay 暴涨」伪相关）
    let (client, parent_id) = connect_foreground_client_light(chrome::DEBUG_PORT).await?;
    clear_overlay_page_and_oopifs(&client, &parent_id).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn refresh_probe_overlay_cmd<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    scope: Option<String>,
    search: Option<String>,
    visible_types: Vec<String>,
    highlight: Option<ProbeHighlight>,
) -> Result<ProbePageResult, String> {
    let (client, parent_id) = connect_foreground_client(&app, &state, true).await?;
    let result =
        scan_page_and_oopifs(&client, &parent_id, scope.as_deref(), search.as_deref()).await?;
    let _ = apply_overlay_page_and_oopifs(
        &client,
        &parent_id,
        scope.as_deref(),
        search.as_deref(),
        &visible_types,
        &highlight,
        false,
    )
    .await?;
    let n = result.items.len();
    log_probe_memory_snapshot(
        &app,
        &state,
        &client,
        &format!(
            "{} | 刷新扫描 items={n}",
            format_probe_types_phase("结构刷新", &visible_types)
        ),
    )
    .await;
    Ok(result)
}

// ─── 点击文字解释探针（独立 overlay；跨 frame 连续 #N，得分高优先，与点击指令一致）───

fn click_text_item_is_soft(it: &ProbeCandidate) -> bool {
    if it.clickable == Some(false) {
        return true;
    }
    it.note
        .as_deref()
        .is_some_and(|n| n.contains("软命中"))
}

fn cmp_click_text_global_rank(a: &ProbeCandidate, b: &ProbeCandidate) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // 与 UI「viewport · 0.98」一致：textScore 优先；同分再比综合分、硬可点击
    let ta = a.text_score.unwrap_or(0.0);
    let tb = b.text_score.unwrap_or(0.0);
    tb.partial_cmp(&ta)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            let sa = a.score.unwrap_or(0.0);
            let sb = b.score.unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            match (click_text_item_is_soft(a), click_text_item_is_soft(b)) {
                (false, true) => Ordering::Less,
                (true, false) => Ordering::Greater,
                _ => Ordering::Equal,
            }
        })
}

fn format_click_text_index_cmd(search: &str, index: u32) -> String {
    if index <= 1 {
        format!("点击{search}")
    } else {
        format!("点击{search}#{index}")
    }
}

/// 各 frame 本地 #N → 跨 frame 全局 #N（textScore 优先，再综合分 / 硬可点击）。
fn click_text_local_to_global_maps(frames: &[FrameScanRaw]) -> Vec<HashMap<u32, u32>> {
    struct Entry {
        frame_i: usize,
        local: u32,
        soft: bool,
        score: f64,
        text_score: f64,
    }
    let mut entries = Vec::new();
    for (frame_i, frame) in frames.iter().enumerate() {
        for it in &frame.items {
            if it.element_type != "点击文字" || it.index == 0 {
                continue;
            }
            entries.push(Entry {
                frame_i,
                local: it.index,
                soft: click_text_item_is_soft(it),
                score: it.score.unwrap_or(0.0),
                text_score: it.text_score.unwrap_or(0.0),
            });
        }
    }
    entries.sort_by(|a, b| {
        use std::cmp::Ordering;
        b.text_score
            .partial_cmp(&a.text_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
            .then_with(|| match (a.soft, b.soft) {
                (false, true) => Ordering::Less,
                (true, false) => Ordering::Greater,
                _ => Ordering::Equal,
            })
            .then_with(|| a.frame_i.cmp(&b.frame_i))
            .then_with(|| a.local.cmp(&b.local))
    });
    let mut remaps: Vec<HashMap<u32, u32>> = (0..frames.len()).map(|_| HashMap::new()).collect();
    for (rank, e) in entries.into_iter().enumerate() {
        remaps[e.frame_i].insert(e.local, (rank + 1) as u32);
    }
    remaps
}

/// 合并列表：跨 frame 按得分重排并连续编号，同步 clickCmd / indexCmd。
fn renumber_merged_click_text_items(items: &mut Vec<ProbeCandidate>, search: &str) {
    let mut numbered_idx: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.element_type == "点击文字" && it.index > 0)
        .map(|(i, _)| i)
        .collect();
    numbered_idx.sort_by(|&a, &b| cmp_click_text_global_rank(&items[a], &items[b]));
    for (rank, &i) in numbered_idx.iter().enumerate() {
        let g = (rank + 1) as u32;
        let cmd = format_click_text_index_cmd(search, g);
        let it = &mut items[i];
        it.index = g;
        it.click_cmd = cmd.clone();
        it.index_cmd = Some(cmd);
        it.text_cmd = Some(format!("点击{search}"));
    }
    let mut numbered = Vec::new();
    let mut rest = Vec::new();
    for it in items.drain(..) {
        if it.element_type == "点击文字" && it.index > 0 {
            numbered.push(it);
        } else {
            rest.push(it);
        }
    }
    numbered.sort_by_key(|it| it.index);
    items.extend(numbered);
    items.extend(rest);
}

async fn scan_click_text_on_client(client: &CdpClient, search: &str) -> Vec<FrameScanRaw> {
    let mut frames = Vec::new();
    let expr = scripts::compose_click_text_probe_page(search);
    // 已附着客户端通常已有 context；勿每次 refresh（sleep + createIsolatedWorld）
    let mut ctx_ids = client.ordered_frame_context_ids().await;
    if ctx_ids.is_empty() {
        ctx_ids = client.refresh_frame_contexts().await;
    }
    if ctx_ids.is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        ctx_ids = client.refresh_frame_contexts().await;
    }
    if ctx_ids.is_empty() {
        let raw = match eval_click_text_probe_call(client, None, &expr).await {
            Ok(v) => parse_frame_scan(v).unwrap_or_else(|_| empty_click_text_frame_scan()),
            Err(_) => empty_click_text_frame_scan(),
        };
        frames.push(raw);
        return frames;
    }
    for ctx_id in ctx_ids {
        let raw = match eval_click_text_probe_call(client, Some(ctx_id), &expr).await {
            Ok(v) => parse_frame_scan(v).unwrap_or_else(|_| empty_click_text_frame_scan()),
            Err(_) => empty_click_text_frame_scan(),
        };
        frames.push(raw);
    }
    frames
}

fn empty_click_text_frame_scan() -> FrameScanRaw {
    FrameScanRaw {
        ok: true,
        error: None,
        url: None,
        title: None,
        scope: None,
        search: None,
        frame_label: None,
        frame_url: None,
        items: Vec::new(),
        counts_by_type: HashMap::new(),
        scan_mode: None,
        logic_hint: None,
    }
}

fn merge_click_text_scans(frames: Vec<FrameScanRaw>) -> Result<ProbePageResult, String> {
    if frames.is_empty() {
        return Err("点击文字探针扫描失败：无可用执行上下文".into());
    }
    let frame_count = frames.len() as u32;
    let mut items = Vec::new();
    let mut counts_by_type = HashMap::new();
    let mut top_url = None;
    let mut top_title = None;
    let mut search = None;
    let mut scan_mode = None;
    let mut logic_hints = Vec::new();
    let mut iframe_n = 0u32;

    let (main_frames, iframe_frames): (Vec<_>, Vec<_>) = frames.into_iter().partition(|raw| {
        !raw
            .frame_label
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    });

    for raw in main_frames.into_iter().chain(iframe_frames.into_iter()) {
        if !raw.ok {
            if let Some(err) = raw.error {
                if raw.frame_label.is_none() {
                    return Err(err);
                }
            }
            continue;
        }
        if top_url.is_none() && raw.frame_label.is_none() {
            top_url = raw.url.clone();
            top_title = raw.title.clone();
        }
        if search.is_none() {
            search = raw.search.clone();
        }
        if scan_mode.is_none() {
            scan_mode = raw.scan_mode.clone();
        }
        if let Some(h) = raw.logic_hint.clone() {
            if !h.is_empty() {
                logic_hints.push(h);
            }
        }
        merge_counts(&mut counts_by_type, &raw.counts_by_type);

        let is_iframe = raw
            .frame_label
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if is_iframe {
            iframe_n += 1;
            let hint = format!(
                "iframe#{} · {}",
                iframe_n,
                short_frame_url(raw.frame_url.as_deref().or(raw.url.as_deref()).unwrap_or(""))
            );
            items.extend(annotate_frame_items(raw.items, &hint));
        } else {
            items.extend(raw.items);
        }
        if top_url.is_none() {
            top_url = raw.url;
            top_title = raw.title;
        }
    }

    let search_str = search.clone().unwrap_or_default();
    if !search_str.is_empty() {
        renumber_merged_click_text_items(&mut items, &search_str);
    }

    let mut logic_hint = logic_hints.first().cloned();
    let extra = if frame_count > 1 {
        "多文档：跨 frame 连续编号；文字匹配分（textScore）高优先，同分再比综合分/硬可点击；#N 与「点击文字#N」一致。"
    } else {
        "编号：文字匹配分高优先；#N 与「点击文字#N」一致。"
    };
    logic_hint = Some(match logic_hint {
        Some(h) => format!("{h} {extra}"),
        None => extra.to_string(),
    });

    Ok(ProbePageResult {
        ok: true,
        error: None,
        url: top_url,
        title: top_title,
        scope: None,
        search,
        frame_count,
        items,
        counts_by_type,
        scan_mode,
        logic_hint,
    })
}

async fn scan_click_text_page_and_oopifs(
    parent: &CdpClient,
    parent_id: &str,
    search: &str,
) -> Result<ProbePageResult, String> {
    let mut all_frames = scan_click_text_on_client(parent, search).await;
    let iframes = list_oopifs_stable(parent, parent_id).await;
    for (i, iframe) in iframes.iter().enumerate() {
        match CdpClient::connect_to_any_target_on(parent.port, &iframe.id).await {
            Ok(child) => {
                let mut child_frames = scan_click_text_on_client(&child, search).await;
                for raw in &mut child_frames {
                    if raw.frame_url.is_none() {
                        raw.frame_url = Some(iframe.url.clone());
                    }
                    if raw.url.is_none() {
                        raw.url = Some(iframe.url.clone());
                    }
                    raw.frame_label = Some(format!("oopif#{}", i + 1));
                }
                all_frames.extend(child_frames);
            }
            Err(_) => continue,
        }
    }
    let remaps = click_text_local_to_global_maps(&all_frames);
    store_click_text_remaps(search, parent_id, remaps);
    merge_click_text_scans(all_frames)
}

async fn apply_click_text_overlay_with_map(
    client: &CdpClient,
    ctx: Option<u64>,
    search: &str,
    highlight: &Option<ProbeHighlight>,
    show_excluded: bool,
    reuse_items: bool,
    index_map: &HashMap<u32, u32>,
) -> bool {
    let map_json: HashMap<String, u32> = index_map
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect();
    let payload = json!({
        "search": search,
        "highlight": highlight,
        "showExcluded": show_excluded,
        "reuseItems": reuse_items,
        "indexMap": map_json,
    });
    let expr = scripts::compose_click_text_probe_overlay(&payload);
    eval_click_text_probe_call(client, ctx, &expr).await.is_ok()
}

/// 与 `scan_click_text_on_client` 相同的 frame 访问顺序，便于本地→全局 indexMap 对齐。
async fn apply_click_text_overlay_on_client_mapped(
    client: &CdpClient,
    search: &str,
    highlight: &Option<ProbeHighlight>,
    show_excluded: bool,
    reuse_items: bool,
    remaps: &mut std::vec::IntoIter<HashMap<u32, u32>>,
) -> u32 {
    // reuse / light OOPIF：禁止 refresh（会 createIsolatedWorld）；已有 map 或主世界即可重画
    let mut ctx_ids = client.ordered_frame_context_ids().await;
    if ctx_ids.is_empty() && !reuse_items {
        ctx_ids = client.refresh_frame_contexts().await;
        if ctx_ids.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            ctx_ids = client.refresh_frame_contexts().await;
        }
    }
    if ctx_ids.is_empty() {
        let map = remaps.next().unwrap_or_default();
        return u32::from(
            apply_click_text_overlay_with_map(
                client,
                None,
                search,
                highlight,
                show_excluded,
                reuse_items,
                &map,
            )
            .await,
        );
    }
    let mut n = 0u32;
    for ctx_id in ctx_ids {
        let map = remaps.next().unwrap_or_default();
        n += u32::from(
            apply_click_text_overlay_with_map(
                client,
                Some(ctx_id),
                search,
                highlight,
                show_excluded,
                reuse_items,
                &map,
            )
            .await,
        );
    }
    n
}

async fn apply_click_text_overlay_page_and_oopifs(
    parent: &CdpClient,
    parent_id: &str,
    search: &str,
    highlight: &Option<ProbeHighlight>,
    show_excluded: bool,
    reuse_items: bool,
) -> Result<Value, String> {
    // reuse：页内 cache 已有全局 #N，只重画当前附着页；禁止再连 OOPIF
    if reuse_items {
        let _ = parent_id;
        let empty = HashMap::new();
        let mut n = 0u32;
        let ctx_ids = parent.ordered_frame_context_ids().await;
        if ctx_ids.is_empty() {
            n += u32::from(
                apply_click_text_overlay_with_map(
                    parent,
                    None,
                    search,
                    highlight,
                    show_excluded,
                    true,
                    &empty,
                )
                .await,
            );
        } else {
            for ctx_id in ctx_ids {
                n += u32::from(
                    apply_click_text_overlay_with_map(
                        parent,
                        Some(ctx_id),
                        search,
                        highlight,
                        show_excluded,
                        true,
                        &empty,
                    )
                    .await,
                );
            }
        }
        return Ok(json!({ "ok": true, "frames": n, "oopifSkipped": true }));
    }

    let (remaps, children) = scan_and_connect_click_text_frames(parent, parent_id, search).await;
    let mut remap_iter = remaps.into_iter();
    let mut n = apply_click_text_overlay_on_client_mapped(
        parent,
        search,
        highlight,
        show_excluded,
        false,
        &mut remap_iter,
    )
    .await;
    for child in &children {
        n += apply_click_text_overlay_on_client_mapped(
            child,
            search,
            highlight,
            show_excluded,
            false,
            &mut remap_iter,
        )
        .await;
    }
    Ok(json!({ "ok": true, "frames": n }))
}

/// 全页扫 click-text + 连 OOPIF；写入 remaps 缓存供悬停 reuse。
/// 扫描需要子 frame context → Full 附着一次；清/reuse 走 light，避免反复造隔离世界。
async fn scan_and_connect_click_text_frames(
    parent: &CdpClient,
    parent_id: &str,
    search: &str,
) -> (Vec<HashMap<u32, u32>>, Vec<CdpClient>) {
    let mut all_frames = scan_click_text_on_client(parent, search).await;
    let iframes = list_oopifs_stable(parent, parent_id).await;
    let mut children = Vec::new();
    for iframe in &iframes {
        if let Ok(child) = CdpClient::connect_to_any_target_on(parent.port, &iframe.id).await {
            let child_frames = scan_click_text_on_client(&child, search).await;
            all_frames.extend(child_frames);
            children.push(child);
        }
    }
    let remaps = click_text_local_to_global_maps(&all_frames);
    store_click_text_remaps(search, parent_id, remaps.clone());
    (remaps, children)
}

async fn clear_click_text_overlay_page_and_oopifs(
    parent: &CdpClient,
    parent_id: &str,
) -> Result<Value, String> {
    clear_click_text_remap_cache();
    let expr = scripts::compose_clear_click_text_probe_overlay();
    let n = clear_expr_light(parent, parent_id, &expr).await;
    Ok(json!({ "ok": true, "frames": n }))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn probe_click_text_cmd<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    search: String,
) -> Result<ProbePageResult, String> {
    let search = search.trim().to_string();
    if search.is_empty() {
        return Err("请输入点击文字（如 HCM、导出）".into());
    }
    let (client, parent_id) = connect_foreground_client(&app, &state, true).await?;
    // 避免与页面探针 overlay 叠画
    let _ = clear_overlay_page_and_oopifs(&client, &parent_id).await;
    let result = scan_click_text_page_and_oopifs(&client, &parent_id, &search).await?;
    let n = result.items.len();
    let q = if search.chars().count() > 24 {
        format!("{}…", search.chars().take(24).collect::<String>())
    } else {
        search.clone()
    };
    log_probe_memory_snapshot(
        &app,
        &state,
        &client,
        &format!("点击文字扫描 | 「{q}」 items={n} frames={}", result.frame_count),
    )
    .await;
    Ok(result)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn set_click_text_probe_overlay_cmd<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    search: String,
    show_excluded: Option<bool>,
    highlight: Option<ProbeHighlight>,
    reuse_items: Option<bool>,
) -> Result<Value, String> {
    let search = search.trim().to_string();
    if search.is_empty() {
        return Err("请输入点击文字".into());
    }
    if probe_overlays_suppressed() {
        return Ok(overlay_skipped_json("run-suppressed"));
    }
    let _gate = PROBE_CDP_GATE.lock().await;
    if probe_overlays_suppressed() {
        return Ok(overlay_skipped_json("run-suppressed"));
    }
    let gen = probe_overlay_gen();
    let reuse = reuse_items.unwrap_or(false);
    let (client, parent_id) = if reuse {
        connect_foreground_client_light(chrome::DEBUG_PORT).await?
    } else {
        connect_foreground_client(&app, &state, false).await?
    };
    if probe_overlays_suppressed() || probe_overlay_gen() != gen {
        let _ = clear_click_text_overlay_page_and_oopifs(&client, &parent_id).await;
        return Ok(overlay_skipped_json("stale-gen"));
    }
    // 悬停复用时不要先清结构 overlay
    if !reuse {
        let _ = clear_overlay_page_and_oopifs(&client, &parent_id).await;
    }
    let result = apply_click_text_overlay_page_and_oopifs(
        &client,
        &parent_id,
        &search,
        &highlight,
        show_excluded.unwrap_or(true),
        reuse,
    )
    .await?;
    if probe_overlays_suppressed() || probe_overlay_gen() != gen {
        let _ = clear_click_text_overlay_page_and_oopifs(&client, &parent_id).await;
        return Ok(overlay_skipped_json("stale-after-apply"));
    }
    if highlight.is_none() && !reuse {
        let excl = if show_excluded.unwrap_or(true) {
            "含未计入"
        } else {
            "仅计入"
        };
        let q = if search.chars().count() > 24 {
            format!("{}…", search.chars().take(24).collect::<String>())
        } else {
            search.clone()
        };
        log_probe_memory_snapshot(
            &app,
            &state,
            &client,
            &format!("点击文字overlay | 「{q}」 {excl} | fresh"),
        )
        .await;
    }
    Ok(result)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn clear_click_text_probe_overlay_cmd<R: Runtime>(
    _app: AppHandle<R>,
    _state: State<'_, AppState>,
) -> Result<Value, String> {
    if !chrome::is_debug_port_ready_on(chrome::DEBUG_PORT).await {
        return Ok(json!({ "ok": true }));
    }
    let _gate = PROBE_CDP_GATE.lock().await;
    let (client, parent_id) = connect_foreground_client_light(chrome::DEBUG_PORT).await?;
    clear_click_text_overlay_page_and_oopifs(&client, &parent_id).await
}

/// 打开探针弹窗前置：与临时步骤执行前相同——附着并激活当前浏览器活跃页签。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn probe_use_active_tab_cmd<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (client, _) = connect_foreground_client(&app, &state, true).await?;
    let title = if client.target_title.is_empty() {
        "(无标题)"
    } else {
        client.target_title.as_str()
    };
    log_probe_memory_snapshot(&app, &state, &client, "打开探针/附着活动页签").await;
    Ok(format!(
        "使用当前浏览器活跃页签「{}」| {}",
        title, client.target_url
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(ty: &str, index: u32, tag: &str, cls: &str, snippet: &str) -> ProbeCandidate {
        ProbeCandidate {
            index,
            element_type: ty.into(),
            tag: Some(tag.into()),
            cls: Some(cls.into()),
            snippet: if snippet.is_empty() {
                None
            } else {
                Some(snippet.into())
            },
            desc: String::new(),
            click_cmd: String::new(),
            input_cmd: None,
            text_cmd: None,
            index_cmd: None,
            text_score: None,
            score: None,
            pool: None,
            note: None,
            clickable: None,
            frame_label: None,
            rect: ProbeRect {
                page_x: 0.0,
                page_y: 0.0,
                width: 8.0,
                height: 8.0,
            },
        }
    }

    #[test]
    fn dump_lists_index_and_class() {
        let items = vec![
            item("图标按钮", 9, "DIV", "user-box-banner", ""),
            item("图标按钮", 10, "DIV", "input-icon icon-hcm-screening", ""),
            item("按钮", 1, "BUTTON", "btn-pc primary", "确定"),
            item("输入框", 1, "INPUT", "component-input-inner", ""),
        ];
        let dump = format_probe_index_dump(&items);
        assert!(dump.contains("探针快照 | 步骤失败时 | 图标按钮 2 · 按钮 1 · 输入框 1"));
        assert!(dump.contains("图标按钮#9  DIV  class=\"user-box-banner\""));
        assert!(dump.contains("图标按钮#10  DIV  class=\"input-icon icon-hcm-screening\""));
        assert!(dump.contains("按钮#1  BUTTON  class=\"btn-pc primary\" 「确定」"));
        assert!(dump.contains("输入框#1  INPUT  class=\"component-input-inner\""));
        assert!(!dump.contains("表格"));
        let phased = format_probe_index_dump_phase(&items, "首次失败时");
        assert!(phased.contains("探针快照 | 首次失败时 |"));
    }

    #[test]
    fn click_text_global_rank_prefers_higher_text_score_across_frames() {
        let mut items = vec![
            ProbeCandidate {
                index: 1,
                element_type: "点击文字".into(),
                tag: None,
                cls: None,
                snippet: None,
                desc: "main".into(),
                click_cmd: "点击作业过程监管".into(),
                input_cmd: None,
                text_cmd: Some("点击作业过程监管".into()),
                index_cmd: Some("点击作业过程监管".into()),
                text_score: Some(0.98),
                // 综合分更高也不应压过更高的 textScore
                score: Some(1.2),
                pool: Some("viewport".into()),
                note: Some("计入序号（视口内）".into()),
                clickable: Some(true),
                frame_label: None,
                rect: ProbeRect {
                    page_x: 0.0,
                    page_y: 0.0,
                    width: 8.0,
                    height: 8.0,
                },
            },
            ProbeCandidate {
                index: 1,
                element_type: "点击文字".into(),
                tag: None,
                cls: None,
                snippet: None,
                desc: "iframe".into(),
                click_cmd: "点击作业过程监管".into(),
                input_cmd: None,
                text_cmd: Some("点击作业过程监管".into()),
                index_cmd: Some("点击作业过程监管".into()),
                text_score: Some(1.0),
                score: Some(0.5),
                pool: Some("viewport".into()),
                note: Some("计入序号（视口内）".into()),
                clickable: Some(true),
                frame_label: Some("iframe#1".into()),
                rect: ProbeRect {
                    page_x: 0.0,
                    page_y: 0.0,
                    width: 8.0,
                    height: 8.0,
                },
            },
        ];
        renumber_merged_click_text_items(&mut items, "作业过程监管");
        assert_eq!(items[0].index, 1);
        assert_eq!(items[0].desc, "iframe");
        assert_eq!(items[0].click_cmd, "点击作业过程监管");
        assert_eq!(items[1].index, 2);
        assert_eq!(items[1].desc, "main");
        assert_eq!(items[1].click_cmd, "点击作业过程监管#2");
    }
}
