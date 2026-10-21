mod instance;

pub use instance::ChromeInstance;

use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

/// 主界面 / 前台调试共用的固定端口
pub const DEBUG_PORT: u16 = 9222;

/// 后台并行任务专用端口池
pub const TASK_PORT_MIN: u16 = 9232;
pub const TASK_PORT_MAX: u16 = 9331;

static TASK_PORT_CURSOR: AtomicU16 = AtomicU16::new(TASK_PORT_MIN);

pub fn debug_user_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("WEBCLAW_USER_DATA") {
        let path = PathBuf::from(p.trim());
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    crate::paths::chrome_debug_dir()
}

pub fn worker_cdp_port() -> u16 {
    std::env::var("WEBCLAW_CDP_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(19222)
}

fn worker_last_page_path(task_id: &str) -> PathBuf {
    let safe: String = task_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    crate::paths::app_data_dir().join(format!("worker-last-page-{safe}.json"))
}

pub fn save_worker_last_page(task_id: &str, url: &str, title: &str) {
    let url = url.trim();
    if url.is_empty()
        || url == "about:blank"
        || url.starts_with("chrome://")
        || url.starts_with("devtools://")
    {
        return;
    }
    let payload = json!({ "url": url, "title": title, "taskId": task_id });
    let _ = std::fs::write(worker_last_page_path(task_id), payload.to_string());
}

pub fn load_worker_last_page(task_id: &str) -> Option<String> {
    let raw = std::fs::read_to_string(worker_last_page_path(task_id)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let url = v.get("url")?.as_str()?.trim().to_string();
    if url.is_empty() || url == "about:blank" {
        return None;
    }
    Some(url)
}

/// 新任务重启浏览器；继续任务接上同一只（同一 taskId 的 profile）。
pub async fn ensure_worker_chrome(
    headless: bool,
    resume: bool,
    task_id: &str,
) -> Result<(u16, bool, String), String> {
    let port = worker_cdp_port();
    let profile = ChromeInstance::profile_for_task(task_id);
    if resume && is_debug_port_ready_on(port).await {
        let pages = list_page_targets_on(port).await.unwrap_or_default();
        let n = pages.len();
        let url = pick_page_target(&pages)
            .map(|p| p.url.clone())
            .unwrap_or_default();
        let msg = if url.starts_with("http://") || url.starts_with("https://") {
            format!("继续任务：已接上浏览器 | 端口 {port} | {n} 个标签 | {url}")
        } else {
            format!("继续任务：已接上浏览器 | 端口 {port} | {n} 个标签页")
        };
        return Ok((port, true, msg));
    }
    if is_debug_port_ready_on(port).await {
        crate::browser_network::drop_port_capture(port);
        kill_listeners_on_port(port);
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let _child = launch_debug_chrome_on(&profile, port, headless)?;
    wait_for_debug_port_on(port, 25).await?;
    let pages = list_page_targets_on(port).await.unwrap_or_default();
    if pages.is_empty() {
        create_page_target_on(port, Some("about:blank")).await?;
    }
    let msg = if resume {
        format!("继续任务：浏览器已不在，已按本任务目录重新打开 | 端口 {port}")
    } else {
        format!("新任务：已重启浏览器 | 端口 {port}")
    };
    Ok((port, false, msg))
}

pub async fn restart_worker_chrome(headless: bool, task_id: &str) -> Result<(u16, String), String> {
    let port = worker_cdp_port();
    crate::browser_network::drop_port_capture(port);
    kill_listeners_on_port(port);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_p, _reused, msg) = ensure_worker_chrome(headless, false, task_id).await?;
    Ok((port, msg))
}

#[derive(Debug, Clone, Deserialize)]
pub struct TargetInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub url: String,
    /// OOPIF 等子目标的父标签 id（Chrome /json/list 字段 parentId）
    #[serde(default, rename = "parentId")]
    pub parent_id: Option<String>,
}

/// Browser CDP Target.getTargets 返回的 page 目标（含 openerId）
#[derive(Debug, Clone)]
pub struct PageTargetWithOpener {
    pub id: String,
    pub url: String,
    pub opener_id: Option<String>,
}

/// 选取由指定父标签 window.open 出的最新子 page（排除 DevTools）
pub fn pick_child_page_target<'a>(
    infos: &'a [PageTargetWithOpener],
    opener_id: &str,
) -> Option<&'a PageTargetWithOpener> {
    infos
        .iter()
        .filter(|t| t.opener_id.as_deref() == Some(opener_id))
        .filter(|t| !is_devtools_page_url(&t.url))
        .last()
}

/// 点击后新出现的、且 opener 指向父标签的 page
pub fn pick_new_child_page_target<'a>(
    infos: &'a [PageTargetWithOpener],
    opener_id: &str,
    known_ids: &std::collections::HashSet<String>,
) -> Option<&'a PageTargetWithOpener> {
    infos
        .iter()
        .filter(|t| t.opener_id.as_deref() == Some(opener_id))
        .filter(|t| !known_ids.contains(&t.id))
        .filter(|t| !is_devtools_page_url(&t.url))
        .last()
}

/// 选取点击步骤后新出现的 page（无 openerId 时的回退）
pub fn pick_newest_unknown_page<'a>(
    infos: &'a [PageTargetWithOpener],
    known_ids: &std::collections::HashSet<String>,
    exclude_id: &str,
) -> Option<&'a PageTargetWithOpener> {
    infos
        .iter()
        .filter(|t| t.id != exclude_id)
        .filter(|t| !known_ids.contains(&t.id))
        .filter(|t| !is_devtools_page_url(&t.url))
        .last()
}

/// 从 /json/list 中选取点击后新出现的 page
pub fn pick_newest_unknown_json_page<'a>(
    pages: &'a [TargetInfo],
    known_ids: &std::collections::HashSet<String>,
    exclude_id: &str,
) -> Option<&'a TargetInfo> {
    pages
        .iter()
        .filter(|p| p.kind == "page")
        .filter(|p| p.id != exclude_id)
        .filter(|p| !known_ids.contains(&p.id))
        .filter(|p| !is_devtools_page_url(&p.url))
        .last()
}

/// 合并 CDP Target.getTargets 与 /json/list 的标签 ID 快照
pub async fn snapshot_all_page_ids_on(
    port: u16,
) -> Result<std::collections::HashSet<String>, String> {
    use std::collections::HashSet;
    let mut ids = HashSet::new();
    if let Ok(pages) = list_page_targets_on(port).await {
        for p in pages {
            if p.kind == "page" && !is_devtools_page_url(&p.url) {
                ids.insert(p.id);
            }
        }
    }
    if let Ok(targets) = crate::browser_network::get_page_targets_on(port).await {
        for t in targets {
            if !is_devtools_page_url(&t.url) {
                ids.insert(t.id);
            }
        }
    }
    Ok(ids)
}

/// 若路径为 macOS `.app` 包，解析到 `Contents/MacOS` 内可执行文件。
pub fn resolve_browser_binary(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    #[cfg(target_os = "macos")]
    {
        let name = path.file_name()?.to_str()?;
        if !name.ends_with(".app") {
            return None;
        }
        let macos_dir = path.join("Contents").join("MacOS");
        if !macos_dir.is_dir() {
            return None;
        }
        let preferred = [
            "Google Chrome",
            "Microsoft Edge",
            "Chromium",
            "Brave Browser",
            "Chrome",
        ];
        for bin in preferred {
            let candidate = macos_dir.join(bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&macos_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        entries.into_iter().next()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

fn chrome_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return vec![
            PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
        ];
    }
    #[cfg(target_os = "macos")]
    {
        let mut out = Vec::new();
        let apps = [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Chromium.app/Contents/MacOS/Chromium",
            "Brave Browser.app/Contents/MacOS/Brave Browser",
        ];
        for rel in apps {
            out.push(PathBuf::from("/Applications").join(rel));
        }
        if let Some(home) = dirs::home_dir() {
            for rel in apps {
                out.push(home.join("Applications").join(rel));
            }
        }
        return out;
    }
    #[cfg(target_os = "linux")]
    {
        return [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
            "/usr/bin/microsoft-edge",
            "/usr/bin/microsoft-edge-stable",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

pub fn find_chrome_exe() -> Option<PathBuf> {
    for p in chrome_candidates() {
        if let Some(bin) = resolve_browser_binary(&p) {
            return Some(bin);
        }
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn load_custom_chrome_path() -> Option<String> {
    crate::settings::load_settings()
        .ok()
        .map(|s| s.chrome_executable.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn resolve_chrome_executable() -> Result<PathBuf, String> {
    if let Ok(env_path) = std::env::var("WEBCLAW_CHROME") {
        let path = PathBuf::from(env_path.trim());
        if path.as_os_str().is_empty() {
            // fall through
        } else if let Some(bin) = resolve_browser_binary(&path) {
            return Ok(bin);
        } else if path.is_file() {
            return Ok(path);
        } else {
            return Err(format!(
                "WEBCLAW_CHROME 无效或不存在：{}",
                path.display()
            ));
        }
    }
    if let Some(custom) = load_custom_chrome_path() {
        let path = PathBuf::from(&custom);
        if let Some(bin) = resolve_browser_binary(&path) {
            return Ok(bin);
        }
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "设置中的浏览器路径无效或不存在：{custom}（请在「设置 → 通用」重新选择）"
        ));
    }
    find_chrome_exe().ok_or_else(|| {
        "未找到 Chrome/Edge。请安装 Google Chrome，或在「设置 → 通用」手动选择浏览器可执行文件；伙伴空间请设置 WEBCLAW_CHROME=/usr/bin/chromium-browser".into()
    })
}

pub(crate) fn debug_base_url_on(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub async fn is_debug_port_ready_on(port: u16) -> bool {
    let url = format!("{}/json/version", debug_base_url_on(port));
    reqwest::get(&url)
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

pub async fn is_debug_port_ready() -> bool {
    is_debug_port_ready_on(DEBUG_PORT).await
}

pub async fn wait_for_debug_port_on(port: u16, timeout_secs: u64) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut interval = Duration::from_millis(200);
    while std::time::Instant::now() < deadline {
        if is_debug_port_ready_on(port).await {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
        interval = (interval * 2).min(Duration::from_millis(800));
    }
    Err(format!(
        "等待 {timeout_secs}s 后仍无法连接调试端口 {port}，请确认 Chrome 已安装且端口未被占用"
    ))
}

pub async fn wait_for_debug_port(timeout_secs: u64) -> Result<(), String> {
    wait_for_debug_port_on(DEBUG_PORT, timeout_secs).await
}

/// 将主调试 Chrome（9222）的 Cookie 同步到任务专用端口，使后台任务携带相同登录态。
pub async fn sync_cookies_from_main_port(target_port: u16) -> Result<usize, String> {
    if target_port == DEBUG_PORT {
        return Ok(0);
    }
    if !is_debug_port_ready_on(DEBUG_PORT).await {
        return Ok(0);
    }
    if !is_debug_port_ready_on(target_port).await {
        return Err(format!("任务调试端口 {target_port} 未就绪，无法同步 Cookie"));
    }

    let main = crate::cdp::CdpClient::connect_on(DEBUG_PORT).await?;
    let cookies_v = main
        .call_result("Network.getAllCookies", json!({}))
        .await?;
    let cookies = cookies_v
        .get("cookies")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    if cookies.is_empty() {
        return Ok(0);
    }

    let target = crate::cdp::CdpClient::connect_on(target_port).await?;
    target
        .call_result("Network.setCookies", json!({ "cookies": cookies }))
        .await?;
    Ok(cookies.len())
}

pub async fn allocate_task_debug_port() -> Result<u16, String> {
    if let Ok(raw) = std::env::var("WEBCLAW_CDP_PORT") {
        let port: u16 = raw
            .trim()
            .parse()
            .map_err(|_| format!("WEBCLAW_CDP_PORT 无效: {raw}"))?;
        if is_debug_port_ready_on(port).await {
            return Err(format!(
                "WEBCLAW_CDP_PORT={port} 已被占用。请换端口或结束占用进程（勿杀平台 9222 上的 /usr/local/bin/browser）"
            ));
        }
        return Ok(port);
    }
    for _ in 0..(TASK_PORT_MAX - TASK_PORT_MIN + 1) {
        let p = TASK_PORT_CURSOR.fetch_add(1, Ordering::SeqCst);
        let port = if p > TASK_PORT_MAX {
            TASK_PORT_CURSOR.store(TASK_PORT_MIN, Ordering::SeqCst);
            TASK_PORT_MIN
        } else {
            p
        };
        if !is_debug_port_ready_on(port).await {
            return Ok(port);
        }
    }
    Err(format!(
        "无可用任务调试端口（{TASK_PORT_MIN}–{TASK_PORT_MAX} 均被占用），请稍后再试或减少并行任务数"
    ))
}

fn want_container_chrome_flags() -> bool {
    if std::env::var("WEBCLAW_CONTAINER")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }
    // 伙伴空间 / Linux worker：无 GUI、常在容器内
    cfg!(target_os = "linux")
        && std::env::var("WEBCLAW_WORKER")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

/// Debug Chrome 启动参数（单元测试可校验关键 flag）。
pub(crate) fn debug_chrome_launch_args(profile: &Path, port: u16, headless: bool) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        "--remote-allow-origins=*".to_string(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        // RPA 场景：站点登录/导出常依赖 window.open 或跳转子页，默认拦截会导致有效页面打不开
        "--disable-popup-blocking".to_string(),
        // macOS 窗口遮挡时默认会把所有标签标为 hidden，导致无法用 visibilityState 识别活动页签；
        // 与 Playwright/Selenium 一致，关闭遮挡后台化，使选中标签在 webclaw 前台时仍报 visible。
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-renderer-backgrounding".to_string(),
        // 自动化专用：抑制「要恢复页面吗 / Chrome 未正确关闭」、信息栏、翻译等打扰气泡
        "--hide-crash-restore-bubble".to_string(),
        "--disable-session-crashed-bubble".to_string(),
        "--disable-infobars".to_string(),
        "--disable-translate".to_string(),
        "--disable-notifications".to_string(),
        "--disable-sync".to_string(),
        "--disable-features=Translate,TranslateUI,PasswordManagerOnboarding,ChromeWhatsNewUI,InfiniteSessionRestore".to_string(),
        // 内网 HTTP 导出：自动化运行时由 client_download 捕获 URL 并 deny 浏览器下载；
        // 空闲时恢复 allow，用户可手动走 Chrome 下载栏（含 HTTP「保留」）。
    ];
    if want_container_chrome_flags() {
        args.push("--no-sandbox".to_string());
        args.push("--disable-dev-shm-usage".to_string());
        args.push("--disable-gpu".to_string());
    }
    // 强制站点隔离，确保跨域 iframe 成为独立 OOPIF target（/json/list type=iframe）。
    // 无头 Chromium / 部分发行版默认弱隔离时，指令与探针会扫不到跨域内容。
    args.push("--site-per-process".to_string());
    if !headless && std::env::var_os("WEBCLAW_LITE").is_some() {
        args.push("--window-size=1280,900".to_string());
        args.push("--window-position=80,60".to_string());
    }
    if headless {
        args.push("--headless=new".to_string());
        // 无头默认视口过小会导致登录验证码 canvas/图片布局为 0，检测失败不弹窗
        args.push("--window-size=1280,900".to_string());
        args.push("--force-device-scale-factor=1".to_string());
    }
    args.push("about:blank".to_string());
    args
}

pub(crate) fn launch_debug_chrome_on(
    profile: &Path,
    port: u16,
    headless: bool,
) -> Result<Child, String> {
    let exe = resolve_chrome_executable()?;
    std::fs::create_dir_all(profile).map_err(|e| e.to_string())?;
    seed_automation_profile_prefs(profile)?;

    let mut cmd = Command::new(&exe);
    for arg in debug_chrome_launch_args(profile, port, headless) {
        cmd.arg(arg);
    }
    // 独立进程组：宿主机短命退出时不要把浏览器一起带走（续跑要接同一 CDP）
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Break away from the parent's console/job so a short-lived CLI (DSH
        // spawn) is not kept waiting on Chrome stdio / process-tree handles.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    // Always detach stdio: otherwise Node/DSH waiting on lite's pipes never sees
    // EOF while the kept-alive Chrome still holds inherited handles.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn()
        .map_err(|e| format!("启动浏览器失败（{}）: {e}", exe.display()))
}

/// 为自动化专用 Profile 预置内容设置，并抹掉「未正确关闭 / 保存密码」等提示状态。
pub(crate) fn seed_automation_profile_prefs(profile: &Path) -> Result<(), String> {
    let default_dir = profile.join("Default");
    std::fs::create_dir_all(&default_dir).map_err(|e| e.to_string())?;
    let prefs_path = default_dir.join("Preferences");

    let mut root: Value = if prefs_path.exists() {
        let data = std::fs::read_to_string(&prefs_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&data).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let Some(root_obj) = root.as_object_mut() else {
        return Err("Preferences 根节点不是对象".into());
    };
    if !root_obj.contains_key("profile") {
        root_obj.insert("profile".to_string(), json!({}));
    }
    let Some(profile_obj) = root_obj
        .get_mut("profile")
        .and_then(|v| v.as_object_mut())
    else {
        return Err("Preferences.profile 不是对象".into());
    };

    let defaults = profile_obj
        .entry("default_content_setting_values")
        .or_insert_with(|| json!({}));
    if let Some(defaults_obj) = defaults.as_object_mut() {
        // 1 = allow, 2 = block（与 Chromium content_settings 一致）
        defaults_obj.insert("popups".into(), json!(1));
        defaults_obj.insert("automatic_downloads".into(), json!(1));
        // 2 = block：站点通知气泡
        defaults_obj.insert("notifications".into(), json!(2));
    }

    // 每次启动前标记为正常退出，避免「要恢复页面吗？Chrome 未正确关闭」
    profile_obj.insert("exit_type".into(), json!("Normal"));
    profile_obj.insert("exited_cleanly".into(), json!(true));
    // 关闭密码管理器：「要保存密码吗？」
    profile_obj.insert("password_manager_enabled".into(), json!(false));
    root_obj.insert("credentials_enable_service".into(), json!(false));
    root_obj.insert("credentials_enable_autosignin".into(), json!(false));

    // 不恢复上次会话标签
    let session = root_obj.entry("session").or_insert_with(|| json!({}));
    if let Some(session_obj) = session.as_object_mut() {
        // 5 = OpenNewTabPage（Chromium SessionStartupPref）
        session_obj.insert("restore_on_startup".into(), json!(5));
    }

    // 放宽 Safe Browsing 对自动化下载的拦截（Preferences 侧）
    let safebrowsing = root_obj
        .entry("safebrowsing")
        .or_insert_with(|| json!({}));
    if let Some(sb) = safebrowsing.as_object_mut() {
        sb.insert("enabled".into(), json!(false));
    }

    let data = serde_json::to_string(&root).map_err(|e| e.to_string())?;
    std::fs::write(&prefs_path, data).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn launch_debug_chrome(headless: bool) -> Result<Child, String> {
    launch_debug_chrome_on(&debug_user_data_dir(), DEBUG_PORT, headless)
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionInfo {
    #[serde(rename = "webSocketDebuggerUrl")]
    pub web_socket_debugger_url: String,
}

pub async fn browser_ws_url_on(port: u16) -> Result<String, String> {
    let url = format!("{}/json/version", debug_base_url_on(port));
    let info: VersionInfo = reqwest::get(&url)
        .await
        .map_err(|e| format!("无法访问 {url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("解析 version 失败: {e}"))?;
    if info.web_socket_debugger_url.is_empty() {
        return Err("未返回 browser WebSocket URL".into());
    }
    Ok(info.web_socket_debugger_url)
}

pub async fn list_targets_on(port: u16) -> Result<Vec<TargetInfo>, String> {
    let url = format!("{}/json/list", debug_base_url_on(port));
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("无法访问调试端口 {port}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("调试端口 {port} 返回 HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn list_targets() -> Result<Vec<TargetInfo>, String> {
    list_targets_on(DEBUG_PORT).await
}

pub async fn list_page_targets_on(port: u16) -> Result<Vec<TargetInfo>, String> {
    let targets = list_targets_on(port).await?;
    Ok(targets.into_iter().filter(|t| t.kind == "page").collect())
}

pub async fn page_target_exists_on(port: u16, target_id: &str) -> Result<bool, String> {
    let pages = list_page_targets_on(port).await?;
    Ok(pages.iter().any(|p| p.id == target_id))
}

pub async fn list_page_targets() -> Result<Vec<TargetInfo>, String> {
    list_page_targets_on(DEBUG_PORT).await
}

/// 列出某 page 下的 OOPIF 子目标（跨域 iframe 在 Chrome 中为独立 type=iframe target）
pub async fn list_iframe_targets_for_parent_on(
    port: u16,
    parent_id: &str,
) -> Result<Vec<TargetInfo>, String> {
    Ok(discover_oopif_targets_for_parent(port, parent_id, None, DiscoverOopifMode::Peek).await)
}

/// OOPIF 发现策略：壳层步骤用 Peek；业务 iframe / 发现为空且 FrameTree 有子节点时用 Wait。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverOopifMode {
    /// 单次 /json/list + 一次 Browser CDP（父页菜单等壳层步骤）
    Peek,
    /// FrameTree 已有子 frame 却仍空时短轮询（跨域业务 iframe）
    Wait,
}

async fn collect_oopif_targets_once(port: u16, parent_id: &str) -> Vec<TargetInfo> {
    let targets = match list_targets_on(port).await {
        Ok(t) => t,
        Err(_) => Vec::new(),
    };
    let page_count = targets.iter().filter(|t| t.kind == "page").count();
    let iframes: Vec<TargetInfo> = targets
        .into_iter()
        .filter(|t| t.kind == "iframe")
        .filter(|t| !is_devtools_page_url(&t.url))
        .collect();

    let mut matched: Vec<TargetInfo> = iframes
        .iter()
        .filter(|t| t.parent_id.as_deref() == Some(parent_id))
        .cloned()
        .collect();

    // 部分无头 Chromium 的 /json/list 不带 parentId：单 page 会话下把全部 iframe 视为其子目标
    if matched.is_empty() && page_count == 1 && !iframes.is_empty() {
        matched = iframes;
    }

    // /json/list 仍为空时，走 Browser CDP Target.getTargets（Linux 无头常漏报）
    if matched.is_empty() {
        if let Ok(extra) = list_iframe_targets_via_browser_cdp(port, parent_id).await {
            matched = extra;
        }
    }
    matched
}

/// 发现跨域 OOPIF target。
///
/// - 不要把「父页已有同源 ctx」当成已经扫到业务 iframe。
/// - FrameTree 声明了子 frame 但 list 仍空时：Wait 模式短轮询 + 反复 Browser CDP。
pub async fn discover_oopif_targets_for_parent(
    port: u16,
    parent_id: &str,
    parent: Option<&crate::cdp::CdpClient>,
    mode: DiscoverOopifMode,
) -> Vec<TargetInfo> {
    let mut matched = collect_oopif_targets_once(port, parent_id).await;
    if !matched.is_empty() {
        return matched;
    }

    let tree_has_children = match parent {
        Some(p) => p.frame_tree_has_child_frames().await,
        None => false,
    };
    if !tree_has_children {
        // 再兜一次 Browser CDP（无 FrameTree 线索时也不要过早放弃）
        if let Ok(extra) = list_iframe_targets_via_browser_cdp(port, parent_id).await {
            if !extra.is_empty() {
                return extra;
            }
        }
        return matched;
    }

    if mode == DiscoverOopifMode::Peek {
        if let Ok(extra) = list_iframe_targets_via_browser_cdp(port, parent_id).await {
            return extra;
        }
        return matched;
    }

    // Wait：FrameTree 已有子节点，target 尚未暴露时短轮询（Linux 远程无头常见）
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        matched = collect_oopif_targets_once(port, parent_id).await;
        if !matched.is_empty() {
            return matched;
        }
    }
    matched
}

/// Browser CDP 补充发现 type=iframe（部分发行版 HTTP /json/list 漏报 OOPIF）
async fn list_iframe_targets_via_browser_cdp(
    port: u16,
    parent_id: &str,
) -> Result<Vec<TargetInfo>, String> {
    let infos = crate::browser_network::get_iframe_targets_on(port).await?;
    let page_count = list_page_targets_on(port).await.map(|p| p.len()).unwrap_or(0);
    let cleaned: Vec<TargetInfo> = infos
        .into_iter()
        .filter(|t| !is_devtools_page_url(&t.url))
        .collect();
    let mut matched: Vec<TargetInfo> = cleaned
        .iter()
        .filter(|t| t.parent_id.as_deref() == Some(parent_id))
        .cloned()
        .collect();
    if matched.is_empty() && page_count == 1 {
        matched = cleaned;
    }
    Ok(matched)
}

const FOREGROUND_PROBE_JS: &str = r#"(() => {
  try {
    return {
      focused: !!document.hasFocus(),
      visible: document.visibilityState === 'visible' && !document.hidden,
      title: document.title || '',
      url: location.href || '',
    };
  } catch (e) {
    return { focused: false, visible: false, title: '', url: '' };
  }
})()"#;

/// 按 hasFocus / visibilityState 给候选标签打分，返回最高分目标。
async fn score_foreground_candidates(
    port: u16,
    candidates: &[TargetInfo],
) -> Option<TargetInfo> {
    let mut best: Option<(TargetInfo, i32)> = None;
    for page in candidates {
        let Ok(client) = crate::cdp::CdpClient::connect_to_target_on(port, &page.id).await else {
            continue;
        };
        let Ok(v) = client.evaluate(FOREGROUND_PROBE_JS).await else {
            continue;
        };
        let focused = v.get("focused").and_then(|x| x.as_bool()).unwrap_or(false);
        let visible = v.get("visible").and_then(|x| x.as_bool()).unwrap_or(false);
        let score = if focused {
            100
        } else if visible {
            50
        } else {
            0
        };
        if score == 0 {
            continue;
        }
        let mut info = page.clone();
        if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
            if !t.is_empty() {
                info.title = t.to_string();
            }
        }
        if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
            if !u.is_empty() {
                info.url = u.to_string();
            }
        }
        if best.as_ref().map(|b| b.1).unwrap_or(-1) < score {
            best = Some((info, score));
            if focused {
                break;
            }
        }
    }
    best.map(|(page, _)| page)
}

/// 仅返回 `document.hasFocus()` 为 true 的标签（用于步骤跟随：当前页仍可见时勿误跟到另一可见窗）。
pub async fn find_focused_page_on(port: u16) -> Option<TargetInfo> {
    let pages = list_page_targets_on(port).await.ok()?;
    let candidates: Vec<TargetInfo> = pages
        .into_iter()
        .filter(|t| t.kind == "page")
        .filter(|t| !is_devtools_page_url(&t.url))
        .collect();
    for page in candidates {
        let Ok(client) = crate::cdp::CdpClient::connect_to_target_on(port, &page.id).await else {
            continue;
        };
        let Ok(v) = client.evaluate(FOREGROUND_PROBE_JS).await else {
            continue;
        };
        if v.get("focused").and_then(|x| x.as_bool()) != Some(true) {
            continue;
        }
        let mut info = page;
        if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
            if !t.is_empty() {
                info.title = t.to_string();
            }
        }
        if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
            if !u.is_empty() {
                info.url = u.to_string();
            }
        }
        return Some(info);
    }
    None
}

/// 探测当前前台聚焦标签：优先 `document.hasFocus()`，其次 `visibilityState === 'visible'`。
/// `allow_mac_unocclude`：macOS 上若全部 score=0（窗口被遮挡），是否短暂置前浏览器再探测。
/// 页面探针应传 `true`；任务执行中的 sync 传 `false`，避免每步抢焦点。
pub async fn find_foreground_page_on(port: u16) -> Result<TargetInfo, String> {
    find_foreground_page_on_ex(port, false).await
}

pub async fn find_foreground_page_on_ex(
    port: u16,
    allow_mac_unocclude: bool,
) -> Result<TargetInfo, String> {
    let _ = close_devtools_page_targets_on(port).await;
    let pages = list_page_targets_on(port).await?;
    let candidates: Vec<TargetInfo> = pages
        .into_iter()
        .filter(|t| t.kind == "page")
        .filter(|t| !is_devtools_page_url(&t.url))
        .collect();
    if candidates.is_empty() {
        return Err("没有可用的浏览器标签页".into());
    }

    if let Some(page) = score_foreground_candidates(port, &candidates).await {
        return Ok(page);
    }

    #[cfg(target_os = "macos")]
    if allow_mac_unocclude {
        if let Some(page) = probe_foreground_after_unocclude(port, &candidates).await {
            return Ok(page);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = allow_mac_unocclude;

    pick_page_target(&candidates)
        .cloned()
        .ok_or_else(|| "无法确定当前活动标签页".to_string())
}

/// macOS：窗口遮挡导致 visibility 全灭时，短暂置前 Debug Chrome 再读活动页签。
#[cfg(target_os = "macos")]
async fn probe_foreground_after_unocclude(
    port: u16,
    candidates: &[TargetInfo],
) -> Option<TargetInfo> {
    if !activate_browser_listening_on_port(port) {
        return None;
    }
    // 等待 macOS 取消 occlusion 并派发 visibilitychange
    tokio::time::sleep(Duration::from_millis(250)).await;
    let page = score_foreground_candidates(port, candidates).await;
    yield_focus_to_webclaw();
    page
}

/// 将监听指定调试端口的浏览器进程置为前台（不切换其内部页签）。
#[cfg(target_os = "macos")]
fn activate_browser_listening_on_port(port: u16) -> bool {
    let Some(pid) = find_listener_pid(port) else {
        return false;
    };
    let script = format!(
        r#"tell application "System Events" to set frontmost of first process whose unix id is {pid} to true"#
    );
    std::process::Command::new("osascript")
        .args(["-e", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub async fn find_foreground_page() -> Result<TargetInfo, String> {
    find_foreground_page_on(DEBUG_PORT).await
}

pub fn is_devtools_page_url(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("devtools://")
        || u.starts_with("chrome://devtools")
        || u.contains("devtools_app.html")
}

/// 关闭 DevTools 附着的标签页，避免「Debugger paused in another tab」导致页面假死
pub async fn close_devtools_page_targets_on(port: u16) -> Result<usize, String> {
    let pages = list_page_targets_on(port).await?;
    let mut closed = 0usize;
    for p in pages {
        if !is_devtools_page_url(&p.url) {
            continue;
        }
        let url = format!("{}/json/close/{}", debug_base_url_on(port), p.id);
        let resp = reqwest::get(&url)
            .await
            .map_err(|e| format!("关闭 DevTools 标签失败: {e}"))?;
        if resp.status().is_success() {
            closed += 1;
        }
    }
    Ok(closed)
}

pub async fn close_devtools_page_targets() -> Result<usize, String> {
    close_devtools_page_targets_on(DEBUG_PORT).await
}

/// 普通 page 标签（排除 DevTools），顺序与 Chrome `/json/list` 一致，供「页签#N」使用。
pub async fn list_recipe_page_targets_on(port: u16) -> Result<Vec<TargetInfo>, String> {
    let pages = list_page_targets_on(port).await?;
    Ok(pages
        .into_iter()
        .filter(|t| !is_devtools_page_url(&t.url))
        .collect())
}

/// 关闭指定页面标签（`/json/close/{id}`）
pub async fn close_page_target_on(port: u16, target_id: &str) -> Result<(), String> {
    let url = format!("{}/json/close/{}", debug_base_url_on(port), target_id);
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("关闭标签页失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("关闭标签页 HTTP {}", resp.status()));
    }
    Ok(())
}

pub async fn create_page_target_on(port: u16, url: Option<&str>) -> Result<TargetInfo, String> {
    let base = debug_base_url_on(port);
    // 仅创建空白标签；带 URL 的导航由调用方通过 CDP Page.navigate 完成（避免 /json/new?url 编码问题）
    let endpoint = format!("{base}/json/new");
    let client = reqwest::Client::new();
    let resp = match client.put(&endpoint).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => reqwest::get(&endpoint)
            .await
            .map_err(|e| format!("创建标签页失败: {e}"))?,
    };
    if !resp.status().is_success() {
        return Err(format!("创建标签页 HTTP {}", resp.status()));
    }
    let target: TargetInfo = resp
        .json()
        .await
        .map_err(|e| format!("解析新标签页信息失败: {e}"))?;

    // 兼容旧调用：若传入 URL 且 create 后 target 仍空白，由上层 attach 后 navigate
    let _ = url;
    Ok(target)
}

pub async fn create_page_target(url: Option<&str>) -> Result<TargetInfo, String> {
    create_page_target_on(DEBUG_PORT, url).await
}

pub fn pick_page_target<'a>(pages: &'a [TargetInfo]) -> Option<&'a TargetInfo> {
    if pages.is_empty() {
        return None;
    }
    const SKIP: &[&str] = &["chrome://devtools", "chrome://inspect", "devtools://"];

    let mut pool: Vec<&TargetInfo> = pages
        .iter()
        .filter(|t| !SKIP.iter().any(|s| t.url.starts_with(s)))
        .collect();
    if pool.is_empty() {
        pool = pages.iter().collect();
    }

    if let Some(p) = pool
        .iter()
        .find(|t| t.url.starts_with("http://") || t.url.starts_with("https://"))
    {
        return Some(*p);
    }
    if let Some(p) = pool.iter().find(|t| {
        t.url == "about:blank"
            || t.url.starts_with("chrome://newtab")
            || t.url.starts_with("chrome://blank")
            || !t.url.is_empty()
    }) {
        return Some(*p);
    }
    pool.first().copied()
}

pub async fn ensure_page_target_on(port: u16) -> Result<TargetInfo, String> {
    for attempt in 1..=5 {
        let pages = list_page_targets_on(port).await?;
        if let Some(p) = pick_page_target(&pages) {
            return Ok(p.clone());
        }
        if attempt == 1 {
            create_page_target_on(port, Some("about:blank")).await?;
        } else {
            tokio::time::sleep(Duration::from_millis(400 * attempt)).await;
        }
    }
    let pages = list_page_targets_on(port).await?;
    pick_page_target(&pages)
        .cloned()
        .ok_or_else(|| "无法获取可用的浏览器标签页".to_string())
}

pub async fn ensure_page_target() -> Result<TargetInfo, String> {
    ensure_page_target_on(DEBUG_PORT).await
}

fn clear_child_if_exited(slot: &mut Option<Child>) {
    let Some(child) = slot.as_mut() else {
        return;
    };
    match child.try_wait() {
        Ok(Some(_)) | Err(_) => {
            *slot = None;
        }
        Ok(None) => {}
    }
}

fn kill_and_clear_child(slot: &mut Option<Child>) {
    if let Some(mut child) = slot.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// 结束占用调试端口的浏览器进程（含子进程树），避免更新重启后孤儿 Chrome 抢焦点。
pub fn kill_listeners_on_port(port: u16) {
    for _ in 0..5 {
        let Some(pid) = find_listener_pid(port) else {
            break;
        };
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// 清理本进程拉起的 Chrome 子进程，并杀掉仍占用 DEBUG_PORT 的孤儿监听。
pub fn shutdown_debug_chrome(chrome_child: &parking_lot::Mutex<Option<Child>>) {
    crate::browser_network::drop_port_capture(DEBUG_PORT);
    kill_and_clear_child(&mut *chrome_child.lock());
    kill_listeners_on_port(DEBUG_PORT);
}

/// 查找监听指定 TCP 端口的进程 PID（跨平台）。
pub(crate) fn find_listener_pid(port: u16) -> Option<u32> {
    #[cfg(unix)]
    {
        let output = Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().parse::<u32>().ok());
    }
    #[cfg(windows)]
    {
        // 优先用 WinAPI，避免每次 netstat 弹黑框；失败再静默回退 netstat。
        if let Some(pid) = find_listener_pid_winapi(port) {
            return Some(pid);
        }
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = Command::new("netstat")
            .args(["-ano"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let needle = format!(":{port}");
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let line = line.trim();
            if !line.contains(&needle) {
                continue;
            }
            // Proto  Local Address  Foreign  State  PID — 只要 LISTENING
            if !line.to_ascii_uppercase().contains("LISTENING") {
                continue;
            }
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() < 5 {
                continue;
            }
            // local addr 通常在 parts[1]
            if !parts[1].ends_with(&needle) && !parts[1].contains(&format!("]{needle}")) {
                continue;
            }
            if let Ok(pid) = parts[parts.len() - 1].parse::<u32>() {
                if pid > 0 {
                    return Some(pid);
                }
            }
        }
        None
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = port;
        None
    }
}

/// Windows：通过 TCP 表查监听端口的 PID（不拉起控制台进程）。
#[cfg(windows)]
fn find_listener_pid_winapi(port: u16) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP_STATE_LISTEN, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    unsafe {
        let mut size = 0u32;
        let _ = GetExtendedTcpTable(
            None,
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let status = GetExtendedTcpTable(
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if status != 0 {
            return None;
        }
        // MIB_TCPTABLE_OWNER_PID: DWORD dwNumEntries; MIB_TCPROW_OWNER_PID table[1];
        if buf.len() < 4 {
            return None;
        }
        let num = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        // Each MIB_TCPROW_OWNER_PID is 24 bytes on x86/x64:
        // dwState(4) dwLocalAddr(4) dwLocalPort(4) dwRemoteAddr(4) dwRemotePort(4) dwOwningPid(4)
        const ROW: usize = 24;
        let table = &buf[4..];
        for i in 0..num {
            let off = i * ROW;
            if off + ROW > table.len() {
                break;
            }
            let state = u32::from_le_bytes(table[off..off + 4].try_into().ok()?);
            if state != MIB_TCP_STATE_LISTEN.0 as u32 {
                continue;
            }
            let local_port_net =
                u32::from_le_bytes(table[off + 8..off + 12].try_into().ok()?);
            // dwLocalPort 高 16 位为网络字节序端口
            let local_port = ((local_port_net >> 8) & 0xff) | ((local_port_net & 0xff) << 8);
            let local_port = local_port as u16;
            if local_port != port {
                continue;
            }
            let pid = u32::from_le_bytes(table[off + 20..off + 24].try_into().ok()?);
            if pid > 0 {
                return Some(pid);
            }
        }
        None
    }
}

/// macOS：Chrome 启动后会抢前台，把焦点交回 webclaw，便于继续操作客户端。
#[cfg(target_os = "macos")]
pub(crate) fn yield_focus_to_webclaw() {
    // 优先按 bundle id（正式包）；失败再按进程名（cargo run 开发态）
    let scripts = [
        r#"tell application id "com.webclaw.client" to activate"#,
        r#"tell application "webclaw" to activate"#,
        r#"tell application "System Events" to set frontmost of first process whose name contains "webclaw" to true"#,
    ];
    for script in scripts {
        let ok = std::process::Command::new("osascript")
            .args(["-e", script])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            break;
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn yield_focus_to_webclaw() {}

async fn spawn_debug_chrome_and_wait(
    chrome_child: &parking_lot::Mutex<Option<Child>>,
    headless: bool,
) -> Result<(), String> {
    // 旧进程/端口上的 Browser WS 已失效，避免复用后报 closed connection
    crate::browser_network::drop_port_capture(DEBUG_PORT);
    kill_and_clear_child(&mut *chrome_child.lock());
    let child = launch_debug_chrome(headless)?;
    *chrome_child.lock() = Some(child);
    wait_for_debug_port(20).await?;
    if !headless {
        // 给 Chrome 一点时间完成窗口激活，再交回焦点
        tokio::time::sleep(Duration::from_millis(350)).await;
        yield_focus_to_webclaw();
    }
    Ok(())
}

pub async fn ensure_debug_chrome_ready(
    chrome_child: &parking_lot::Mutex<Option<Child>>,
    headless: bool,
) -> Result<String, String> {
    {
        let mut guard = chrome_child.lock();
        clear_child_if_exited(&mut guard);
    }

    if is_debug_port_ready().await {
        let _ = close_devtools_page_targets().await;
        let pages = list_page_targets().await.unwrap_or_default();
        let n = pages.len();
        return Ok(format!("调试端口已就绪（{n} 个标签页）"));
    }

    let has_live_child = chrome_child.lock().is_some();
    if !has_live_child {
        spawn_debug_chrome_and_wait(chrome_child, headless).await?;
    } else if !(wait_for_debug_port(8).await.is_ok() && is_debug_port_ready().await) {
        spawn_debug_chrome_and_wait(chrome_child, headless).await?;
    }

    let pages = list_page_targets().await.unwrap_or_default();
    if pages.is_empty() {
        create_page_target(Some("about:blank")).await?;
    }
    let _ = close_devtools_page_targets().await;
    let n = list_page_targets().await.map(|p| p.len()).unwrap_or(1);
    Ok(format!(
        "Chrome 已就绪 | 端口 {DEBUG_PORT} | 标签页 {n} 个"
    ))
}

/// 仅连接已运行的调试浏览器，不主动拉起（「打开」「登录」以外的步骤用）。
pub async fn require_debug_chrome_ready() -> Result<String, String> {
    if is_debug_port_ready().await {
        let _ = close_devtools_page_targets().await;
        let pages = list_page_targets().await.unwrap_or_default();
        let n = pages.len();
        return Ok(format!("调试端口已就绪（{n} 个标签页）"));
    }
    Err(
        "当前未打开浏览器。请先执行「打开」或「登录」，再运行需要页面的步骤。".into(),
    )
}

/// 强制结束占用 DEBUG_PORT 的 Chrome 并重新拉起（OOM/卡死后恢复用）。
/// 注意：会丢失当前标签与未持久化的登录态。
pub async fn force_restart_debug_chrome(
    chrome_child: &parking_lot::Mutex<Option<Child>>,
    headless: bool,
) -> Result<String, String> {
    shutdown_debug_chrome(chrome_child);
    // 再扫一轮，清掉孤儿监听
    kill_listeners_on_port(DEBUG_PORT);
    tokio::time::sleep(Duration::from_millis(200)).await;
    spawn_debug_chrome_and_wait(chrome_child, headless).await?;
    let pages = list_page_targets().await.unwrap_or_default();
    if pages.is_empty() {
        create_page_target(Some("about:blank")).await?;
    }
    let n = list_page_targets().await.map(|p| p.len()).unwrap_or(1);
    Ok(format!(
        "已强制重启调试浏览器 | 端口 {DEBUG_PORT} | 标签页 {n} 个（登录态可能需重新登录）"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn seed_automation_profile_prefs_allows_popups() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-prefs-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        seed_automation_profile_prefs(&dir).unwrap();
        let prefs_path = dir.join("Default").join("Preferences");
        let data = fs::read_to_string(&prefs_path).unwrap();
        let v: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(
            v["profile"]["default_content_setting_values"]["popups"],
            1
        );
        assert_eq!(
            v["profile"]["default_content_setting_values"]["automatic_downloads"],
            1
        );
        assert_eq!(v["profile"]["exit_type"], "Normal");
        assert_eq!(v["profile"]["exited_cleanly"], true);
        assert_eq!(v["profile"]["password_manager_enabled"], false);
        assert_eq!(v["credentials_enable_service"], false);
        assert_eq!(v["session"]["restore_on_startup"], 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn debug_chrome_launch_args_include_popup_blocking() {
        let dir = std::env::temp_dir().join("webclaw-launch-args-test");
        let args = debug_chrome_launch_args(&dir, 9222, false);
        assert!(args.iter().any(|a| a == "--disable-popup-blocking"));
        assert!(args
            .iter()
            .any(|a| a == "--disable-backgrounding-occluded-windows"));
        assert!(args
            .iter()
            .any(|a| a == "--disable-renderer-backgrounding"));
        assert!(args.iter().any(|a| a == "--hide-crash-restore-bubble"));
        assert!(args.iter().any(|a| a == "--disable-session-crashed-bubble"));
        assert!(args.iter().any(|a| a.starts_with("--disable-features=")));
        assert!(args.iter().any(|a| a.starts_with("--remote-debugging-port=")));
        assert!(args.iter().any(|a| a == "--site-per-process"));
        assert!(!args.iter().any(|a| a == "--test-type"));
        assert!(!args
            .iter()
            .any(|a| a.starts_with("--unsafely-treat-insecure-origin-as-secure=")));
        assert!(!args.iter().any(|a| a == "--headless=new"));
        assert!(!args.iter().any(|a| a.starts_with("--window-size=")));
    }

    #[test]
    fn debug_chrome_launch_args_headless_has_window_size() {
        let dir = std::env::temp_dir().join("webclaw-launch-args-headless-test");
        let args = debug_chrome_launch_args(&dir, 9233, true);
        assert!(args.iter().any(|a| a == "--headless=new"));
        assert!(args.iter().any(|a| a == "--window-size=1280,900"));
        assert!(args.iter().any(|a| a == "--force-device-scale-factor=1"));
        assert!(args.iter().any(|a| a == "--site-per-process"));
    }

    #[test]
    fn is_devtools_page_url_detects_devtools_targets() {
        assert!(is_devtools_page_url(
            "devtools://devtools/bundled/devtools_app.html?remoteBase=..."
        ));
        assert!(is_devtools_page_url("chrome://devtools/"));
        assert!(!is_devtools_page_url("https://example.com"));
    }

    #[test]
    fn pick_child_page_target_prefers_newest_non_devtools_child() {
        let infos = vec![
            PageTargetWithOpener {
                id: "old".into(),
                url: "https://a.example".into(),
                opener_id: Some("parent".into()),
            },
            PageTargetWithOpener {
                id: "devtools".into(),
                url: "devtools://devtools/bundled/devtools_app.html".into(),
                opener_id: Some("parent".into()),
            },
            PageTargetWithOpener {
                id: "new".into(),
                url: "https://b.example".into(),
                opener_id: Some("parent".into()),
            },
            PageTargetWithOpener {
                id: "other".into(),
                url: "https://c.example".into(),
                opener_id: Some("other-parent".into()),
            },
        ];
        let picked = pick_child_page_target(&infos, "parent").unwrap();
        assert_eq!(picked.id, "new");
    }

    #[test]
    fn pick_child_page_target_returns_none_without_opener_match() {
        let infos = vec![PageTargetWithOpener {
            id: "solo".into(),
            url: "https://example.com".into(),
            opener_id: None,
        }];
        assert!(pick_child_page_target(&infos, "parent").is_none());
    }

    #[test]
    fn pick_new_child_page_target_ignores_existing_tabs() {
        let known: HashSet<String> = ["old-child".into(), "parent".into()].into_iter().collect();
        let infos = vec![
            PageTargetWithOpener {
                id: "old-child".into(),
                url: "https://child.example".into(),
                opener_id: Some("parent".into()),
            },
            PageTargetWithOpener {
                id: "parent".into(),
                url: "https://parent.example".into(),
                opener_id: None,
            },
        ];
        assert!(pick_new_child_page_target(&infos, "parent", &known).is_none());
    }

    #[test]
    fn pick_new_child_page_target_finds_fresh_popup() {
        let known: HashSet<String> = ["parent".into()].into_iter().collect();
        let infos = vec![
            PageTargetWithOpener {
                id: "parent".into(),
                url: "https://parent.example".into(),
                opener_id: None,
            },
            PageTargetWithOpener {
                id: "new-child".into(),
                url: "about:blank".into(),
                opener_id: Some("parent".into()),
            },
        ];
        let picked = pick_new_child_page_target(&infos, "parent", &known).unwrap();
        assert_eq!(picked.id, "new-child");
    }
}
