//! 在线更新：按平台拉取 latest.json / latest-macos.json → 下载 → 校验 sha256 → 安装重启。
//!
//! - Windows：下载 exe，脱离进程后覆盖并重启
//! - macOS：下载 dmg，挂载后覆盖 .app 并重新打开

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Default update base (empty in OSS — set settings or env).
pub const DEFAULT_UPDATE_BASE_URL: &str = "";

/// 兼容旧代码的别名
pub const UPDATE_BASE_URL: &str = DEFAULT_UPDATE_BASE_URL;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestInfo {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    pub file: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
}

/// 兼容本地 mac 更新 UI 的类型别名
pub type LatestMeta = LatestInfo;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckUpdateResult {
    pub current_version: String,
    pub update_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<LatestInfo>,
    pub message: String,
    /// 实际使用的更新基址（便于排查）
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub update_available: bool,
    pub latest: Option<LatestMeta>,
    pub download_url: Option<String>,
    pub meta_url: String,
    pub platform: String,
    pub arch: String,
    pub message: String,
}

fn parse_semver(v: &str) -> Result<(u64, u64, u64), String> {
    let v = v.trim().trim_start_matches('v');
    let parts: Vec<_> = v.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("无效版本号：{v}"));
    }
    let major = parts[0]
        .parse::<u64>()
        .map_err(|_| format!("无效版本号：{v}"))?;
    let minor = parts[1]
        .parse::<u64>()
        .map_err(|_| format!("无效版本号：{v}"))?;
    let patch = parts[2]
        .parse::<u64>()
        .map_err(|_| format!("无效版本号：{v}"))?;
    Ok((major, minor, patch))
}

pub fn version_gt(a: &str, b: &str) -> Result<bool, String> {
    Ok(parse_semver(a)? > parse_semver(b)?)
}

fn semver_gt(a: &str, b: &str) -> Result<bool, String> {
    version_gt(a, b)
}

pub fn current_app_version() -> String {
    // 由 build.rs 从 platform-versions/<os>.json 注入；Mac=macos.json，Windows=windows.json
    option_env!("WEBCLAW_APP_VERSION")
        .filter(|s| !s.is_empty())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_string()
}

pub fn host_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

pub fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        std::env::consts::ARCH
    }
}

pub fn normalize_base_url(base: &str) -> String {
    let u = base.trim().trim_end_matches('/');
    if u.is_empty() {
        DEFAULT_UPDATE_BASE_URL.trim_end_matches('/').to_string()
    } else {
        u.to_string()
    }
}

fn meta_file_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "latest-macos.json"
    } else {
        "latest.json"
    }
}

fn latest_meta_url(base: &str) -> String {
    format!("{}/{}", normalize_base_url(base), meta_file_name())
}

fn file_url(base: &str, file: &str) -> String {
    let name = file.trim().trim_start_matches('/');
    format!("{}/{name}", normalize_base_url(base))
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

pub async fn fetch_latest_info(base_url: &str) -> Result<LatestInfo, String> {
    let url = latest_meta_url(base_url);
    let client = http_client(20)?;
    let resp = client
        .get(&url)
        .header("Cache-Control", "no-cache")
        .send()
        .await
        .map_err(|e| format!("请求更新信息失败（{url}）: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("请求更新信息失败：HTTP {}", resp.status()));
    }
    resp.json::<LatestInfo>()
        .await
        .map_err(|e| format!("解析 {} 失败: {e}", meta_file_name()))
}

/// 设置页 / 帮助页使用的检查更新（可配置基址）
pub async fn check_update(
    current_version: &str,
    base_url: &str,
) -> Result<CheckUpdateResult, String> {
    let base = normalize_base_url(base_url);
    let latest = fetch_latest_info(&base).await?;
    let newer = version_gt(&latest.version, current_version)?;
    Ok(CheckUpdateResult {
        current_version: current_version.to_string(),
        update_available: newer,
        message: if newer {
            format!("发现新版本 {}", latest.version)
        } else {
            "已是最新版本".into()
        },
        latest: Some(latest),
        base_url: base,
    })
}

/// mac 设置页使用的检查更新（读取 settings 基址）
pub async fn check_for_update() -> Result<UpdateCheckResult, String> {
    let settings = crate::settings::load_settings().unwrap_or_default();
    let base = crate::settings::resolve_update_base_url(&settings);
    check_for_update_with_base(&base).await
}

pub async fn check_for_update_with_base(base_url: &str) -> Result<UpdateCheckResult, String> {
    let current = current_app_version();
    let base = normalize_base_url(base_url);
    let url = latest_meta_url(&base);
    let platform = host_platform().to_string();
    let arch = host_arch().to_string();

    let latest = fetch_latest_info(&base).await?;
    let available = semver_gt(&latest.version, &current)?;
    let download_url = Some(file_url(&base, &latest.file));
    let message = if available {
        format!("发现新版本 {}（当前 {}）", latest.version, current)
    } else {
        format!("已是最新版本 {}", current)
    };

    Ok(UpdateCheckResult {
        current_version: current,
        update_available: available,
        latest: Some(latest),
        download_url,
        meta_url: url,
        platform,
        arch,
        message,
    })
}

#[allow(dead_code)]
fn sha256_hex(path: &Path) -> Result<String, String> {
    let data = fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgressPayload {
    /// downloading | verifying | installing | done | error
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u32>,
    pub received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub filename: String,
}

fn emit_update_progress(app: &AppHandle, payload: UpdateProgressPayload) {
    let _ = app.emit("app-update-progress", &payload);
}

fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let x = n as f64;
    if x >= MB {
        format!("{:.1} MB", x / MB)
    } else if x >= KB {
        format!("{:.0} KB", x / KB)
    } else {
        format!("{n} B")
    }
}

/// 流式下载：边下边算 sha256，并按间隔推送进度；失败时删除半成品。
async fn download_to_file(
    app: Option<&AppHandle>,
    url: &str,
    dest: &Path,
    expected_size: Option<u64>,
    filename: &str,
) -> Result<(u64, String), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let client = http_client(600)?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载失败（{url}）: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败：HTTP {}", resp.status()));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.contains("text/html") || content_type.contains("text/plain") {
        return Err(format!(
            "下载地址返回了网页而非安装包（Content-Type: {content_type}），请检查更新基址"
        ));
    }

    let header_len = resp.content_length();
    if let (Some(cl), Some(exp)) = (header_len, expected_size) {
        if exp > 0 && cl > 0 && cl != exp {
            return Err(format!(
                "服务器声明大小与版本信息不符：HTTP Content-Length={cl}，latest.size={exp}"
            ));
        }
    }
    let total = expected_size.filter(|s| *s > 0).or(header_len);

    if let Some(app) = app {
        emit_update_progress(
            app,
            UpdateProgressPayload {
                phase: "downloading".into(),
                message: format!("开始下载 {filename}…"),
                percent: Some(0),
                received: 0,
                total,
                filename: filename.into(),
            },
        );
    }

    let mut stream = resp.bytes_stream();
    let mut file = fs::File::create(dest).map_err(|e| format!("写入临时文件失败: {e}"))?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut last_emit = Instant::now() - Duration::from_secs(1);

    let fail_cleanup = |err: String| -> String {
        let _ = fs::remove_file(dest);
        err
    };

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| fail_cleanup(format!("下载中断: {e}")))?;
        if chunk.is_empty() {
            continue;
        }
        file.write_all(&chunk)
            .map_err(|e| fail_cleanup(format!("写入临时文件失败: {e}")))?;
        hasher.update(&chunk);
        written += chunk.len() as u64;

        if let Some(exp) = expected_size {
            if exp > 0 && written > exp {
                return Err(fail_cleanup(format!(
                    "下载超出预期大小：已收 {}，期望 {}",
                    format_bytes(written),
                    format_bytes(exp)
                )));
            }
        }

        if let Some(app) = app {
            let due = last_emit.elapsed() >= Duration::from_millis(200)
                || total.map(|t| written >= t).unwrap_or(false);
            if due {
                let percent = total
                    .filter(|t| *t > 0)
                    .map(|t| ((written.min(t) as f64 / t as f64) * 100.0).round() as u32)
                    .map(|p| p.min(99));
                let msg = match total {
                    Some(t) if t > 0 => format!(
                        "正在下载… {} / {} ({})",
                        format_bytes(written),
                        format_bytes(t),
                        percent
                            .map(|p| format!("{p}%"))
                            .unwrap_or_else(|| "…".into())
                    ),
                    _ => format!("正在下载… {}", format_bytes(written)),
                };
                emit_update_progress(
                    app,
                    UpdateProgressPayload {
                        phase: "downloading".into(),
                        message: msg,
                        percent,
                        received: written,
                        total,
                        filename: filename.into(),
                    },
                );
                last_emit = Instant::now();
            }
        }
    }

    file.flush()
        .map_err(|e| fail_cleanup(format!("落盘失败: {e}")))?;
    drop(file);

    if let Some(exp) = expected_size {
        if exp > 0 && written != exp {
            return Err(fail_cleanup(format!(
                "下载大小不符：期望 {}，实际 {}",
                format_bytes(exp),
                format_bytes(written)
            )));
        }
    }
    if written == 0 {
        return Err(fail_cleanup("下载结果为空文件".into()));
    }

    let digest = format!("{:x}", hasher.finalize());
    Ok((written, digest))
}

fn update_staging_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("webclaw-update");
    fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    Ok(dir)
}

#[cfg(target_os = "macos")]
fn updates_cache_dir() -> PathBuf {
    crate::paths::app_root_dir().join("updates")
}

fn default_download_name(latest: &LatestInfo) -> String {
    let name = latest.file.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    if cfg!(target_os = "macos") {
        "webclaw.dmg".into()
    } else {
        "webclaw.exe".into()
    }
}

/// 下载并校验；返回临时包路径。随后调用 `spawn_replace_and_exit`。
pub async fn download_update(
    app: &AppHandle,
    base_url: &str,
    latest: &LatestInfo,
) -> Result<PathBuf, String> {
    let file_name = default_download_name(latest);
    let url = file_url(base_url, &file_name);
    let staging = update_staging_dir()?;
    let dest = staging.join(&file_name);
    let _ = fs::remove_file(&dest);

    let expected_sha = latest.sha256.trim();
    if expected_sha.is_empty() {
        return Err("更新元数据缺少 sha256，已拒绝下载（防止安装损坏包）".into());
    }

    emit_update_progress(
        app,
        UpdateProgressPayload {
            phase: "downloading".into(),
            message: format!("准备下载 {}…", latest.version),
            percent: Some(0),
            received: 0,
            total: latest.size.filter(|s| *s > 0),
            filename: file_name.clone(),
        },
    );

    let (written, digest) = match download_to_file(
        Some(app),
        &url,
        &dest,
        latest.size,
        &file_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            emit_update_progress(
                app,
                UpdateProgressPayload {
                    phase: "error".into(),
                    message: e.clone(),
                    percent: None,
                    received: 0,
                    total: latest.size,
                    filename: file_name.clone(),
                },
            );
            return Err(e);
        }
    };

    emit_update_progress(
        app,
        UpdateProgressPayload {
            phase: "verifying".into(),
            message: format!("正在校验文件完整性…（{}）", format_bytes(written)),
            percent: Some(99),
            received: written,
            total: Some(written),
            filename: file_name.clone(),
        },
    );

    if !digest.eq_ignore_ascii_case(expected_sha) {
        let _ = fs::remove_file(&dest);
        let msg = format!("sha256 校验失败：期望 {expected_sha}，实际 {digest}");
        emit_update_progress(
            app,
            UpdateProgressPayload {
                phase: "error".into(),
                message: msg.clone(),
                percent: None,
                received: written,
                total: latest.size,
                filename: file_name.clone(),
            },
        );
        return Err(msg);
    }

    #[cfg(windows)]
    {
        if let Err(e) = validate_windows_pe_exe(&dest) {
            emit_update_progress(
                app,
                UpdateProgressPayload {
                    phase: "error".into(),
                    message: e.clone(),
                    percent: None,
                    received: written,
                    total: latest.size,
                    filename: file_name.clone(),
                },
            );
            return Err(e);
        }
    }

    emit_update_progress(
        app,
        UpdateProgressPayload {
            phase: "installing".into(),
            message: "校验通过，正在安装并重启…".into(),
            percent: Some(100),
            received: written,
            total: Some(written),
            filename: file_name,
        },
    );

    Ok(dest)
}

/// 校验更新包为可运行的 Windows PE，且架构与当前进程一致。
/// 避免把损坏包/错误架构包覆盖进安装目录后，只看到系统「此应用无法在你的电脑上运行」。
#[cfg(windows)]
fn validate_windows_pe_exe(path: &Path) -> Result<(), String> {
    let data = fs::read(path).map_err(|e| format!("读取更新包失败: {e}"))?;
    if data.len() < 0x40 || data[0] != b'M' || data[1] != b'Z' {
        let _ = fs::remove_file(path);
        return Err("更新包不是有效的 Windows 程序（可能下载损坏或被拦截）".into());
    }
    let pe_off = u32::from_le_bytes(
        data.get(0x3C..0x40)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| "更新包 PE 头损坏".to_string())?,
    ) as usize;
    if pe_off + 6 > data.len() || &data[pe_off..pe_off + 4] != b"PE\0\0" {
        let _ = fs::remove_file(path);
        return Err("更新包 PE 头无效（文件可能不完整）".into());
    }
    let machine = u16::from_le_bytes(
        data.get(pe_off + 4..pe_off + 6)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| "更新包架构字段损坏".to_string())?,
    );
    // IMAGE_FILE_MACHINE_AMD64 / ARM64 / I386
    const AMD64: u16 = 0x8664;
    const ARM64: u16 = 0xAA64;
    const I386: u16 = 0x14C;
    let (need, need_label) = if cfg!(target_arch = "x86_64") {
        (AMD64, "x64")
    } else if cfg!(target_arch = "aarch64") {
        (ARM64, "ARM64")
    } else if cfg!(target_arch = "x86") {
        (I386, "x86")
    } else {
        return Ok(());
    };
    if machine != need {
        let got = match machine {
            AMD64 => "x64",
            ARM64 => "ARM64",
            I386 => "x86",
            _ => "未知",
        };
        let _ = fs::remove_file(path);
        return Err(format!(
            "更新包架构不匹配：需要 {need_label}，实际为 {got}（0x{machine:04X}）。请使用与电脑匹配的版本（设置 → 系统 → 关于 → 系统类型）。"
        ));
    }
    Ok(())
}

/// 下载并安装更新。成功后进程会退出并由安装脚本/新版本拉起。
pub async fn download_and_install(app: &AppHandle) -> Result<String, String> {
    let settings = crate::settings::load_settings().unwrap_or_default();
    let base = crate::settings::resolve_update_base_url(&settings);
    let check = check_for_update_with_base(&base).await?;
    if !check.update_available {
        return Ok(check.message);
    }
    let meta = check.latest.ok_or_else(|| "缺少版本元数据".to_string())?;
    let path = download_update(app, &base, &meta).await?;
    spawn_replace_and_exit(&path, app)?;
    Ok(format!("已开始安装 {}，程序即将重启", meta.version))
}

/// 按平台安装已下载的更新包并退出当前进程。
pub fn spawn_replace_and_exit(package: &Path, app: &AppHandle) -> Result<(), String> {
    // 先关掉 Debug Chrome，避免更新重启后孤儿进程反复抢前台
    prepare_chrome_shutdown_for_update(app);

    #[cfg(windows)]
    {
        return install_windows_exe(package, app);
    }
    #[cfg(target_os = "macos")]
    {
        return install_macos_dmg(package, app);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (package, app);
        Err("当前平台暂不支持自动安装更新，请手动下载安装".into())
    }
}

fn prepare_chrome_shutdown_for_update(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(state) = app.try_state::<crate::AppState>() {
        crate::chrome::shutdown_debug_chrome(&state.chrome_child);
    } else {
        crate::chrome::kill_listeners_on_port(crate::chrome::DEBUG_PORT);
    }
}

#[cfg(windows)]
fn strip_extended_path(p: &str) -> String {
    let t = p.trim();
    if let Some(rest) = t.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        t.to_string()
    }
}

#[cfg(windows)]
fn install_windows_exe(new_exe: &Path, app: &tauri::AppHandle) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // Tauri/WebView2 常把子进程放进 Job（KILL_ON_JOB_CLOSE）。
    // 仅 DETACHED 不够：父进程 exit 后更新脚本会被一并杀掉 → 只关程序、不替换、不重启。
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const DETACHED_PROCESS: u32 = 0x00000008;

    let current = std::env::current_exe().map_err(|e| format!("无法定位当前程序: {e}"))?;
    let pid = std::process::id();
    let staging = update_staging_dir()?;
    let script = staging.join("apply_update.bat");
    let helper = staging.join("launch_update.vbs");
    let log = staging.join("apply_update.log");

    let new_exe_s = strip_extended_path(&new_exe.to_string_lossy().replace('"', ""));
    let current_s = strip_extended_path(&current.to_string_lossy().replace('"', ""));
    let script_s = strip_extended_path(&script.to_string_lossy().replace('"', ""));
    let helper_s = strip_extended_path(&helper.to_string_lossy().replace('"', ""));
    let log_s = strip_extended_path(&log.to_string_lossy().replace('"', ""));

    if !Path::new(&new_exe_s).is_file() {
        return Err(format!("更新包不存在：{new_exe_s}"));
    }

    let body = format!(
        r#"@echo off
setlocal EnableExtensions EnableDelayedExpansion
set "PID={pid}"
set "NEW={new}"
set "CUR={cur}"
set "LOG={log}"
echo [%date% %time%] start pid=%PID% > "%LOG%"
echo NEW=%NEW%>> "%LOG%"
echo CUR=%CUR%>> "%LOG%"
:wait
tasklist /FI "PID eq %PID%" 2>NUL | find "%PID%" >NUL
if not errorlevel 1 (
  ping -n 2 127.0.0.1 >NUL
  goto wait
)
echo [%date% %time%] process exited>> "%LOG%"
ping -n 2 127.0.0.1 >NUL
set "OK=0"
for /L %%i in (1,1,30) do (
  copy /Y "%NEW%" "%CUR%" >> "%LOG%" 2>&1
  if not errorlevel 1 (
    set "OK=1"
    goto copied
  )
  echo [%date% %time%] copy retry %%i>> "%LOG%"
  ping -n 2 127.0.0.1 >NUL
)
:copied
if "!OK!"=="1" (
  echo [%date% %time%] copy ok>> "%LOG%"
  start "" "%CUR%"
) else (
  echo [%date% %time%] copy failed, launch NEW from staging>> "%LOG%"
  start "" "%NEW%"
)
ping -n 3 127.0.0.1 >NUL
if "!OK!"=="1" del /F /Q "%NEW%" >NUL 2>&1
del /F /Q "%~f0" >NUL 2>&1
"#,
        pid = pid,
        new = new_exe_s,
        cur = current_s,
        log = log_s,
    );
    fs::write(&script, body).map_err(|e| format!("写入更新脚本失败: {e}"))?;

    // WScript 二次脱离，Job 禁止 breakaway 时仍可能活下来
    let vbs = format!(
        "Set sh = CreateObject(\"WScript.Shell\")\r\nsh.Run \"cmd.exe /D /C call \"\"{script}\"\"\", 0, False\r\n",
        script = script_s,
    );
    fs::write(&helper, vbs).map_err(|e| format!("写入更新辅助脚本失败: {e}"))?;

    let flags =
        CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | DETACHED_PROCESS;
    let mut launched = Command::new("wscript.exe")
        .args(["//B", "//Nologo", &helper_s])
        .creation_flags(flags)
        .spawn()
        .is_ok();

    if !launched {
        launched = Command::new("cmd.exe")
            .args(["/D", "/C", &format!("call \"{script_s}\"")])
            .creation_flags(flags)
            .spawn()
            .is_ok();
    }

    if !launched {
        // 最后手段：允许弹窗，用 start 跳出 job
        Command::new("cmd.exe")
            .args(["/C", "start", "", "/MIN", &script_s])
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("启动更新脚本失败: {e}"))?;
    }

    // 给子进程一点时间真正 breakaway，再退出父进程
    std::thread::sleep(Duration::from_millis(800));
    app.exit(0);
    Ok(())
}

#[cfg(target_os = "macos")]
fn current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // .../webclaw.app/Contents/MacOS/webclaw
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?;
    if contents.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let app = contents.parent()?;
    if app.extension()?.to_str()? != "app" {
        return None;
    }
    Some(app.to_path_buf())
}

#[cfg(target_os = "macos")]
fn install_macos_dmg(dmg: &Path, app: &tauri::AppHandle) -> Result<(), String> {
    use std::process::Command;

    let target_app = current_app_bundle()
        .unwrap_or_else(|| PathBuf::from("/Applications/webclaw.app"));
    let target_str = target_app.to_string_lossy().to_string();
    let dmg_str = dmg.to_string_lossy().to_string();

    let script = format!(
        r#"#!/bin/bash
set -euo pipefail
DMG={dmg_q}
TARGET={target_q}
MOUNT="$(mktemp -d /tmp/webclaw-upd-XXXXXX)"
cleanup() {{
  hdiutil detach "$MOUNT" -quiet -force 2>/dev/null || true
  rmdir "$MOUNT" 2>/dev/null || true
}}
trap cleanup EXIT
hdiutil attach "$DMG" -nobrowse -quiet -mountpoint "$MOUNT"
SRC="$(find "$MOUNT" -maxdepth 2 -name 'webclaw.app' -type d | head -1)"
if [[ -z "$SRC" ]]; then
  echo "DMG 内未找到 webclaw.app" >&2
  exit 1
fi
sleep 1
rm -rf "$TARGET"
mkdir -p "$(dirname "$TARGET")"
ditto "$SRC" "$TARGET"
codesign --force --deep --sign - "$TARGET" >/dev/null 2>&1 || true
cleanup
trap - EXIT
open "$TARGET"
"#,
        dmg_q = shell_quote(&dmg_str),
        target_q = shell_quote(&target_str),
    );

    let cache = updates_cache_dir();
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let script_path = cache.join("install_macos.sh");
    fs::write(&script_path, script).map_err(|e| e.to_string())?;
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).map_err(|e| e.to_string())?;
    }

    Command::new("bash")
        .arg(&script_path)
        .spawn()
        .map_err(|e| format!("启动安装脚本失败：{e}"))?;

    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        handle.exit(0);
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compare() {
        assert!(version_gt("0.1.1", "0.1.0").unwrap());
        assert!(!version_gt("0.1.0", "0.1.1").unwrap());
        assert!(!version_gt("0.1.0", "0.1.0").unwrap());
        assert!(semver_gt("0.1.11", "0.1.4").unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn reject_non_pe_update_package() {
        let dir = std::env::temp_dir().join(format!("webclaw-pe-check-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let p = dir.join("bad.exe");
        fs::write(&p, b"not an executable").unwrap();
        let err = validate_windows_pe_exe(&p).unwrap_err();
        assert!(err.contains("不是有效的 Windows 程序"));
        let _ = fs::remove_dir_all(&dir);
    }
}
