//! Authoring probe for webclaw-lite: scan the current (or newly opened) page
//! and emit recipe-ready click/input commands as one JSON object.
//!
//! Keeps the shared CDP browser alive so multi-round probe → short run → probe works.

use crate::cdp::CdpClient;
use crate::chrome;
use crate::page_probe::{self, ProbeCandidate};
use serde_json::{json, Value};

const DEFAULT_TYPES: &[&str] = &["输入框", "按钮", "图标按钮", "链接"];
const DEFAULT_LIMIT: usize = 60;

struct ProbeArgs {
    url: Option<String>,
    types: Vec<String>,
    search: Option<String>,
    limit: usize,
}

fn parse_args(args: &[String]) -> Result<ProbeArgs, String> {
    let mut url = None;
    let mut types: Option<Vec<String>> = None;
    let mut search = None;
    let mut limit = DEFAULT_LIMIT;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                i += 1;
                let v = args.get(i).ok_or("--url 需要地址")?.trim().to_string();
                if v.is_empty() {
                    return Err("--url 不能为空".into());
                }
                url = Some(v);
            }
            "--types" => {
                i += 1;
                let raw = args.get(i).ok_or("--types 需要类型列表")?.trim();
                let list: Vec<String> = raw
                    .split(|c: char| c == ',' || c == '，')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if list.is_empty() {
                    return Err("--types 不能为空".into());
                }
                types = Some(list);
            }
            "--search" => {
                i += 1;
                let v = args.get(i).ok_or("--search 需要关键词")?.trim().to_string();
                search = if v.is_empty() { None } else { Some(v) };
            }
            "--limit" => {
                i += 1;
                let raw = args.get(i).ok_or("--limit 需要正整数")?;
                limit = raw
                    .parse::<usize>()
                    .map_err(|_| format!("--limit 无效: {raw}"))?;
                if limit == 0 {
                    return Err("--limit 须 ≥ 1".into());
                }
            }
            "-h" | "--help" => {
                return Err("__help__".into());
            }
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    Ok(ProbeArgs {
        url,
        types: types.unwrap_or_else(|| DEFAULT_TYPES.iter().map(|s| (*s).to_string()).collect()),
        search,
        limit,
    })
}

fn lite_headless() -> bool {
    std::env::var("WEBCLAW_HEADLESS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Attach to existing worker CDP browser, or launch a shared "lite" profile once.
async fn ensure_shared_browser() -> Result<u16, String> {
    let port = chrome::worker_cdp_port();
    if chrome::is_debug_port_ready_on(port).await {
        return Ok(port);
    }
    let (p, _reused, msg) =
        chrome::ensure_worker_chrome(lite_headless(), false, "lite").await?;
    eprintln!("[probe] {msg}");
    Ok(p)
}

async fn connect_foreground(port: u16) -> Result<CdpClient, String> {
    let page = chrome::find_foreground_page_on_ex(port, true).await?;
    let client = CdpClient::connect_to_any_target_on(port, &page.id).await?;
    let _ = client.activate_tab().await;
    let _ = client.unblock_debugger().await;
    Ok(client)
}

async fn navigate_if_needed(client: &mut CdpClient, url: &str) -> Result<(), String> {
    client.navigate(url).await?;
    let _ = client
        .wait_spa_content_ready(std::time::Duration::from_secs(8))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    Ok(())
}

fn candidate_json(it: &ProbeCandidate) -> Value {
    json!({
        "type": it.element_type,
        "index": it.index,
        "snippet": it.snippet,
        "desc": it.desc,
        "textCmd": it.text_cmd,
        "indexCmd": it.index_cmd,
        "clickCmd": if it.click_cmd.is_empty() { Value::Null } else { json!(it.click_cmd) },
        "inputCmd": it.input_cmd,
        "frameLabel": it.frame_label,
    })
}

fn print_help() {
    eprintln!(
        "webclaw-lite probe — 编排期页面观察（多轮可用）

用法:
  webclaw-lite probe [--url URL] [--types 输入框,按钮,图标按钮,链接] [--search 关键词] [--limit 60]

说明:
  无 --url：扫描当前前台页（导航/短 run 之后再 probe 用这个）
  有 --url：先打开该地址再扫（编排首跳）
  与 run 共用 WEBCLAW_CDP_PORT 上的浏览器，不强制杀进程
"
    );
}

/// CLI entry: `webclaw-lite probe …`
pub async fn probe_cli(args: &[String]) -> Result<(), String> {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) if e == "__help__" => {
            print_help();
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    let port = ensure_shared_browser().await?;
    let mut client = connect_foreground(port).await?;
    if let Some(url) = parsed.url.as_deref() {
        navigate_if_needed(&mut client, url).await?;
        // After navigate, re-attach in case the target changed.
        client = connect_foreground(port).await?;
    }

    let type_refs: Vec<&str> = parsed.types.iter().map(|s| s.as_str()).collect();
    let result = page_probe::scan_attached_page_for_authoring(
        &client,
        &type_refs,
        parsed.search.as_deref(),
        parsed.limit,
    )
    .await?;

    let candidates: Vec<Value> = result.items.iter().map(candidate_json).collect();
    let payload = json!({
        "type": "probe",
        "ok": result.ok,
        "url": result.url,
        "title": result.title,
        "countsByType": result.counts_by_type,
        "candidateCount": candidates.len(),
        "candidates": candidates,
        "hint": "编排：只用 candidates 里的 textCmd/indexCmd/clickCmd/inputCmd 拼下一段菜谱；页面变了再 probe（勿带 url）。复跑：整段 webclaw_run，不要逐步 probe。",
    });
    println!("{}", serde_json::to_string(&payload).unwrap_or_default());
    if !result.ok {
        return Err(result.error.unwrap_or_else(|| "probe failed".into()));
    }
    Ok(())
}
