//! 本机轻客户端（webclaw-lite）CLI：Skill 约定的工具面。
//!
//! 契约命令：`install` / `status` / `run` / `human-reply` / `last-log` / `update`
//! 远程 host / pair-redeem 不在本机主路径（仍可由同内核 worker 二进制提供）。

use serde_json::json;
use std::path::{Path, PathBuf};

/// Optional static base for `update` / install downloads.
/// Set `WEBCLAW_LITE_BASE` (directory prefix) or `WEBCLAW_WORKER_URL` (full exe URL).
/// Empty base falls back to GitHub Releases `latest` for the current OS.
pub const DEFAULT_STATIC_BASE: &str = "";

const GITHUB_LATEST_DIR: &str =
    "https://github.com/lonk1989/webclaw-oss/releases/latest/download";

/// GitHub Releases "latest" asset URL for this host OS/arch.
pub fn default_github_latest_url() -> String {
    #[cfg(target_os = "windows")]
    {
        return format!("{GITHUB_LATEST_DIR}/webclaw-lite-windows-x64.exe");
    }
    #[cfg(target_os = "macos")]
    {
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        };
        return format!("{GITHUB_LATEST_DIR}/webclaw-lite-macos-{arch}");
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        format!("{GITHUB_LATEST_DIR}/webclaw-lite-linux-x64")
    }
}

/// Backward-compatible alias (Windows x64 latest URL).
pub const DEFAULT_GITHUB_LATEST_EXE: &str =
    "https://github.com/lonk1989/webclaw-oss/releases/latest/download/webclaw-lite-windows-x64.exe";

pub fn default_download_url() -> String {
    if let Ok(u) = std::env::var("WEBCLAW_WORKER_URL") {
        let t = u.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let base = std::env::var("WEBCLAW_LITE_BASE")
        .unwrap_or_else(|_| DEFAULT_STATIC_BASE.to_string());
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return default_github_latest_url();
    }
    // Prefix is a release/static directory (GitHub download dir or mirror).
    // Asset sits directly under the prefix — not under a nested `/release/` folder.
    #[cfg(target_os = "windows")]
    {
        return format!("{base}/webclaw-lite-windows-x64.exe");
    }
    #[cfg(target_os = "macos")]
    {
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        };
        return format!("{base}/webclaw-lite-macos-{arch}");
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        format!("{base}/webclaw-lite-linux-x64")
    }
}

pub fn print_usage() {
    eprintln!(
        "webclaw-lite — 本机轻客户端 RPA（Skill/CLI）

用法:
  webclaw-lite install
  webclaw-lite status [--run-id <id>]
  webclaw-lite probe [--url URL] [--types 输入框,按钮,图标按钮,链接] [--search 关键词] [--limit 60]
  webclaw-lite recipe save --name <stem> (--stdin | --file <path>) [--title 标题]
  webclaw-lite recipe list
  webclaw-lite run --recipe <file.webclaw|txt|name> [--title 名称] [--params JSON]
  webclaw-lite run --stdin [--title 名称] [--params JSON]
  webclaw-lite human-reply (--text CODE | --ok | --cancel) [--run-dir <run>]
  webclaw-lite cancel
  webclaw-lite last-log [--lines N]
  webclaw-lite update
  webclaw-lite skills sync --from-dir <目录>
  webclaw-lite skills list
  webclaw-lite accounts list
  webclaw-lite accounts ensure --id ehr|old-mes [--name 名称] --url URL
  webclaw-lite accounts set --id ehr|old-mes [--name 名称] [--url URL] --user 账号 --password 密码

说明:
  由 AI 助手按 SKILL.md 调用本 CLI；禁止用 Playwright 等重写步骤。
  编排期：probe 观察当前页 → 用返回指令拼短菜谱 → run 片段 → 再 probe（勿杀浏览器）。
  冻结：recipe save 写入 <WEBCLAW_HOME>/recipes/<name>.webclaw（DSH 优先写工作区 recipes/）。
  复跑期：只 run 完整菜谱（可用路径或 recipes 下的短名）；不要逐步 probe。
  执行 stdout 为 JSONL（run_start / step_* / need_human / done）；probe/recipe 为单条 JSON。
  遇验证码：读 need_human 事件 → 向用户要码 → human-reply --text … → 继续等 JSONL。
  正在跑的任务卡住：另开进程执行 cancel（不要用 human-reply --cancel 代替）。
  产物在 <WEBCLAW_HOME>/runs/<runId>/downloads|documents。

环境变量:
  WEBCLAW_HOME          数据根（默认 %LOCALAPPDATA%\\\\webclaw 或 ~/.webclaw）
  WEBCLAW_CHROME        Chrome/Edge 可执行文件
  WEBCLAW_CDP_PORT      CDP 端口（默认 19222）
  WEBCLAW_HEADLESS      设为 1 时无头启动 Chrome（默认弹出可见窗口）
  WEBCLAW_HUMAN_DIR     人机交换目录（默认 <run>/human）
  WEBCLAW_WORKER_URL    update 下载地址（完整 URL）
  WEBCLAW_LITE_BASE     静态站/Release 目录前缀（拼平台资产名；
                        未设置时默认 GitHub Releases latest）
"
    );
}

/// 本机数据根与浏览器探测（不强制容器参数）。
pub fn ensure_lite_env() {
    std::env::set_var("WEBCLAW_WORKER", "1");
    std::env::set_var("WEBCLAW_LITE", "1");
    // Multi-round authoring: probe ↔ short run share the same CDP browser.
    if std::env::var_os("WEBCLAW_RESUME_BROWSER").is_none() {
        std::env::set_var("WEBCLAW_RESUME_BROWSER", "1");
    }
    // 本机勿默认 --no-sandbox
    if std::env::var_os("WEBCLAW_CDP_PORT").is_none() {
        std::env::set_var("WEBCLAW_CDP_PORT", "19222");
    }
    if std::env::var_os("WEBCLAW_HOME").is_none() {
        let dir = default_home_dir();
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("WEBCLAW_HOME", &dir);
    }
    if std::env::var_os("WEBCLAW_CHROME").is_none() {
        if let Some(p) = crate::chrome::find_chrome_exe() {
            std::env::set_var("WEBCLAW_CHROME", p);
        }
    }
    if std::env::var_os("WEBCLAW_WORKER_URL").is_none() {
        std::env::set_var("WEBCLAW_WORKER_URL", default_download_url());
    }
    // 确保常用子目录
    if let Ok(home) = std::env::var("WEBCLAW_HOME") {
        let home = PathBuf::from(home);
        for sub in ["bin", "runs", "recipes", "jobs"] {
            let _ = std::fs::create_dir_all(home.join(sub));
        }
    }
}

fn default_home_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        return crate::paths::app_root_dir();
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = dirs::home_dir() {
            return home.join(".webclaw");
        }
        PathBuf::from(".webclaw")
    }
}

fn product_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn resolve_human_dir(run_dir: Option<&Path>) -> Result<PathBuf, String> {
    if let Ok(raw) = std::env::var("WEBCLAW_HUMAN_DIR") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    if let Some(rd) = run_dir {
        return Ok(rd.join("human"));
    }
    // 从全局收件箱 / last-need-human 推断
    let data = crate::paths::app_data_dir();
    for name in ["need_human.json", "last-need-human.json"] {
        let p = data.join(name);
        if p.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&p) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(rf) = v.get("replyFile").or_else(|| v.get("reply_file")) {
                        if let Some(s) = rf.as_str() {
                            if let Some(parent) = Path::new(s).parent() {
                                return Ok(parent.to_path_buf());
                            }
                        }
                    }
                    if let Some(img) = v.get("imagePath").or_else(|| v.get("image_path")) {
                        if let Some(s) = img.as_str() {
                            if let Some(parent) = Path::new(s).parent() {
                                return Ok(parent.to_path_buf());
                            }
                        }
                    }
                }
            }
        }
    }
    // 最近一次含 human/waiting_human.json 的 run
    let runs = crate::paths::runs_dir();
    if let Ok(rd) = std::fs::read_dir(&runs) {
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
        for p in dirs.into_iter().rev() {
            let waiting = p.join("human").join("waiting_human.json");
            if waiting.is_file() {
                return Ok(p.join("human"));
            }
        }
    }
    Err("未找到人机目录：请传 --run-dir，或确认任务正在等待验证码".into())
}

fn install_cli() -> Result<(), String> {
    ensure_lite_env();
    let home = PathBuf::from(std::env::var("WEBCLAW_HOME").map_err(|_| "WEBCLAW_HOME 未设置")?);
    let chrome = std::env::var("WEBCLAW_CHROME").unwrap_or_default();
    let bin_hint = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let marker = home.join("lite-installed.json");
    let payload = json!({
        "type": "installed",
        "product": "webclaw-lite",
        "version": product_version(),
        "home": home.display().to_string(),
        "chrome": if chrome.is_empty() { serde_json::Value::Null } else { json!(chrome) },
        "bin": bin_hint,
        "cdpPort": std::env::var("WEBCLAW_CDP_PORT").unwrap_or_else(|_| "19222".into()),
        "updateUrl": default_download_url(),
    });
    std::fs::write(
        &marker,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    )
    .map_err(|e| format!("写安装标记失败: {e}"))?;
    println!("{payload}");
    if chrome.is_empty() {
        eprintln!("警告: 未探测到 Chrome/Edge，请安装浏览器或设置 WEBCLAW_CHROME");
    }
    Ok(())
}

fn cancel_flag_path() -> Result<PathBuf, String> {
    let home = std::env::var("WEBCLAW_HOME").map_err(|_| "WEBCLAW_HOME 未设置")?;
    Ok(PathBuf::from(home).join("control").join("cancel"))
}

fn request_cancel() -> Result<PathBuf, String> {
    ensure_lite_env();
    let flag = cancel_flag_path()?;
    if let Some(parent) = flag.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 control 目录失败: {e}"))?;
    }
    std::fs::write(&flag, b"1").map_err(|e| format!("写取消标记失败: {e}"))?;
    Ok(flag)
}

fn human_reply_cli(args: &[String]) -> Result<(), String> {
    let mut run_dir = None;
    let mut text = None;
    let mut ok = false;
    let mut cancel = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--run-dir" => {
                i += 1;
                run_dir = Some(PathBuf::from(
                    args.get(i).ok_or("--run-dir 需要路径")?,
                ));
            }
            "--text" => {
                i += 1;
                text = Some(args.get(i).ok_or("--text 需要验证码")?.clone());
            }
            "--ok" => ok = true,
            "--cancel" => cancel = true,
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }

    if cancel {
        let flag = request_cancel()?;
        if let Ok(human) = resolve_human_dir(run_dir.as_deref()) {
            let _ = std::fs::create_dir_all(&human);
            let _ = std::fs::write(human.join("human_reply.json"), r#"{"cancel":true}"#);
        }
        println!(
            "{}",
            json!({
                "type": "cancel_requested",
                "path": flag.display().to_string(),
            })
        );
        return Ok(());
    }
    let human = resolve_human_dir(run_dir.as_deref())?;
    std::fs::create_dir_all(&human).map_err(|e| format!("创建人机目录失败: {e}"))?;
    if ok {
        std::fs::write(human.join("human_reply.json"), r#"{"ok":true}"#)
            .map_err(|e| e.to_string())?;
        println!(
            "{}",
            json!({
                "type": "human_reply_written",
                "kind": "ok",
                "dir": human.display().to_string(),
            })
        );
        return Ok(());
    }
    let text = text.ok_or("需要 --text CODE 或 --ok 或 --cancel")?;
    std::fs::write(human.join("human_reply.txt"), format!("{}\n", text.trim()))
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({
            "type": "human_reply_written",
            "kind": "text",
            "dir": human.display().to_string(),
        })
    );
    Ok(())
}

fn lite_status(run_id: Option<&str>) -> Result<(), String> {
    ensure_lite_env();
    let home = std::env::var("WEBCLAW_HOME").unwrap_or_default();
    let chrome = std::env::var("WEBCLAW_CHROME").unwrap_or_default();
    let chrome_ok = !chrome.is_empty() && Path::new(&chrome).is_file();
    let bin = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    // 复用 worker status 核心字段
    crate::worker::status_run_for_lite(run_id.unwrap_or(""), |base| {
        let mut v = base;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("product".into(), json!("webclaw-lite"));
            obj.insert("version".into(), json!(product_version()));
            obj.insert("home".into(), json!(home));
            obj.insert(
                "chrome".into(),
                if chrome.is_empty() {
                    serde_json::Value::Null
                } else {
                    json!(chrome)
                },
            );
            obj.insert("chromeOk".into(), json!(chrome_ok));
            obj.insert("bin".into(), json!(bin));
            obj.insert("updateUrl".into(), json!(default_download_url()));
            obj.insert("skillProvider".into(), json!("local"));
            obj.insert("corpSignedIn".into(), json!(false));
            obj.insert("corpUser".into(), serde_json::Value::Null);
        }
        println!("{}", serde_json::to_string(&v).unwrap_or_default());
    })
}

/// 轻客户端入口。
pub fn run_cli() {
    ensure_lite_env();
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match cmd {
        "" | "-h" | "--help" | "help" => {
            print_usage();
            std::process::exit(0);
        }
        "install" => match install_cli() {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        "skills" => {
            let sub = args.get(2).map(|s| s.as_str()).unwrap_or("list");
            let code = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(async {
                    match sub {
                        "sync" => {
                            let from = crate::corp_skills::parse_sync_from_dir(&args);
                            let result = if let Some(dir) = from {
                                crate::corp_skills::sync_skills_from_dir(&dir).await
                            } else {
                                crate::corp_skills::sync_skills().await
                            };
                            match result {
                                Ok(n) => {
                                    println!("{}", json!({"type":"skills_synced","count": n}));
                                    0
                                }
                                Err(e) => {
                                    eprintln!("{e}");
                                    2
                                }
                            }
                        }
                        "list" => match crate::corp_skills::list_cached_names() {
                            Ok(items) => {
                                println!("{}", json!({"type":"skills","items": items}));
                                0
                            }
                            Err(e) => {
                                eprintln!("{e}");
                                2
                            }
                        },
                        other => {
                            eprintln!("未知子命令: {other}（可用 sync、list）");
                            2
                        }
                    }
                }),
                Err(e) => {
                    eprintln!("{e}");
                    2
                }
            };
            std::process::exit(code);
        }
        "cancel" => match request_cancel() {
            Ok(flag) => {
                println!(
                    "{}",
                    json!({
                        "type": "cancel_requested",
                        "path": flag.display().to_string(),
                    })
                );
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        "human-reply" => match human_reply_cli(&args) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        "status" => {
            let mut run_id = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--run-id" => {
                        i += 1;
                        run_id = args.get(i).cloned();
                    }
                    "-h" | "--help" => {
                        print_usage();
                        std::process::exit(0);
                    }
                    other => {
                        eprintln!("未知参数: {other}");
                        std::process::exit(2);
                    }
                }
                i += 1;
            }
            match lite_status(run_id.as_deref()) {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "probe" => {
            let code = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => match rt.block_on(crate::lite_probe::probe_cli(&args)) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("{e}");
                        2
                    }
                },
                Err(e) => {
                    eprintln!("{e}");
                    2
                }
            };
            std::process::exit(code);
        }
        "recipe" => match crate::lite_recipes::recipe_cli(&args) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        // 其余交给 worker 内核（run / update / last-log / accounts …）
        _ => crate::worker::run_cli(),
    }
}
