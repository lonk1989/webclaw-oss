//! 伙伴空间按需 host：连中继收一次 `start_job`，跑完后退出；浏览器保持，供继续任务衔接。
//! 不再常驻；每次远程任务由伙伴兑换任务码后短命拉起。关闭浏览器请用 `host stop`。

use crate::remote_job::{self, HostPairFile};
use crate::run_relay;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// 上线后等待桌面下发 `start_job` 的最长时间（秒）。
const IDLE_WAIT_SECS: u64 = 180;

fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("WEBCLAW_HOME") {
        let p = PathBuf::from(h.trim());
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    crate::paths::app_data_dir()
}

fn default_pair_path() -> PathBuf {
    home_dir().join("host-pair.json")
}

fn jobs_dir() -> PathBuf {
    let d = home_dir().join("jobs");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn cdp_port() -> u16 {
    std::env::var("WEBCLAW_CDP_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(19222)
}

fn load_pair(path: Option<&Path>) -> Result<HostPairFile, String> {
    let p = path
        .map(|x| x.to_path_buf())
        .unwrap_or_else(default_pair_path);
    if !p.is_file() {
        return Err(format!(
            "找不到宿主机配对文件（{}）。请用 webclaw-worker host-pair-install --file … 安装，或 host --pair …",
            p.display()
        ));
    }
    remote_job::read_host_pair(&p)
}

fn apply_human_reply(msg: &Value) {
    let dir = match std::env::var("WEBCLAW_HUMAN_DIR") {
        Ok(d) if !d.trim().is_empty() => PathBuf::from(d),
        _ => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Some(text) = msg.get("text").and_then(|v| v.as_str()) {
        let t = text.trim();
        if !t.is_empty() {
            let _ = std::fs::write(dir.join("human_reply.txt"), t);
            return;
        }
    }
    if msg.get("cancel").and_then(|v| v.as_bool()) == Some(true) {
        let _ = std::fs::write(dir.join("human_reply.json"), r#"{"cancel":true}"#);
        return;
    }
    if msg.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        let _ = std::fs::write(dir.join("human_reply.json"), r#"{"ok":true}"#);
    }
}

/// 强制结束 19222（或 WEBCLAW_CDP_PORT）上的浏览器。仅 `host stop` / 取消时调用，任务结束要保留以便续跑。
pub fn cleanup_cdp_browser() {
    let port = cdp_port();
    crate::task_run::cancel_current_job();
    crate::browser_network::drop_port_capture(port);
    crate::chrome::kill_listeners_on_port(port);
    eprintln!("[host] cleaned CDP port {port}");
}

fn clear_ready_markers() {
    let home = home_dir();
    let _ = std::fs::remove_file(home.join("HOST_READY"));
    let _ = std::fs::remove_file(home.join("READY"));
    let _ = std::fs::remove_file(home.join("host.pid"));
}

/// 查找正在跑的 host 进程 PID（排除 `--daemon` 短命父进程）。
pub fn find_host_pids() -> Vec<u32> {
    let mut markers: Vec<String> = vec![
        "webclaw-worker".into(),
        "webclaw-worker.bin".into(),
    ];
    if let Ok(exe) = std::env::current_exe() {
        markers.push(exe.to_string_lossy().into_owned());
        if let Some(name) = exe.file_name() {
            markers.push(name.to_string_lossy().into_owned());
        }
    }
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == std::process::id() {
            continue;
        }
        let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        let s = String::from_utf8_lossy(&cmd).replace('\0', " ");
        if !markers.iter().any(|m| s.contains(m)) {
            continue;
        }
        if !s.split_whitespace().any(|a| a == "host") {
            continue;
        }
        if s.split_whitespace().any(|a| a == "--daemon") {
            continue;
        }
        if s.split_whitespace().any(|a| a == "stop" || a == "status") {
            continue;
        }
        out.push(pid);
    }
    out
}

fn host_process_alive() -> bool {
    !find_host_pids().is_empty()
}

fn cdp_port_in_use(port: u16) -> bool {
    use std::net::SocketAddr;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// 服务状态（给伙伴「查看服务状态」用）。
pub fn service_status_json() -> Value {
    let home = home_dir();
    let worker = std::env::current_exe().ok();
    let worker_ok = worker
        .as_ref()
        .map(|p| p.is_file())
        .unwrap_or(false);
    let pair_ok = default_pair_path().is_file();
    let pids = find_host_pids();
    let host_alive = !pids.is_empty();
    let log = home.join("host.log");
    let relay_ok = host_alive
        && log.is_file()
        && std::fs::read_to_string(&log)
            .map(|t| t.contains("[host] online"))
            .unwrap_or(false);
    let port = cdp_port();
    let browser_up = cdp_port_in_use(port);
    let phase = if !worker_ok {
        "no_worker"
    } else if !pair_ok {
        "need_pair"
    } else if host_alive && relay_ok {
        "running"
    } else if host_alive {
        "starting"
    } else {
        "idle"
    };
    json!({
        "type": "service_status",
        "phase": phase,
        "workerOk": worker_ok,
        "pairOk": pair_ok,
        "hostAlive": host_alive,
        "relayOk": relay_ok,
        "hostPids": pids,
        "cdpPort": port,
        "browserUp": browser_up,
        "home": home.display().to_string(),
        "hint": match phase {
            "no_worker" => "办事程序未安装",
            "need_pair" => "还缺配对，请兑换本机弹出的任务码",
            "running" => "本轮服务在线，正在或可接远程任务",
            "starting" => "服务正在连接中继",
            _ => "空闲（未常驻；远程执行时再拉起）",
        },
    })
}

/// 停止当前 host + 关闭 CDP 浏览器。
pub fn stop_host_service() -> Value {
    let pids = find_host_pids();
    let mut killed = Vec::new();
    for pid in &pids {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
        killed.push(*pid);
    }
    if !pids.is_empty() {
        std::thread::sleep(Duration::from_millis(400));
        for pid in &pids {
            let alive = Path::new(&format!("/proc/{pid}")).exists();
            if alive {
                let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
            }
        }
    }
    cleanup_cdp_browser();
    clear_ready_markers();
    let still = find_host_pids();
    json!({
        "type": "service_stopped",
        "killedPids": killed,
        "stillAlive": still,
        "browserCleaned": true,
        "cdpPort": cdp_port(),
    })
}

/// 最近一次执行相关日志摘要。
pub fn last_log_text(max_lines: usize) -> String {
    let home = home_dir();
    let mut parts: Vec<String> = Vec::new();

    let host_log = home.join("host.log");
    if host_log.is_file() {
        parts.push(format!("=== host.log ({}) ===", host_log.display()));
        parts.push(tail_file(&host_log, max_lines));
    } else {
        parts.push("=== host.log ===\n(无)".into());
    }

    let current = home.join("current.jsonl");
    if current.is_file() {
        parts.push(format!("\n=== current.jsonl ==="));
        parts.push(tail_file(&current, max_lines));
    }

    let runs = home.join("runs");
    if let Ok(mut ents) = std::fs::read_dir(&runs) {
        let mut dirs: Vec<_> = ents
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort_by(|a, b| {
            let ma = a.metadata().and_then(|m| m.modified()).ok();
            let mb = b.metadata().and_then(|m| m.modified()).ok();
            mb.cmp(&ma)
        });
        if let Some(latest) = dirs.first() {
            parts.push(format!("\n=== latest run: {} ===", latest.display()));
            for name in ["run-log.txt", "events.jsonl", "summary.txt"] {
                let p = latest.join(name);
                if p.is_file() {
                    parts.push(format!("--- {name} ---"));
                    parts.push(tail_file(&p, max_lines.min(80)));
                }
            }
        }
    }

    if parts.len() <= 1 {
        parts.push("(尚无执行日志)".into());
    }
    parts.join("\n")
}

fn tail_file(path: &Path, max_lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return "(读失败)".into();
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text;
    }
    lines[lines.len() - max_lines..].join("\n")
}

fn is_lite_product() -> bool {
    std::env::var_os("WEBCLAW_LITE").is_some()
}

fn download_url_to_file(url: &str, dest: &Path) -> Result<(), String> {
    // 优先 reqwest（Windows 无 curl 也能更新）；失败再试 curl
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP 客户端失败: {e}"))?;
    match client.get(url).send() {
        Ok(resp) => {
            if !resp.status().is_success() {
                return Err(format!("下载失败 HTTP {}", resp.status()));
            }
            let bytes = resp.bytes().map_err(|e| format!("读下载体失败: {e}"))?;
            std::fs::write(dest, &bytes).map_err(|e| format!("写下载文件失败: {e}"))?;
            return Ok(());
        }
        Err(e) => {
            eprintln!("[update] reqwest 失败，回退 curl: {e}");
        }
    }
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "30",
            "-o",
            dest.to_str().ok_or("下载路径无效")?,
            url,
        ])
        .status()
        .map_err(|e| format!("curl 失败: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(dest);
        return Err("下载失败".into());
    }
    Ok(())
}

/// 重新下载 worker / lite。Unix：`.bin` + shell loader；Windows lite：直接替换 `.exe`。
pub fn self_update() -> Result<Value, String> {
    let lite = is_lite_product();
    let home = home_dir();
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;

    let url = if let Ok(u) = std::env::var("WEBCLAW_WORKER_URL") {
        let t = u.trim().to_string();
        if t.is_empty() {
            default_update_url(lite)
        } else {
            t
        }
    } else {
        default_update_url(lite)
    };

    if lite && cfg!(windows) {
        return self_update_windows_lite(&bin_dir, &url);
    }

    let stem = if lite { "webclaw-lite" } else { "webclaw-worker" };
    let loader = bin_dir.join(stem);
    let real = bin_dir.join(format!("{stem}.bin"));
    let tmp = bin_dir.join(format!("{stem}.download.{}", std::process::id()));
    download_url_to_file(&url, &tmp)?;
    chmod_755(&tmp)?;
    let _ = stop_host_service();

    migrate_legacy_worker_layout(&loader, &real)?;
    replace_file_atomic(&tmp, &real)?;
    if lite {
        ensure_product_loader(&loader, &format!("{stem}.bin"))?;
    } else {
        ensure_worker_loader(&loader)?;
    }

    let mut persistent = None;
    if let Ok(user_home) = std::env::var("HOME") {
        let pdir = PathBuf::from(user_home).join(".webclaw").join("bin");
        if std::fs::create_dir_all(&pdir).is_ok() {
            let p_real = pdir.join(format!("{stem}.bin"));
            let p_loader = pdir.join(stem);
            if std::fs::copy(&real, &p_real).is_ok() {
                let _ = chmod_755(&p_real);
                let _ = if lite {
                    ensure_product_loader(&p_loader, &format!("{stem}.bin"))
                } else {
                    ensure_worker_loader(&p_loader)
                };
                persistent = Some(p_loader.display().to_string());
            }
        }
    }

    let help = Command::new(&loader)
        .arg("--help")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Ok(json!({
        "type": "updated",
        "product": if lite { "webclaw-lite" } else { "webclaw-worker" },
        "path": loader.display().to_string(),
        "binary": real.display().to_string(),
        "persistentPath": persistent,
        "url": url,
        "helpPreview": help,
    }))
}

fn default_update_url(lite: bool) -> String {
    if lite {
        crate::lite::default_download_url()
    } else {
        std::env::var("WEBCLAW_WORKER_URL").unwrap_or_default()
    }
}

fn self_update_windows_lite(bin_dir: &Path, url: &str) -> Result<Value, String> {
    let exe = bin_dir.join("webclaw-lite.exe");
    let tmp = bin_dir.join(format!("webclaw-lite.download.{}.exe", std::process::id()));
    download_url_to_file(url, &tmp)?;
    // 正在运行的自身：先写旁路，再尝试替换；失败则保留 .new 供下次启动
    if exe.is_file() {
        let bak = bin_dir.join("webclaw-lite.exe.bak");
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::rename(&exe, &bak);
    }
    match replace_file_atomic(&tmp, &exe) {
        Ok(()) => {}
        Err(_) => {
            let pending = bin_dir.join("webclaw-lite.exe.new");
            let _ = std::fs::rename(&tmp, &pending);
            return Ok(json!({
                "type": "updated",
                "product": "webclaw-lite",
                "path": exe.display().to_string(),
                "pending": pending.display().to_string(),
                "url": url,
                "note": "当前进程占用，已写入 .new；请退出后再次运行 install/update 或手动替换",
            }));
        }
    }
    let help = Command::new(&exe)
        .arg("--help")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Ok(json!({
        "type": "updated",
        "product": "webclaw-lite",
        "path": exe.display().to_string(),
        "binary": exe.display().to_string(),
        "url": url,
        "helpPreview": help,
    }))
}

fn ensure_product_loader(loader: &Path, bin_name: &str) -> Result<(), String> {
    let script = format!("#!/bin/sh\nexec \"$(dirname \"$0\")/{bin_name}\" \"$@\"\n");
    if loader.is_file() && is_shell_loader(loader) {
        let _ = chmod_755(loader);
        return Ok(());
    }
    let tmp = loader.with_extension("loader.tmp");
    std::fs::write(&tmp, script).map_err(|e| format!("写 loader 失败: {e}"))?;
    chmod_755(&tmp)?;
    replace_file_atomic(&tmp, loader)?;
    Ok(())
}

fn chmod_755(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
    }
    let _ = path;
    Ok(())
}

fn is_shell_loader(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    bytes.starts_with(b"#!")
}

fn migrate_legacy_worker_layout(loader: &Path, real: &Path) -> Result<(), String> {
    if real.is_file() {
        return Ok(());
    }
    if loader.is_file() && !is_shell_loader(loader) {
        // 旧版：webclaw-worker 即 ELF → 挪成 .bin
        replace_file_atomic(loader, real)?;
    }
    Ok(())
}

fn ensure_worker_loader(loader: &Path) -> Result<(), String> {
    let script = "#!/bin/sh\nexec \"$(dirname \"$0\")/webclaw-worker.bin\" \"$@\"\n";
    if loader.is_file() && is_shell_loader(loader) {
        let _ = chmod_755(loader);
        return Ok(());
    }
    let tmp = loader.with_extension("loader.tmp");
    std::fs::write(&tmp, script).map_err(|e| format!("写 loader 失败: {e}"))?;
    chmod_755(&tmp)?;
    replace_file_atomic(&tmp, loader)?;
    Ok(())
}

/// 同目录原子替换：先把目标挪走再 rename 新文件，避免 ETXTBSY；跨挂载则 copy。
fn replace_file_atomic(src: &Path, dest: &Path) -> Result<(), String> {
    let bak = dest.with_extension(format!(
        "old.{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&bak);
    if dest.exists() {
        if let Err(e) = std::fs::rename(dest, &bak) {
            // 跨设备时 rename 目标也可能失败：改为删（或 copy 备份）
            let _ = std::fs::copy(dest, &bak);
            let _ = std::fs::remove_file(dest);
            if dest.exists() {
                return Err(format!("无法挪开旧文件 {}: {e}", dest.display()));
            }
        }
    }
    match std::fs::rename(src, dest) {
        Ok(()) => {
            let _ = std::fs::remove_file(&bak);
            chmod_755(dest)?;
            Ok(())
        }
        Err(e) => {
            // EXDEV 等：copy 到目标
            let copy_err = std::fs::copy(src, dest).err();
            let _ = std::fs::remove_file(src);
            if copy_err.is_none() {
                let _ = std::fs::remove_file(&bak);
                chmod_755(dest)?;
                return Ok(());
            }
            // 恢复备份
            if bak.exists() {
                let _ = std::fs::rename(&bak, dest);
            }
            Err(format!(
                "替换失败 {} → {}: {e}",
                src.display(),
                dest.display()
            ))
        }
    }
}

enum SessionEnd {
    JobFinished { job_id: String },
    IdleTimeout,
    Cancelled,
}

/// 单次会话：上线 → 最多接一单 → 清理浏览器 → 返回（由调用方退出进程）。
pub async fn run_host_loop(pair_path: Option<PathBuf>) -> Result<(), String> {
    let pair = load_pair(pair_path.as_deref())?;
    let url = run_relay::normalize_ws_url(&pair.relay_ws_url);
    eprintln!("[host] connecting {url} owner={} (one-shot)", pair.owner_client_id);

    let busy = Arc::new(AtomicBool::new(false));
    let (ws, _) = connect_async(&url)
        .await
        .map_err(|e| format!("连接运行中继失败 ({url}): {e}"))?;
    let (write, mut read) = ws.split();
    let write = Arc::new(Mutex::new(write));

    {
        let mut w = write.lock().await;
        w.send(Message::Text(
            json!({
                "type": "host_hello",
                "ownerClientId": pair.owner_client_id,
                "hostToken": pair.host_token,
            })
            .to_string(),
        ))
        .await
        .map_err(|e| format!("host_hello 失败: {e}"))?;
    }

    let ready_marker = home_dir().join("HOST_READY");
    let (end_tx, mut end_rx) = mpsc::unbounded_channel::<SessionEnd>();
    let mut online = false;
    let mut idle_deadline = None::<tokio::time::Instant>;
    let mut ping = tokio::time::interval(Duration::from_secs(25));

    let end_reason = loop {
        tokio::select! {
            _ = ping.tick() => {
                let mut w = write.lock().await;
                let _ = w.send(Message::Text(json!({ "type": "ping" }).to_string())).await;
            }
            end = end_rx.recv() => {
                if let Some(e) = end {
                    break e;
                }
            }
            _ = async {
                if let Some(d) = idle_deadline {
                    tokio::time::sleep_until(d).await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if idle_deadline.is_some() && !busy.load(Ordering::SeqCst) => {
                break SessionEnd::IdleTimeout;
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        let v: Value = match serde_json::from_str(&t) {
                            Ok(x) => x,
                            Err(_) => continue,
                        };
                        match v.get("type").and_then(|x| x.as_str()) {
                            Some("host_hello_ok") => {
                                eprintln!("[host] online ownerOnline={}",
                                    v.get("ownerOnline").and_then(|x| x.as_bool()).unwrap_or(false));
                                let _ = std::fs::write(
                                    &ready_marker,
                                    format!("{}\n", chrono::Utc::now().to_rfc3339()),
                                );
                                online = true;
                                idle_deadline = Some(
                                    tokio::time::Instant::now()
                                        + Duration::from_secs(IDLE_WAIT_SECS),
                                );
                            }
                            Some("start_job") => {
                                if !online {
                                    eprintln!("[host] ignore start_job before hello_ok");
                                    continue;
                                }
                                if busy.swap(true, Ordering::SeqCst) {
                                    eprintln!("[host] busy, ignore start_job");
                                    continue;
                                }
                                idle_deadline = None;
                                let job_id = v.get("jobId").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let run_token = v.get("runToken").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("远程任务").to_string();
                                let b64 = v.get("jobBase64").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let write2 = write.clone();
                                let busy2 = busy.clone();
                                let end_tx2 = end_tx.clone();
                                let job_id2 = job_id.clone();
                                tokio::spawn(async move {
                                    let result = handle_start_job(&job_id, &run_token, &title, &b64).await;
                                    if let Err(e) = &result {
                                        eprintln!("[host] job failed: {e}");
                                    }
                                    busy2.store(false, Ordering::SeqCst);
                                    let mut w = write2.lock().await;
                                    let _ = w.send(Message::Text(
                                        json!({ "type": "host_idle", "jobId": job_id }).to_string()
                                    )).await;
                                    let _ = end_tx2.send(SessionEnd::JobFinished { job_id: job_id2 });
                                });
                            }
                            Some("human_reply") => apply_human_reply(&v),
                            Some("cancel_job") => {
                                eprintln!("[host] cancel_job");
                                apply_human_reply(&json!({ "cancel": true }));
                                crate::task_run::cancel_current_job();
                                if !busy.load(Ordering::SeqCst) {
                                    break SessionEnd::Cancelled;
                                }
                                // 任务中：等 job 任务发 JobFinished
                            }
                            Some("error") => {
                                eprintln!(
                                    "[host] error: {}",
                                    v.get("message").and_then(|x| x.as_str()).unwrap_or("?")
                                );
                            }
                            Some("pong") => {}
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let mut w = write.lock().await;
                        let _ = w.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return Err("中继连接已关闭".into());
                    }
                    Some(Err(e)) => return Err(format!("中继读失败: {e}")),
                    _ => {}
                }
            }
        }
    };

    match &end_reason {
        SessionEnd::JobFinished { job_id } => {
            eprintln!("[host] job done id={job_id}; keep browser for continue");
        }
        SessionEnd::IdleTimeout => {
            eprintln!("[host] idle timeout {IDLE_WAIT_SECS}s; shutting down");
        }
        SessionEnd::Cancelled => {
            eprintln!("[host] cancelled before/without job; shutting down");
            cleanup_cdp_browser();
        }
    }

    clear_ready_markers();
    // 给中继一点时间把 host_idle 发出去
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}

/// 按需入口：跑完一单（或空闲超时）后进程退出，不重连。
pub async fn run_host_forever(pair_path: Option<PathBuf>) -> ! {
    let code = match run_host_loop(pair_path).await {
        Ok(()) => {
            eprintln!("[host] exit after one-shot session");
            0
        }
        Err(e) => {
            eprintln!("[host] session error: {e}");
            clear_ready_markers();
            1
        }
    };
    std::process::exit(code);
}

async fn handle_start_job(
    job_id: &str,
    _run_token: &str,
    title: &str,
    job_base64: &str,
) -> Result<(), String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(job_base64.trim())
        .map_err(|e| format!("jobBase64 解码失败: {e}"))?;
    if bytes.len() < 4 || &bytes[0..4] != b"WCJB" {
        return Err("任务包不是有效的 .webclawjob".into());
    }
    let name = format!(
        "remote-{}.webclawjob",
        &job_id[..8.min(job_id.len())]
    );
    let path = jobs_dir().join(name);
    std::fs::write(&path, &bytes).map_err(|e| format!("写任务包失败: {e}"))?;
    eprintln!("[host] start job title={title} path={}", path.display());
    crate::worker::run_job_file_for_host(path).await
}

/// 在独立会话中拉起短命 `host`。
///
/// **成功 = spawn/fork 已发出**，父进程立即返回；绝不 waitpid host。
/// 伙伴沙箱会等「命令进程树」：父进程若卡住或未 exit，即使兑换成功也会 30s timeout。
pub fn spawn_detached_host(pair_path: Option<&Path>) -> Result<u32, String> {
    if host_process_alive() {
        // 只杀旧 host，不在这里 cleanup 浏览器（免拖慢 pair-redeem）
        let pids = find_host_pids();
        for pid in &pids {
            let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
        }
        if !pids.is_empty() {
            std::thread::sleep(Duration::from_millis(200));
            for pid in &pids {
                if Path::new(&format!("/proc/{pid}")).exists() {
                    let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
                }
            }
        }
        clear_ready_markers();
    }
    let home = home_dir();
    let _ = std::fs::create_dir_all(&home);
    let log_path = home.join("host.log");
    {
        let _ = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .map_err(|e| format!("打开 host.log 失败: {e}"))?;
    }

    let exe = resolve_worker_exe()?;
    #[cfg(unix)]
    {
        return spawn_detached_host_unix(&exe, pair_path, &home, &log_path);
    }
    #[cfg(not(unix))]
    {
        spawn_detached_host_fallback(&exe, pair_path, &home, &log_path)
    }
}

/// 优先用 `.bin`（loader 布局），否则 current_exe / bin/webclaw-worker。
fn resolve_worker_exe() -> Result<PathBuf, String> {
    let home_bin = home_dir().join("bin").join("webclaw-worker.bin");
    if home_bin.is_file() {
        return Ok(home_bin);
    }
    let home_loader = home_dir().join("bin").join("webclaw-worker");
    if home_loader.is_file() && !is_shell_loader(&home_loader) {
        return Ok(home_loader);
    }
    std::env::current_exe().map_err(|e| format!("current_exe: {e}"))
}

#[cfg(unix)]
fn spawn_detached_host_unix(
    exe: &Path,
    pair_path: Option<&Path>,
    home: &Path,
    log_path: &Path,
) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;

    let exe_c = exe.to_path_buf();
    let pair_c = pair_path.map(|p| p.to_path_buf());
    let home_c = home.to_path_buf();
    let log_c = log_path.to_path_buf();

    // 自动收尸：父进程不再 waitpid，避免任何子进程状态把 pair-redeem 卡住
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_IGN);
    }

    let first = unsafe { libc::fork() };
    if first < 0 {
        return Err(format!("fork 失败: {}", std::io::Error::last_os_error()));
    }
    if first > 0 {
        // 父进程立刻返回（不等中间进程、更不等 host）
        let pid = first as u32;
        let _ = std::fs::write(home.join("host.pid"), format!("{pid}\n"));
        return Ok(pid);
    }

    // —— 子进程：setsid + 再 fork，脱离会话后再 exec host ——
    if unsafe { libc::setsid() } < 0 {
        unsafe { libc::_exit(1) };
    }
    let second = unsafe { libc::fork() };
    if second < 0 {
        unsafe { libc::_exit(1) };
    }
    if second > 0 {
        // 中间进程退出；真正 host 为孙进程，由 init 收养
        unsafe { libc::_exit(0) };
    }

    let log = match OpenOptions::new().create(true).append(true).open(&log_c) {
        Ok(f) => f,
        Err(_) => unsafe { libc::_exit(2) },
    };
    let log_err = match log.try_clone() {
        Ok(f) => f,
        Err(_) => unsafe { libc::_exit(2) },
    };

    let mut cmd = Command::new(&exe_c);
    cmd.arg("host");
    if let Some(p) = pair_c.as_ref() {
        cmd.arg("--pair").arg(p);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .env("WEBCLAW_HOME", home_c.as_os_str())
        .env_remove("WEBCLAW_WORKER_DAEMONIZING");
    for key in [
        "WEBCLAW_CHROME",
        "WEBCLAW_CDP_PORT",
        "WEBCLAW_CONTAINER",
        "AILY_WORKDIR",
        "WEBCLAW_RUN_RELAY_WS",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    let err = cmd.exec();
    let _ = err;
    unsafe { libc::_exit(127) };
}

#[cfg(not(unix))]
fn spawn_detached_host_fallback(
    exe: &Path,
    pair_path: Option<&Path>,
    home: &Path,
    log_path: &Path,
) -> Result<u32, String> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("打开 host.log 失败: {e}"))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("clone host.log 失败: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("host");
    if let Some(p) = pair_path {
        cmd.arg("--pair").arg(p);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .env("WEBCLAW_HOME", home.as_os_str());
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn host 失败: {e}"))?;
    let pid = child.id();
    // 不 wait：成功 = spawn 成功
    std::mem::forget(child);
    if pid > 0 {
        let _ = std::fs::write(home.join("host.pid"), format!("{pid}\n"));
    }
    Ok(pid)
}
