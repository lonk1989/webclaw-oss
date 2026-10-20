//! 能力脚本跨 frame 求值：主文档 + OOPIF（跨域 iframe 独立 CDP target）

use crate::cdp::CdpClient;
use crate::chrome;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::task::JoinSet;

/// 单个 iframe 连接+求值上限（须 ≥ 脚本内 elementWait ~15s + SPA 就绪）
const IFRAME_EVAL_TIMEOUT: Duration = Duration::from_secs(20);
/// 主文档求值上限（点击/输入扇出；与脚本完整等待对齐）
pub const PARENT_EVAL_TIMEOUT: Duration = Duration::from_secs(20);
/// 壳层父页 peek：菜单点击等，不必为 OOPIF 白等
pub const PARENT_PEEK_TIMEOUT: Duration = Duration::from_millis(1500);
/// OOPIF SPA 内容就绪上限（target 出现 ≠ 可点）
const OOPIF_SPA_READY_TIMEOUT: Duration = Duration::from_secs(3);
/// 全局 `类型#N` 定位：与点击脚本默认等待对齐（Linux OOPIF 晚于父页就绪）
const STRUCTURAL_LOCATE_TIMEOUT: Duration = Duration::from_secs(15);
const STRUCTURAL_LOCATE_POLL: Duration = Duration::from_millis(300);
/// 非扇出点击/输入：脚本内最多等 15s + 遮罩，须大于脚本等待
pub const SCRIPT_WAIT_EVAL_TIMEOUT: Duration = Duration::from_secs(20);
/// sticky / 热页面短探：脚本内短等 + CDP 上限
pub const STICKY_ELEMENT_WAIT_SECS: f64 = 5.0;
/// 热路径回退扇出时的元素等待（页面已暖，勿再烧满 15s）
pub const WARM_FANOUT_ELEMENT_WAIT_SECS: f64 = 5.0;
pub const STICKY_TRY_EVAL_TIMEOUT: Duration = Duration::from_secs(8);
/// 父页短探 CDP 上限（与 sticky 并行，避免主页操作先空等 iframe）
pub const PARENT_SHORT_TRY_TIMEOUT: Duration = Duration::from_secs(6);
/// 同源子 frame 优先短探：元素已在 DOM 时毫秒级返回；未命中勿烧满 15s
const SAME_ORIGIN_EARLY_TIMEOUT: Duration = Duration::from_secs(4);
/// sticky 优先试探时的 iframe 连接上限（含短脚本等待）
const STICKY_IFRAME_EVAL_TIMEOUT: Duration = Duration::from_secs(10);
/// 得分达到该阈值即可提前结束（不必等慢/挂起的其它 frame）
const DECISIVE_SCORE: f64 = 1.0;
/// 高分命中后短暂等待，看其它已在飞的 frame 是否更高分
const GRACE_AFTER_DECISIVE: Duration = Duration::from_millis(120);

#[derive(Debug)]
pub struct ScriptEvalHit {
    pub value: Value,
    /// 命中所在 CDP target（page 或 iframe）
    pub target_id: String,
    pub is_parent: bool,
    /// 同进程子 frame 的 executionContextId；点击时需把坐标加上 iframe 在父页中的偏移
    pub context_id: Option<u64>,
}

/// 全局 `类型#N` 映射到具体 frame 上的本地序号（与探针连续编号一致）
#[derive(Debug, Clone)]
pub struct StructuralLocalHit {
    pub local_index: u32,
    pub target_id: String,
    pub context_id: Option<u64>,
    pub is_parent: bool,
}

/// 点击文字全局 #N 定位超时（与结构化对齐）
const CLICK_TEXT_LOCATE_TIMEOUT: Duration = Duration::from_secs(15);
const CLICK_TEXT_LOCATE_POLL: Duration = Duration::from_millis(300);

fn count_from_value(v: &Value) -> u32 {
    v.get("count").and_then(|x| x.as_u64()).unwrap_or(0) as u32
}

async fn list_oopifs_stable_for(
    parent: &CdpClient,
    parent_id: &str,
    mode: chrome::DiscoverOopifMode,
) -> Vec<chrome::TargetInfo> {
    let mut iframes =
        chrome::discover_oopif_targets_for_parent(parent.port, parent_id, Some(parent), mode).await;
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

/// 任务内热路径：优先复用最近成功的 OOPIF / 父页表面，避免每步 Peek→Wait + 满超时扇出。
#[derive(Debug, Clone, Default)]
pub struct FrameHotPath {
    pub preferred_oopif: Option<String>,
    /// 该 OOPIF 近期已成功交互过：可跳过 SPA 就绪轮询
    pub preferred_warm: bool,
    /// 最近命中在父页：应先短探父页，勿串行空等 sticky iframe
    pub prefer_parent: bool,
}

/// 按探针相同顺序（主文档 → 同源 iframe → OOPIF）定位全局 `类型#N` 所属 frame 与本地序号。
///
/// 与点击脚本不同：结构化路径先在 Rust 侧做全局→本地映射，再进页面脚本。
/// 若只扫一次，父页已有 8 个、iframe 尚未暴露时会立刻 `未找到…共 8 个` → AI 修复，
/// 永远走不到脚本内 30s 等待。Linux OOPIF 晚就绪时尤其明显，故此处轮询到超时。
pub async fn locate_global_structural(
    parent: &CdpClient,
    type_name: &str,
    global_index: u32,
) -> Result<StructuralLocalHit, String> {
    locate_global_structural_with_hot(
        parent,
        type_name,
        global_index,
        &FrameHotPath::default(),
        None,
    )
    .await
}

pub async fn locate_global_structural_with_hot(
    parent: &CdpClient,
    type_name: &str,
    global_index: u32,
    hot: &FrameHotPath,
    cancel: Option<&AtomicBool>,
) -> Result<StructuralLocalHit, String> {
    if global_index < 1 {
        return Err("结构化序号从 1 开始".into());
    }
    if cancelled(cancel) {
        return Err("已取消".into());
    }
    // sticky 热 iframe：先只扫该 target（跳过 SPA），命中则免 15s 全局轮询
    if let Some(pref) = hot.preferred_oopif.as_deref() {
        if let Ok(hit) =
            try_locate_on_oopif(parent, type_name, global_index, pref, hot.preferred_warm).await
        {
            return Ok(hit);
        }
    }
    let started = Instant::now();
    let deadline = started + STRUCTURAL_LOCATE_TIMEOUT;
    let mut last_err = String::new();
    loop {
        if cancelled(cancel) {
            return Err("已取消".into());
        }
        match locate_global_structural_once(parent, type_name, global_index, hot).await {
            Ok(hit) => return Ok(hit),
            Err(e) => {
                last_err = e;
                if Instant::now() >= deadline {
                    break;
                }
                // 短睡可打断，避免点停止后仍空等满 15s
                let slice = STRUCTURAL_LOCATE_POLL.min(Duration::from_millis(100));
                let mut waited = Duration::ZERO;
                while waited < STRUCTURAL_LOCATE_POLL {
                    if cancelled(cancel) {
                        return Err("已取消".into());
                    }
                    tokio::time::sleep(slice).await;
                    waited += slice;
                }
            }
        }
    }
    let waited_ms = started.elapsed().as_millis();
    let waited_s = (waited_ms as f64 / 1000.0).round() as u64;
    let count_hint = candidate_count_from_locate_err(&last_err)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());
    Err(format!(
        "等待 {waited_s}s 后仍未找到{type_name}#{global_index}（跨 frame 共 {count_hint} 个候选；实际轮询 {waited_ms} ms）"
    ))
}

fn candidate_count_from_locate_err(err: &str) -> Option<u32> {
    let marker = "共 ";
    let i = err.find(marker)?;
    let rest = &err[i + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

async fn locate_global_structural_once(
    parent: &CdpClient,
    type_name: &str,
    global_index: u32,
    hot: &FrameHotPath,
) -> Result<StructuralLocalHit, String> {
    let label = format!("{type_name}#{global_index}");
    let stamp_expr = crate::commands::scripts::compose_find_probe_stamp(&label);
    let count_expr = crate::commands::scripts::compose_structural_count(type_name);
    let mut offset = 0u32;
    let parent_id = parent.target_id.clone();

    // 1) 优先按打标查找（窗口缩放折行后仍指向同一元素）
    let mut ctx_ids = parent.refresh_frame_contexts().await;
    if ctx_ids.is_empty() {
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx_ids = parent.refresh_frame_contexts().await;
    }
    if ctx_ids.is_empty() {
        if let Ok(v) = parent.evaluate(&stamp_expr).await {
            if v.get("found").and_then(|x| x.as_bool()) == Some(true) {
                return Ok(StructuralLocalHit {
                    local_index: global_index,
                    target_id: parent_id.clone(),
                    context_id: None,
                    is_parent: true,
                });
            }
        }
    } else {
        for ctx_id in &ctx_ids {
            if let Ok(v) = parent.evaluate_in_context(&stamp_expr, Some(*ctx_id)).await {
                if v.get("found").and_then(|x| x.as_bool()) == Some(true) {
                    return Ok(StructuralLocalHit {
                        local_index: global_index,
                        target_id: parent_id.clone(),
                        context_id: Some(*ctx_id),
                        is_parent: true,
                    });
                }
            }
        }
    }
    let mut oopifs =
        list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Peek).await;
    prefer_oopif_first(&mut oopifs, hot.preferred_oopif.as_deref());
    if oopifs.is_empty() {
        if let Some(pref) = hot.preferred_oopif.as_deref() {
            if let Ok(hit) =
                try_locate_on_oopif(parent, type_name, global_index, pref, hot.preferred_warm)
                    .await
            {
                return Ok(hit);
            }
        }
        oopifs =
            list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Wait).await;
        prefer_oopif_first(&mut oopifs, hot.preferred_oopif.as_deref());
    }
    for iframe in oopifs {
        let skip_spa = hot.preferred_warm
            && hot
                .preferred_oopif
                .as_deref()
                .is_some_and(|p| p == iframe.id);
        if let Ok(hit) = locate_stamp_on_oopif(
            parent.port,
            &iframe.id,
            &stamp_expr,
            global_index,
            skip_spa,
        )
        .await
        {
            return Ok(hit);
        }
    }

    // 2) 无打标时回退：按候选数量映射全局→本地（与探针首次编号一致）
    let expr = count_expr;
    let mut ctx_ids = parent.refresh_frame_contexts().await;
    if ctx_ids.is_empty() {
        ctx_ids = parent.refresh_frame_contexts().await;
    }

    if ctx_ids.is_empty() {
        let v = parent.evaluate(&expr).await.unwrap_or(Value::Null);
        let count = count_from_value(&v);
        if global_index <= count {
            return Ok(StructuralLocalHit {
                local_index: global_index,
                target_id: parent_id,
                context_id: None,
                is_parent: true,
            });
        }
        offset = count;
    } else {
        for ctx_id in ctx_ids {
            let v = parent
                .evaluate_in_context(&expr, Some(ctx_id))
                .await
                .unwrap_or(Value::Null);
            let count = count_from_value(&v);
            if global_index > offset && global_index <= offset + count {
                return Ok(StructuralLocalHit {
                    local_index: global_index - offset,
                    target_id: parent_id.clone(),
                    context_id: Some(ctx_id),
                    is_parent: true,
                });
            }
            offset += count;
        }
    }

    let mut oopifs =
        list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Peek).await;
    prefer_oopif_first(&mut oopifs, hot.preferred_oopif.as_deref());
    if oopifs.is_empty() {
        oopifs =
            list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Wait).await;
        prefer_oopif_first(&mut oopifs, hot.preferred_oopif.as_deref());
    }
    for iframe in oopifs {
        let skip_spa = hot.preferred_warm
            && hot
                .preferred_oopif
                .as_deref()
                .is_some_and(|p| p == iframe.id);
        let Ok(child) = CdpClient::connect_to_any_target_on(parent.port, &iframe.id).await else {
            continue;
        };
        if !skip_spa {
            let _ = child.wait_spa_content_ready(OOPIF_SPA_READY_TIMEOUT).await;
        }
        let mut child_ctx = child.refresh_frame_contexts().await;
        if child_ctx.is_empty() {
            let v = child.evaluate(&expr).await.unwrap_or(Value::Null);
            let count = count_from_value(&v);
            if global_index > offset && global_index <= offset + count {
                return Ok(StructuralLocalHit {
                    local_index: global_index - offset,
                    target_id: iframe.id,
                    context_id: None,
                    is_parent: false,
                });
            }
            offset += count;
            continue;
        }
        for ctx_id in child_ctx.drain(..) {
            let v = child
                .evaluate_in_context(&expr, Some(ctx_id))
                .await
                .unwrap_or(Value::Null);
            let count = count_from_value(&v);
            if global_index > offset && global_index <= offset + count {
                return Ok(StructuralLocalHit {
                    local_index: global_index - offset,
                    target_id: iframe.id.clone(),
                    context_id: Some(ctx_id),
                    is_parent: false,
                });
            }
            offset += count;
        }
    }

    Err(format!(
        "未找到{type_name}#{global_index}（跨 frame 共 {offset} 个候选）"
    ))
}

fn prefer_oopif_first(iframes: &mut Vec<chrome::TargetInfo>, preferred: Option<&str>) {
    let Some(pref) = preferred.filter(|s| !s.is_empty()) else {
        return;
    };
    if let Some(i) = iframes.iter().position(|t| t.id == pref) {
        if i > 0 {
            let t = iframes.remove(i);
            iframes.insert(0, t);
        }
        return;
    }
    // Peek/Wait 未列出但 sticky 仍可能存活：注入占位，避免退化成只扫父页
    iframes.insert(
        0,
        chrome::TargetInfo {
            id: pref.to_string(),
            kind: "iframe".into(),
            title: String::new(),
            url: String::new(),
            parent_id: None,
        },
    );
}

async fn locate_stamp_on_oopif(
    port: u16,
    target_id: &str,
    stamp_expr: &str,
    global_index: u32,
    skip_spa: bool,
) -> Result<StructuralLocalHit, String> {
    let child = CdpClient::connect_to_any_target_on(port, target_id).await?;
    if !skip_spa {
        let _ = child.wait_spa_content_ready(OOPIF_SPA_READY_TIMEOUT).await;
    }
    let child_ctx = child.refresh_frame_contexts().await;
    if child_ctx.is_empty() {
        let v = child.evaluate(stamp_expr).await?;
        if v.get("found").and_then(|x| x.as_bool()) == Some(true) {
            return Ok(StructuralLocalHit {
                local_index: global_index,
                target_id: target_id.to_string(),
                context_id: None,
                is_parent: false,
            });
        }
        return Err("stamp miss".into());
    }
    for ctx_id in child_ctx {
        let v = child.evaluate_in_context(stamp_expr, Some(ctx_id)).await?;
        if v.get("found").and_then(|x| x.as_bool()) == Some(true) {
            return Ok(StructuralLocalHit {
                local_index: global_index,
                target_id: target_id.to_string(),
                context_id: Some(ctx_id),
                is_parent: false,
            });
        }
    }
    Err("stamp miss".into())
}

async fn try_locate_on_oopif(
    parent: &CdpClient,
    type_name: &str,
    global_index: u32,
    target_id: &str,
    skip_spa: bool,
) -> Result<StructuralLocalHit, String> {
    let label = format!("{type_name}#{global_index}");
    let stamp_expr = crate::commands::scripts::compose_find_probe_stamp(&label);
    if let Ok(hit) =
        locate_stamp_on_oopif(parent.port, target_id, &stamp_expr, global_index, skip_spa).await
    {
        return Ok(hit);
    }
    let count_expr = crate::commands::scripts::compose_structural_count(type_name);
    let child = CdpClient::connect_to_any_target_on(parent.port, target_id).await?;
    if !skip_spa {
        let _ = child.wait_spa_content_ready(OOPIF_SPA_READY_TIMEOUT).await;
    }
    let mut child_ctx = child.refresh_frame_contexts().await;
    if child_ctx.is_empty() {
        let v = child.evaluate(&count_expr).await.unwrap_or(Value::Null);
        let count = count_from_value(&v);
        if global_index >= 1 && global_index <= count {
            return Ok(StructuralLocalHit {
                local_index: global_index,
                target_id: target_id.to_string(),
                context_id: None,
                is_parent: false,
            });
        }
    } else {
        let mut offset = 0u32;
        for ctx_id in child_ctx.drain(..) {
            let v = child
                .evaluate_in_context(&count_expr, Some(ctx_id))
                .await
                .unwrap_or(Value::Null);
            let count = count_from_value(&v);
            if global_index > offset && global_index <= offset + count {
                return Ok(StructuralLocalHit {
                    local_index: global_index - offset,
                    target_id: target_id.to_string(),
                    context_id: Some(ctx_id),
                    is_parent: false,
                });
            }
            offset += count;
        }
    }
    Err("preferred oopif miss".into())
}

#[derive(Clone)]
struct ClickTextCatalogEntry {
    local_index: u32,
    score: f64,
    text_score: f64,
    clickable: bool,
    target_id: String,
    context_id: Option<u64>,
    is_parent: bool,
}

fn parse_click_text_catalog(v: &Value) -> Vec<(u32, f64, f64, bool)> {
    let Some(hits) = v.get("hits").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for h in hits {
        let local = h
            .get("localIndex")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32;
        if local < 1 {
            continue;
        }
        let score = h.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let text_score = h.get("textScore").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let clickable = h
            .get("clickable")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        out.push((local, score, text_score, clickable));
    }
    out
}

fn cmp_click_text_catalog(a: &ClickTextCatalogEntry, b: &ClickTextCatalogEntry) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // 与文字探针列表一致：textScore 优先（UI 显示的得分）
    b.text_score
        .partial_cmp(&a.text_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
        .then_with(|| match (a.clickable, b.clickable) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => Ordering::Equal,
        })
}

async fn catalog_click_text_on_client(
    client: &CdpClient,
    expr: &str,
    target_id: &str,
    is_parent: bool,
) -> Vec<ClickTextCatalogEntry> {
    let mut out = Vec::new();
    let mut ctx_ids = client.refresh_frame_contexts().await;
    if ctx_ids.is_empty() {
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx_ids = client.refresh_frame_contexts().await;
    }
    if ctx_ids.is_empty() {
        let v = client.evaluate(expr).await.unwrap_or(Value::Null);
        for (local, score, text_score, clickable) in parse_click_text_catalog(&v) {
            out.push(ClickTextCatalogEntry {
                local_index: local,
                score,
                text_score,
                clickable,
                target_id: target_id.to_string(),
                context_id: None,
                is_parent,
            });
        }
        return out;
    }
    for ctx_id in ctx_ids {
        let v = client
            .evaluate_in_context(expr, Some(ctx_id))
            .await
            .unwrap_or(Value::Null);
        for (local, score, text_score, clickable) in parse_click_text_catalog(&v) {
            out.push(ClickTextCatalogEntry {
                local_index: local,
                score,
                text_score,
                clickable,
                target_id: target_id.to_string(),
                context_id: Some(ctx_id),
                is_parent,
            });
        }
    }
    out
}

async fn locate_global_click_text_once(
    parent: &CdpClient,
    query: &str,
    global_index: u32,
    hot: &FrameHotPath,
) -> Result<StructuralLocalHit, String> {
    let expr = crate::commands::scripts::compose_click_text_catalog(query);
    let parent_id = parent.target_id.clone();
    let mut entries = catalog_click_text_on_client(parent, &expr, &parent_id, true).await;

    let mut oopifs =
        list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Peek).await;
    prefer_oopif_first(&mut oopifs, hot.preferred_oopif.as_deref());
    if oopifs.is_empty() {
        oopifs =
            list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Wait).await;
        prefer_oopif_first(&mut oopifs, hot.preferred_oopif.as_deref());
    }
    for iframe in oopifs {
        let skip_spa = hot.preferred_warm
            && hot
                .preferred_oopif
                .as_deref()
                .is_some_and(|p| p == iframe.id);
        let Ok(child) = CdpClient::connect_to_any_target_on(parent.port, &iframe.id).await else {
            continue;
        };
        if !skip_spa {
            let _ = child.wait_spa_content_ready(OOPIF_SPA_READY_TIMEOUT).await;
        }
        entries.extend(catalog_click_text_on_client(&child, &expr, &iframe.id, false).await);
    }

    let total = entries.len() as u32;
    entries.sort_by(cmp_click_text_catalog);
    if global_index < 1 || global_index as usize > entries.len() {
        return Err(format!(
            "未找到「{query}」#{global_index}（跨 frame 共 {total} 个足够匹配）"
        ));
    }
    let hit = &entries[global_index as usize - 1];
    Ok(StructuralLocalHit {
        local_index: hit.local_index,
        target_id: hit.target_id.clone(),
        context_id: hit.context_id,
        is_parent: hit.is_parent,
    })
}

/// 按探针相同规则（硬可点击优先 + 得分降序，跨 frame 连续 #N）定位点击文字所属 frame 与本地序号。
pub async fn locate_global_click_text_with_hot(
    parent: &CdpClient,
    query: &str,
    global_index: u32,
    hot: &FrameHotPath,
    cancel: Option<&AtomicBool>,
) -> Result<StructuralLocalHit, String> {
    if global_index < 1 {
        return Err("点击文字序号从 1 开始".into());
    }
    if cancelled(cancel) {
        return Err("已取消".into());
    }
    let started = Instant::now();
    let deadline = started + CLICK_TEXT_LOCATE_TIMEOUT;
    let mut last_err = String::new();
    loop {
        if cancelled(cancel) {
            return Err("已取消".into());
        }
        match locate_global_click_text_once(parent, query, global_index, hot).await {
            Ok(hit) => return Ok(hit),
            Err(e) => {
                last_err = e;
                if Instant::now() >= deadline {
                    break;
                }
                let slice = CLICK_TEXT_LOCATE_POLL.min(Duration::from_millis(100));
                let mut waited = Duration::ZERO;
                while waited < CLICK_TEXT_LOCATE_POLL {
                    if cancelled(cancel) {
                        return Err("已取消".into());
                    }
                    tokio::time::sleep(slice).await;
                    waited += slice;
                }
            }
        }
    }
    let waited_ms = started.elapsed().as_millis();
    let waited_s = (waited_ms as f64 / 1000.0).round() as u64;
    let count_hint = candidate_count_from_locate_err(&last_err)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());
    Err(format!(
        "等待 {waited_s}s 后仍未找到「{query}」#{global_index}（跨 frame 共 {count_hint} 个足够匹配；实际轮询 {waited_ms} ms）"
    ))
}

/// 在已定位的 frame/context 上执行能力脚本（全局序号已改写为本地）
pub async fn eval_on_structural_owner(
    parent: &CdpClient,
    owner: &StructuralLocalHit,
    expr: &str,
    cancel: Option<&AtomicBool>,
    timeout: Duration,
) -> Result<ScriptEvalHit, String> {
    if cancelled(cancel) {
        return Err("已取消".into());
    }
    let v = if owner.is_parent || owner.target_id == parent.target_id {
        let eval = parent.evaluate_in_context_with_timeout(expr, owner.context_id, timeout);
        tokio::pin!(eval);
        if let Some(flag) = cancel {
            tokio::select! {
                biased;
                _ = async {
                    while !flag.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(80)).await;
                    }
                } => return Err("已取消".into()),
                r = &mut eval => r?,
            }
        } else {
            eval.await?
        }
    } else {
        let port = parent.port;
        let tid = owner.target_id.clone();
        let ctx = owner.context_id;
        let expr = expr.to_string();
        let join = tokio::spawn(async move {
            let client = CdpClient::connect_to_any_target_on(port, &tid).await?;
            client
                .evaluate_in_context_with_timeout(&expr, ctx, timeout)
                .await
        });
        tokio::pin!(join);
        if let Some(flag) = cancel {
            tokio::select! {
                biased;
                _ = async {
                    while !flag.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(80)).await;
                    }
                } => {
                    join.abort();
                    return Err("已取消".into());
                }
                r = &mut join => r.map_err(|e| format!("frame 任务异常: {e}"))??,
            }
        } else {
            join.await.map_err(|e| format!("frame 任务异常: {e}"))??
        }
    };
    if v.is_null() {
        return Err("脚本返回 null".into());
    }
    Ok(ScriptEvalHit {
        value: v,
        target_id: owner.target_id.clone(),
        is_parent: owner.is_parent,
        context_id: owner.context_id,
    })
}

pub fn build_script_expr(
    script_body: &str,
    args: &str,
    session_bits: Option<(&str, &str, &str)>,
    extra_ctx: Option<&Value>,
) -> Result<String, String> {
    let mut ctx = serde_json::json!({ "args": args });
    if let Some((run_dir, run_id, downloads_dir)) = session_bits {
        if let Some(obj) = ctx.as_object_mut() {
            obj.insert("runDir".into(), serde_json::json!(run_dir));
            obj.insert("runId".into(), serde_json::json!(run_id));
            obj.insert("downloadsDir".into(), serde_json::json!(downloads_dir));
        }
    }
    if let (Some(base), Some(extra)) = (
        ctx.as_object_mut(),
        extra_ctx.and_then(|v| v.as_object()),
    ) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    let ctx_str = serde_json::to_string(&ctx).map_err(|e| e.to_string())?;
    let body_escaped = serde_json::to_string(script_body).map_err(|e| e.to_string())?;
    Ok(format!(
        r#"(async () => {{
  const ctx = {ctx_str};
  const __body = {body_escaped};
  const __AsyncFunction = Object.getPrototypeOf(async function() {{}}).constructor;
  const fn = new __AsyncFunction('ctx', __body);
  return await fn(ctx);
}})()"#
    ))
}

fn hit_score(v: &Value) -> f64 {
    v.get("score")
        .and_then(|x| x.as_f64())
        .or_else(|| v.get("textScore").and_then(|x| x.as_f64()))
        .unwrap_or(0.0)
}

fn is_ok(v: &Value) -> bool {
    v.get("ok").and_then(|x| x.as_bool()) == Some(true)
}

/// 是否足以提前结束其它 frame（不必等主文档跑满 10s 超时）
fn is_decisive_hit(v: &Value) -> bool {
    if !is_ok(v) {
        return false;
    }
    if v.get("decisive").and_then(|x| x.as_bool()) == Some(true) {
        return true;
    }
    if hit_score(v) >= DECISIVE_SCORE {
        return true;
    }
    // 结构化目标（输入框#1 等）不带 textScore，但已定位到元素
    if v.get("structural").and_then(|x| x.as_bool()) == Some(true) {
        return true;
    }
    // 输入成功：已定位到具体输入框并建立焦点锚点
    if v.get("focusAnchor").is_some() {
        return true;
    }
    // 已返回点击坐标：可直接 CDP 点击，无需再等慢/挂起的其它 frame
    v.get("x").and_then(|x| x.as_f64()).is_some()
        && v.get("y").and_then(|x| x.as_f64()).is_some()
}

/// 失败原因已确定（如输入框只读/禁用）：其它 frame 再试也是同样结果
fn is_fatal_failure(v: &Value) -> bool {
    if is_ok(v) {
        return false;
    }
    v.get("fatal").and_then(|x| x.as_bool()) == Some(true)
        || v.get("locked").and_then(|x| x.as_bool()) == Some(true)
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|f| f.load(Ordering::SeqCst))
}

async fn wait_until_cancelled(cancel: Option<&AtomicBool>) {
    while !cancelled(cancel) {
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

async fn eval_on_parent_timed(
    parent: &CdpClient,
    expr: &str,
    cancel: Option<&AtomicBool>,
    timeout: Duration,
) -> Result<Value, String> {
    if cancelled(cancel) {
        return Err("已取消".into());
    }
    let eval = parent.evaluate_in_context_with_timeout(expr, None, timeout);
    tokio::pin!(eval);
    if cancel.is_some() {
        tokio::select! {
            biased;
            _ = wait_until_cancelled(cancel) => Err("已取消".into()),
            r = eval => r,
        }
    } else {
        eval.await
    }
}

/// 是否需要跨 iframe 扫描（纯等待秒数 / 下载 / 加载 等仅主文档）
pub fn should_fanout_frames(keyword: &str, args: &str) -> bool {
    match keyword {
        "点击" | "输入" => true,
        "等待" => {
            let s = args
                .trim()
                .trim_end_matches(|c: char| {
                    matches!(c, ',' | '，' | '。' | '；' | ';' | '、' | '．' | '.')
                })
                .trim();
            if s.is_empty() {
                return false;
            }
            if s.parse::<f64>().is_ok() {
                return false;
            }
            if s.contains("下载") {
                return false;
            }
            // 「加载」「加载10」只关心顶层 document.readyState
            let body = s
                .trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c.is_whitespace())
                .trim();
            if body == "加载" {
                return false;
            }
            true
        }
        _ => false,
    }
}

async fn eval_on_target(port: u16, target_id: &str, expr: &str) -> Result<Value, String> {
    eval_on_target_opts(port, target_id, expr, false).await
}

async fn eval_on_target_opts(
    port: u16,
    target_id: &str,
    expr: &str,
    skip_spa: bool,
) -> Result<Value, String> {
    let client = CdpClient::connect_to_any_target_on(port, target_id).await?;
    if !skip_spa {
        let _ = client.wait_spa_content_ready(OOPIF_SPA_READY_TIMEOUT).await;
    }
    let v = client.evaluate(expr).await?;
    if v.is_null() {
        return Err("脚本返回 null".into());
    }
    Ok(v)
}

/// sticky OOPIF 优先试探：短超时；失败则由调用方回退全量扇出
pub async fn try_eval_preferred_oopif(
    parent: &CdpClient,
    preferred_id: &str,
    expr: &str,
    skip_spa: bool,
    cancel: Option<&AtomicBool>,
) -> Result<ScriptEvalHit, String> {
    if cancelled(cancel) {
        return Err("已取消".into());
    }
    let port = parent.port;
    let tid = preferred_id.to_string();
    let expr = expr.to_string();
    let join = tokio::spawn(async move {
        match tokio::time::timeout(
            STICKY_IFRAME_EVAL_TIMEOUT,
            eval_on_target_opts(port, &tid, &expr, skip_spa),
        )
        .await
        {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(format!(
                "sticky iframe 求值超时（{}ms）",
                STICKY_IFRAME_EVAL_TIMEOUT.as_millis()
            )),
        }
    });
    tokio::pin!(join);
    let v = if let Some(flag) = cancel {
        tokio::select! {
            biased;
            _ = async {
                while !flag.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
            } => {
                join.abort();
                return Err("已取消".into());
            }
            r = &mut join => r.map_err(|e| format!("frame 任务异常: {e}"))??,
        }
    } else {
        join.await.map_err(|e| format!("frame 任务异常: {e}"))??
    };
    if !is_ok(&v) {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("sticky 未命中")
            .to_string();
        return Err(err);
    }
    Ok(ScriptEvalHit {
        value: v,
        target_id: preferred_id.to_string(),
        is_parent: false,
        context_id: None,
    })
}

/// 父页短探：热路径上与 sticky iframe 并行，避免主页操作先空等 OOPIF。
pub async fn try_eval_on_parent_short(
    parent: &CdpClient,
    expr: &str,
    cancel: Option<&AtomicBool>,
) -> Result<ScriptEvalHit, String> {
    if cancelled(cancel) {
        return Err("已取消".into());
    }
    let v = eval_on_parent_timed(parent, expr, cancel, PARENT_SHORT_TRY_TIMEOUT).await?;
    if v.is_null() {
        return Err("脚本返回 null".into());
    }
    if !is_ok(&v) {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("父页短探未命中")
            .to_string();
        return Err(err);
    }
    Ok(ScriptEvalHit {
        value: v,
        target_id: parent.target_id.clone(),
        is_parent: true,
        context_id: None,
    })
}

fn absorb_result(
    best: &mut Option<ScriptEvalHit>,
    parent_err: &mut Option<String>,
    last_err: &mut Option<String>,
    fatal_err: &mut Option<String>,
    tid: String,
    is_parent: bool,
    context_id: Option<u64>,
    v: Value,
) {
    if is_ok(&v) {
        let hit = ScriptEvalHit {
            value: v,
            target_id: tid,
            is_parent,
            context_id,
        };
        *best = Some(match best.take() {
            Some(prev) => pick_better(prev, hit),
            None => hit,
        });
    } else {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("脚本失败")
            .to_string();
        // 明确原因优先于其它 frame 的「未找到」，否则会被主文档的泛化错误覆盖
        if is_fatal_failure(&v) && fatal_err.is_none() {
            *fatal_err = Some(err.clone());
        }
        if is_parent && context_id.is_none() {
            *parent_err = Some(err);
        } else {
            *last_err = Some(err);
        }
    }
}

/// 并行在主标签 + 同源子 frame 上下文 + 其子 OOPIF 上求值。
///
/// - **复用**已附着的主文档连接（禁止二次 attach，否则易与现有会话冲突并卡满 30s）
/// - 同源 iframe：在主连接上按 contextId 短等求值
/// - 跨域 OOPIF：单独连接；先等 SPA 内容再求值
/// - 壳层：父页先 peek；FrameTree 有子 frame 却发现为空时再短轮询 OOPIF
/// - 出现高分命中后短暂 grace 即中止其余任务
/// - `parent_timeout`：主文档求值上限（点击/输入扇出约 10s；智能等待需 ≥ 脚本超时）
pub async fn eval_across_frames(
    parent: &CdpClient,
    expr: &str,
    cancel: Option<&AtomicBool>,
    parent_timeout: Duration,
) -> Result<ScriptEvalHit, String> {
    eval_across_frames_with_hot(
        parent,
        expr,
        cancel,
        parent_timeout,
        &FrameHotPath::default(),
        None,
    )
    .await
}

pub async fn eval_across_frames_with_hot(
    parent: &CdpClient,
    expr: &str,
    cancel: Option<&AtomicBool>,
    parent_timeout: Duration,
    hot: &FrameHotPath,
    early_expr: Option<&str>,
) -> Result<ScriptEvalHit, String> {
    if cancelled(cancel) {
        return Err("已取消".into());
    }
    // sticky 短试探由 runtime 用缩短后的 elementWaitTimeout 单独发起；
    // 此处只做全量扇出，并把 preferred 排到前面 / 跳过热 SPA。
    let _port = parent.port;
    let parent_id = parent.target_id.clone();

    let child_ctxs = parent.child_frame_context_ids().await;
    let tree_has_children = parent.frame_tree_has_child_frames().await;
    // Peek：壳层步骤不白等；有同源 ctx ≠ 已扫到跨域业务 iframe
    let mut iframes =
        list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Peek).await;
    prefer_oopif_first(&mut iframes, hot.preferred_oopif.as_deref());

    // 无任何子 frame 线索：与主窗一致走完整等待（有 sticky 时 prefer 已注入，不会误走这里）
    if iframes.is_empty() && child_ctxs.is_empty() && !tree_has_children {
        let v = eval_on_parent_timed(parent, expr, cancel, parent_timeout).await?;
        if v.is_null() {
            return Err("脚本返回 null".into());
        }
        return Ok(ScriptEvalHit {
            value: v,
            target_id: parent_id,
            is_parent: true,
            context_id: None,
        });
    }

    // 同源子 frame 与主文档共用一条 CDP 连接，无法并行。
    // 必须先短探子 frame，否则会先空等主文档跑满 elementWait（常见 ~15s）。
    if let Some(early) = early_expr {
        if let Ok(hit) =
            try_eval_same_origin_children_early(parent, early, &child_ctxs, cancel).await
        {
            return Ok(hit);
        }
        // 跨域 OOPIF：同样先短探，勿与主文档 15s 长等并行空耗
        if !iframes.is_empty() {
            if let Ok(hit) = try_eval_oopifs_early(parent, early, &iframes, cancel).await {
                return Ok(hit);
            }
        }
    }

    // FrameTree 已声明子 frame 但 OOPIF 尚未暴露：先父页 peek，不够再长等 OOPIF。
    // 同源子 frame 已在上方 early 扫过，此处勿再串行长等挡住 OOPIF 发现。
    if tree_has_children && iframes.is_empty() {
        match eval_across_frames_inner(
            parent,
            expr,
            cancel,
            PARENT_PEEK_TIMEOUT.min(parent_timeout),
            Vec::new(),
            Vec::new(),
            hot,
        )
        .await
        {
            Ok(hit) if is_ok(&hit.value) => return Ok(hit),
            _ => {}
        }
        iframes =
            list_oopifs_stable_for(parent, &parent_id, chrome::DiscoverOopifMode::Wait).await;
        if let Some(early) = early_expr {
            if !iframes.is_empty() {
                if let Ok(hit) = try_eval_oopifs_early(parent, early, &iframes, cancel).await {
                    return Ok(hit);
                }
            }
        }
    }

    eval_across_frames_inner(parent, expr, cancel, parent_timeout, iframes, child_ctxs, hot).await
}

/// 同源 iframe 优先短探：命中则立即返回，避免主文档长等挡住业务 frame。
async fn try_eval_same_origin_children_early(
    parent: &CdpClient,
    expr: &str,
    child_ctxs: &[u64],
    cancel: Option<&AtomicBool>,
) -> Result<ScriptEvalHit, String> {
    if child_ctxs.is_empty() {
        return Err("无同源子 frame".into());
    }
    let parent_id = parent.target_id.clone();
    let mut best: Option<ScriptEvalHit> = None;
    let mut parent_err: Option<String> = None;
    let mut last_err: Option<String> = None;
    let mut fatal_err: Option<String> = None;
    for ctx in child_ctxs.iter().copied() {
        if cancelled(cancel) {
            return Err("已取消".into());
        }
        match tokio::time::timeout(
            SAME_ORIGIN_EARLY_TIMEOUT,
            parent.evaluate_in_context(expr, Some(ctx)),
        )
        .await
        {
            Ok(Ok(v)) if !v.is_null() => {
                absorb_result(
                    &mut best,
                    &mut parent_err,
                    &mut last_err,
                    &mut fatal_err,
                    parent_id.clone(),
                    true,
                    Some(ctx),
                    v,
                );
                if let Some(err) = fatal_err {
                    return Err(err);
                }
                if best.as_ref().is_some_and(|h| is_decisive_hit(&h.value)) {
                    return Ok(best.take().unwrap());
                }
            }
            Ok(Err(e)) => last_err = Some(e),
            Err(_) => {
                // 短探超时：该 frame 暂无目标或仍在等，交给后续全量扇出
            }
            _ => {}
        }
    }
    if let Some(hit) = best {
        if is_ok(&hit.value) {
            return Ok(hit);
        }
    }
    Err(last_err.unwrap_or_else(|| "同源短探未命中".into()))
}

/// 跨域 OOPIF 优先短探（并行）：页面已展示时跳过 SPA 长等。
async fn try_eval_oopifs_early(
    parent: &CdpClient,
    expr: &str,
    iframes: &[chrome::TargetInfo],
    cancel: Option<&AtomicBool>,
) -> Result<ScriptEvalHit, String> {
    if iframes.is_empty() {
        return Err("无 OOPIF".into());
    }
    let port = parent.port;
    let mut set = JoinSet::new();
    for iframe in iframes {
        let expr = expr.to_string();
        let tid = iframe.id.clone();
        set.spawn(async move {
            match tokio::time::timeout(
                SAME_ORIGIN_EARLY_TIMEOUT,
                // 业务页已渲染：短探跳过 SPA 就绪，避免再空等 3s
                eval_on_target_opts(port, &tid, &expr, true),
            )
            .await
            {
                Ok(Ok(v)) => Ok((tid, v)),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(format!(
                    "OOPIF 短探超时（{}ms）",
                    SAME_ORIGIN_EARLY_TIMEOUT.as_millis()
                )),
            }
        });
    }

    let mut best: Option<ScriptEvalHit> = None;
    let mut last_err: Option<String> = None;
    while let Some(joined) = set.join_next().await {
        if cancelled(cancel) {
            set.abort_all();
            return Err("已取消".into());
        }
        let outcome = match joined {
            Ok(inner) => inner,
            Err(e) => {
                if !e.is_cancelled() {
                    last_err = Some(format!("frame 任务异常: {e}"));
                }
                continue;
            }
        };
        match outcome {
            Ok((tid, v)) => {
                if is_ok(&v) {
                    let hit = ScriptEvalHit {
                        value: v,
                        target_id: tid,
                        is_parent: false,
                        context_id: None,
                    };
                    best = Some(match best.take() {
                        Some(prev) => pick_better(prev, hit),
                        None => hit,
                    });
                    if best.as_ref().is_some_and(|h| is_decisive_hit(&h.value)) {
                        set.abort_all();
                        return Ok(best.take().unwrap());
                    }
                } else {
                    let err = v
                        .get("error")
                        .and_then(|x| x.as_str())
                        .unwrap_or("OOPIF 短探未命中")
                        .to_string();
                    last_err = Some(err);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(hit) = best {
        if is_ok(&hit.value) {
            return Ok(hit);
        }
    }
    Err(last_err.unwrap_or_else(|| "OOPIF 短探未命中".into()))
}

async fn eval_across_frames_inner(
    parent: &CdpClient,
    expr: &str,
    cancel: Option<&AtomicBool>,
    parent_timeout: Duration,
    iframes: Vec<chrome::TargetInfo>,
    child_ctxs: Vec<u64>,
    hot: &FrameHotPath,
) -> Result<ScriptEvalHit, String> {
    let port = parent.port;
    let parent_id = parent.target_id.clone();

    let preferred = hot.preferred_oopif.clone();
    let preferred_warm = hot.preferred_warm;
    let mut set = JoinSet::new();
    for iframe in iframes {
        let expr = expr.to_string();
        let tid = iframe.id;
        let skip_spa = preferred_warm && preferred.as_deref() == Some(tid.as_str());
        set.spawn(async move {
            match tokio::time::timeout(
                IFRAME_EVAL_TIMEOUT,
                eval_on_target_opts(port, &tid, &expr, skip_spa),
            )
            .await
            {
                Ok(Ok(v)) => Ok((tid, false, v)),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(format!(
                    "iframe 求值超时（{}ms）",
                    IFRAME_EVAL_TIMEOUT.as_millis()
                )),
            }
        });
    }

    let mut best: Option<ScriptEvalHit> = None;
    let mut parent_err: Option<String> = None;
    let mut last_err: Option<String> = None;
    let mut fatal_err: Option<String> = None;

    // 无 OOPIF、仅有同源子 frame：必须先扫子 frame，再扫主文档。
    // 同连接无法并行；若先跑主文档会空等满 elementWait（~15s）才轮到业务 iframe。
    if set.is_empty() && !child_ctxs.is_empty() {
        for ctx in child_ctxs.iter().copied() {
            if cancelled(cancel) {
                return Err("已取消".into());
            }
            match tokio::time::timeout(
                IFRAME_EVAL_TIMEOUT,
                parent.evaluate_in_context(expr, Some(ctx)),
            )
            .await
            {
                Ok(Ok(v)) if !v.is_null() => {
                    absorb_result(
                        &mut best,
                        &mut parent_err,
                        &mut last_err,
                        &mut fatal_err,
                        parent_id.clone(),
                        true,
                        Some(ctx),
                        v,
                    );
                    if let Some(err) = fatal_err.take() {
                        return Err(err);
                    }
                    if best.as_ref().is_some_and(|h| is_decisive_hit(&h.value)) {
                        return Ok(best.take().unwrap());
                    }
                }
                Ok(Err(e)) => last_err = Some(e),
                Err(_) => {
                    last_err = Some(format!(
                        "同源 iframe 求值超时（{}ms）",
                        IFRAME_EVAL_TIMEOUT.as_millis()
                    ));
                }
                _ => {}
            }
        }
        let v = eval_on_parent_timed(parent, expr, cancel, parent_timeout).await?;
        if v.is_null() {
            parent_err = Some("脚本返回 null".into());
        } else {
            absorb_result(
                &mut best,
                &mut parent_err,
                &mut last_err,
                &mut fatal_err,
                parent_id.clone(),
                true,
                None,
                v,
            );
        }
        if let Some(err) = fatal_err {
            return Err(err);
        }
        if let Some(hit) = best {
            return Ok(hit);
        }
        return Err(parent_err
            .or(last_err)
            .unwrap_or_else(|| "未找到目标".into()));
    }

    let mut parent_done = false;
    let mut grace_deadline: Option<Instant> = None;
    let mut child_ctx_iter = child_ctxs.into_iter();

    let parent_eval = eval_on_parent_timed(parent, expr, cancel, parent_timeout);
    tokio::pin!(parent_eval);

    loop {
        if cancelled(cancel) {
            set.abort_all();
            return Err("已取消".into());
        }
        if let Some(deadline) = grace_deadline {
            if Instant::now() >= deadline {
                set.abort_all();
                break;
            }
        }

        let waiting_parent = !parent_done;
        let waiting_oopif = !set.is_empty();
        if !waiting_parent && !waiting_oopif {
            break;
        }

        let grace_left = grace_deadline.map(|d| d.saturating_duration_since(Instant::now()));

        tokio::select! {
            biased;

            _ = wait_until_cancelled(cancel), if cancel.is_some() => {
                set.abort_all();
                return Err("已取消".into());
            }

            _ = tokio::time::sleep(grace_left.unwrap_or(Duration::MAX)), if grace_left.is_some() => {
                set.abort_all();
                break;
            }

            v = &mut parent_eval, if waiting_parent => {
                parent_done = true;
                match v {
                    Ok(val) => {
                        if val.is_null() {
                            parent_err = Some("脚本返回 null".into());
                        } else {
                            absorb_result(
                                &mut best,
                                &mut parent_err,
                                &mut last_err,
                                &mut fatal_err,
                                parent_id.clone(),
                                true,
                                None,
                                val,
                            );
                            if best.as_ref().is_some_and(|h| is_decisive_hit(&h.value))
                                || fatal_err.is_some()
                            {
                                grace_deadline =
                                    Some(Instant::now() + GRACE_AFTER_DECISIVE);
                            }
                        }
                    }
                    Err(e) => parent_err = Some(e),
                }
            }

            joined = set.join_next(), if waiting_oopif => {
                let Some(joined) = joined else { continue };
                let outcome = match joined {
                    Ok(inner) => inner,
                    Err(e) => {
                        if !e.is_cancelled() {
                            last_err = Some(format!("frame 任务异常: {e}"));
                        }
                        continue;
                    }
                };
                match outcome {
                    Ok((tid, is_parent, v)) => {
                        absorb_result(
                            &mut best,
                            &mut parent_err,
                            &mut last_err,
                            &mut fatal_err,
                            tid,
                            is_parent,
                            None,
                            v,
                        );
                        if best.as_ref().is_some_and(|h| is_decisive_hit(&h.value))
                            || fatal_err.is_some()
                        {
                            grace_deadline =
                                Some(Instant::now() + GRACE_AFTER_DECISIVE);
                        }
                    }
                    Err(e) => last_err = Some(e),
                }
            }
        }

        // 穿插一个同源子 frame（复用 parent 连接；不可并行二次 attach）
        if let Some(ctx) = child_ctx_iter.next() {
            if cancelled(cancel) {
                set.abort_all();
                return Err("已取消".into());
            }
            if let Some(deadline) = grace_deadline {
                if Instant::now() >= deadline {
                    set.abort_all();
                    break;
                }
            }
            match tokio::time::timeout(
                IFRAME_EVAL_TIMEOUT,
                parent.evaluate_in_context(expr, Some(ctx)),
            )
            .await
            {
                Ok(Ok(v)) => {
                    if !v.is_null() {
                        absorb_result(
                            &mut best,
                            &mut parent_err,
                            &mut last_err,
                            &mut fatal_err,
                            parent_id.clone(),
                            true,
                            Some(ctx),
                            v,
                        );
                        if best.as_ref().is_some_and(|h| is_decisive_hit(&h.value))
                            || fatal_err.is_some()
                        {
                            grace_deadline = Some(Instant::now() + GRACE_AFTER_DECISIVE);
                        }
                    }
                }
                Ok(Err(e)) => last_err = Some(e),
                Err(_) => {
                    last_err = Some(format!(
                        "同源 iframe 求值超时（{}ms）",
                        IFRAME_EVAL_TIMEOUT.as_millis()
                    ));
                }
            }
        }
    }

    // 主文档与 OOPIF 已结束后，扫完剩余同源 frame
    while let Some(ctx) = child_ctx_iter.next() {
        if cancelled(cancel) {
            return Err("已取消".into());
        }
        if let Some(deadline) = grace_deadline {
            if Instant::now() >= deadline {
                break;
            }
        }
        if best.as_ref().is_some_and(|h| is_decisive_hit(&h.value)) || fatal_err.is_some() {
            break;
        }
        match tokio::time::timeout(
            IFRAME_EVAL_TIMEOUT,
            parent.evaluate_in_context(expr, Some(ctx)),
        )
        .await
        {
            Ok(Ok(v)) => {
                if !v.is_null() {
                    absorb_result(
                        &mut best,
                        &mut parent_err,
                        &mut last_err,
                        &mut fatal_err,
                        parent_id.clone(),
                        true,
                        Some(ctx),
                        v,
                    );
                }
            }
            Ok(Err(e)) => last_err = Some(e),
            Err(_) => {
                last_err = Some(format!(
                    "同源 iframe 求值超时（{}ms）",
                    IFRAME_EVAL_TIMEOUT.as_millis()
                ));
            }
        }
    }

    if let Some(err) = fatal_err {
        return Err(err);
    }
    if let Some(hit) = best {
        return Ok(hit);
    }
    Err(parent_err
        .or(last_err)
        .unwrap_or_else(|| "未找到目标".into()))
}

/// 在命中 target 上执行点击坐标（OOPIF 需附着到该 iframe target）
pub async fn click_on_hit(parent: &mut CdpClient, hit: &ScriptEvalHit) -> Result<(), String> {
    if hit.value.get("clicked").and_then(|x| x.as_bool()) == Some(true) {
        return Ok(());
    }
    let local_x = hit
        .value
        .get("x")
        .and_then(|x| x.as_f64())
        .ok_or("脚本未返回 x")?;
    let local_y = hit
        .value
        .get("y")
        .and_then(|x| x.as_f64())
        .ok_or("脚本未返回 y")?;

    // 同源 iframe：脚本已沿 frameElement 算好顶层坐标，直接点（最可靠）
    let top_x = hit.value.get("topX").and_then(|x| x.as_f64());
    let top_y = hit.value.get("topY").and_then(|x| x.as_f64());
    if let (Some(tx), Some(ty)) = (top_x, top_y) {
        if hit.value.get("inFrame").and_then(|x| x.as_bool()) == Some(true) {
            eprintln!(
                "[frame_eval] 同源 iframe 使用脚本 top 坐标 ({tx:.1},{ty:.1})（frame 内 ({local_x:.1},{local_y:.1})）"
            );
        }
        if hit.is_parent || hit.target_id == parent.target_id {
            return parent.click_at(tx, ty).await;
        }
        let child = CdpClient::connect_to_any_target_on(parent.port, &hit.target_id).await?;
        // OOPIF：top 坐标对父页无意义，仍用 frame 内坐标在子 target 上点
        return child.click_at(local_x, local_y).await;
    }

    let mut x = local_x;
    let mut y = local_y;

    if hit.is_parent || hit.target_id == parent.target_id {
        // 跨域同进程少见 / 脚本未给出 topX：回退 CDP 偏移
        if let Some(ctx) = hit.context_id {
            match parent.frame_context_viewport_offset(ctx).await {
                Ok((ox, oy)) => {
                    if ox != 0.0 || oy != 0.0 {
                        eprintln!(
                            "[frame_eval] 同源 iframe CDP 坐标偏移 context={ctx} +({ox:.1},{oy:.1}) → ({:.1},{:.1})",
                            x + ox,
                            y + oy
                        );
                    }
                    x += ox;
                    y += oy;
                }
                Err(e) => {
                    return Err(format!(
                        "同源 iframe 点击坐标偏移失败（context={ctx}）：{e}。请重试或改用页面探针确认目标所在 frame"
                    ));
                }
            }
        }
        return parent.click_at(x, y).await;
    }
    let child = CdpClient::connect_to_any_target_on(parent.port, &hit.target_id).await?;
    child.click_at(x, y).await
}

/// 在命中 target 上 CDP 键入
pub async fn insert_text_on_hit(
    parent: &mut CdpClient,
    hit: &ScriptEvalHit,
    text: &str,
) -> Result<(), String> {
    if hit.is_parent || hit.target_id == parent.target_id {
        return parent.insert_text(text).await;
    }
    let child = CdpClient::connect_to_any_target_on(parent.port, &hit.target_id).await?;
    child.insert_text(text).await
}

/// 在指定 target 上执行表达式（焦点恢复等）
pub async fn evaluate_on_target(
    parent: &CdpClient,
    target_id: Option<&str>,
    expr: &str,
) -> Result<Value, String> {
    let Some(tid) = target_id.filter(|s| !s.is_empty()) else {
        return parent.evaluate(expr).await;
    };
    if tid == parent.target_id {
        return parent.evaluate(expr).await;
    }
    let child = CdpClient::connect_to_any_target_on(parent.port, tid).await?;
    child.evaluate(expr).await
}

/// 在指定 target 上点击坐标
pub async fn click_at_on_target(
    parent: &CdpClient,
    target_id: Option<&str>,
    x: f64,
    y: f64,
) -> Result<(), String> {
    let Some(tid) = target_id.filter(|s| !s.is_empty()) else {
        return parent.click_at(x, y).await;
    };
    if tid == parent.target_id {
        return parent.click_at(x, y).await;
    }
    let child = CdpClient::connect_to_any_target_on(parent.port, tid).await?;
    child.click_at(x, y).await
}

fn pick_better(a: ScriptEvalHit, b: ScriptEvalHit) -> ScriptEvalHit {
    let sa = hit_score(&a.value);
    let sb = hit_score(&b.value);
    if (sb - sa).abs() > 1e-9 {
        return if sb > sa { b } else { a };
    }
    // 同分：优先子 frame（同进程 context / OOPIF），避免壳层与内容区同文案时点到父页
    let a_in_child = a.context_id.is_some() || !a.is_parent;
    let b_in_child = b.context_id.is_some() || !b.is_parent;
    if a_in_child != b_in_child {
        return if b_in_child { b } else { a };
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_for_click_and_element_wait() {
        assert!(should_fanout_frames("点击", "查询"));
        assert!(should_fanout_frames("输入", "abc|用户名"));
        assert!(should_fanout_frames("等待", "查询按钮"));
        assert!(!should_fanout_frames("等待", "3"));
        assert!(!should_fanout_frames("等待", "1,"));
        assert!(!should_fanout_frames("等待", "加载"));
        assert!(!should_fanout_frames("等待", "下载完成"));
        assert!(!should_fanout_frames("打开", "http://x"));
    }

    #[test]
    fn structural_hit_is_decisive_without_score() {
        let v = serde_json::json!({
            "ok": true,
            "structural": true,
            "x": 10.0,
            "y": 20.0,
        });
        assert!(is_decisive_hit(&v));
        assert!(hit_score(&v) < DECISIVE_SCORE);
    }

    #[test]
    fn coordinate_hit_is_decisive() {
        let v = serde_json::json!({ "ok": true, "x": 1.0, "y": 2.0 });
        assert!(is_decisive_hit(&v));
    }

    /// iframe 内输入成功不带坐标/分数，仍应立即结束其它 frame（否则每次白等主文档超时）
    #[test]
    fn input_success_with_focus_anchor_is_decisive() {
        let v = serde_json::json!({
            "ok": true,
            "matched": "月份",
            "focusAnchor": { "kind": "fieldHint", "value": "月份" },
        });
        assert!(is_decisive_hit(&v));
    }

    #[test]
    fn locked_input_failure_is_fatal() {
        let v = serde_json::json!({ "ok": false, "error": "输入框只读，请用点击", "locked": true });
        assert!(is_fatal_failure(&v));
        assert!(!is_decisive_hit(&v));
        assert!(!is_fatal_failure(
            &serde_json::json!({ "ok": false, "error": "未找到字段「月份」" })
        ));
    }

    /// iframe 的明确原因不能被主文档的「未找到」覆盖
    #[test]
    fn fatal_error_wins_over_parent_error() {
        let mut best = None;
        let mut parent_err = None;
        let mut last_err = None;
        let mut fatal_err = None;
        absorb_result(
            &mut best,
            &mut parent_err,
            &mut last_err,
            &mut fatal_err,
            "iframe-1".into(),
            false,
            None,
            serde_json::json!({ "ok": false, "error": "输入框只读，请用点击", "fatal": true }),
        );
        absorb_result(
            &mut best,
            &mut parent_err,
            &mut last_err,
            &mut fatal_err,
            "page-1".into(),
            true,
            None,
            serde_json::json!({ "ok": false, "error": "等待 1.2s 后仍未找到可用输入框" }),
        );
        assert_eq!(fatal_err.as_deref(), Some("输入框只读，请用点击"));
        assert!(parent_err.is_some());
    }

    #[test]
    fn failed_hit_is_not_decisive() {
        let v = serde_json::json!({ "ok": false, "error": "未找到" });
        assert!(!is_decisive_hit(&v));
    }
}
