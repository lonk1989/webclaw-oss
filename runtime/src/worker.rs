//! 无 UI 的 webclaw-worker：伙伴空间 / Linux 无头 CDP 执行入口。

use crate::captcha;
use crate::run_sink::{self, FanoutRunSink, JsonlRunSink, RunEvent};
use crate::task_run::TaskControl;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DEFAULT_CDP_PORT: &str = "19222";

#[derive(Debug)]
struct CliArgs {
    recipe: Option<PathBuf>,
    recipe_text: Option<String>,
    title: String,
    run_id: Option<String>,
    status_only: bool,
    /// CLI `--params '{"公司":"…"}'` 预填菜谱参数
    params: HashMap<String, String>,
}

fn is_lite() -> bool {
    std::env::var_os("WEBCLAW_LITE").is_some()
}

fn print_usage() {
    if is_lite() {
        crate::lite::print_usage();
        return;
    }
    eprintln!(
        "webclaw-worker — 无头 CDP 执行器（伙伴空间，按需短命 host）

用法:
  webclaw-worker run --recipe <file.webclaw|txt> [--title 名称] [--params JSON]
  webclaw-worker run --stdin [--title 名称] [--params JSON]
  webclaw-worker status [--run-id <id>]
  webclaw-worker accounts list
  webclaw-worker accounts ensure --id ehr|old-mes [--name 名称] --url URL
  webclaw-worker accounts set --id ehr|old-mes [--name 名称] [--url URL] --user 账号 --password 密码
  webclaw-worker job --file <file.webclawjob>
  webclaw-worker secret-install --file <webclaw-remote-secret>
  webclaw-worker accounts-install --file <file.webclawacct>
  webclaw-worker host-pair-install --file <webclaw-host-pair.json>
  webclaw-worker pair-redeem --code <6位数字>
  webclaw-worker host --daemon [--pair <webclaw-host-pair.json>]
  webclaw-worker host [--pair <webclaw-host-pair.json>]
  webclaw-worker host status
  webclaw-worker host stop
  webclaw-worker update
  webclaw-worker last-log [--lines N]

说明:
  host 不再常驻：pair-redeem 拉起短命 host，接一单后退出并关闭 19222 浏览器。
  禁止在伙伴沙箱用 bash/setsid/nohup 拉起 host（工具必超时失败一次）。
  pair-redeem 内部 double-fork，秒级返回（不等 host 退出）。
  运维：host status / host stop / update / last-log。
  update 写 webclaw-worker.bin + 入口 loader，避免自替换卡住。

环境变量:
  WEBCLAW_HOME          数据根目录（优先；未设时用 AILY_WORKDIR，再回退 ~/.webclaw-worker 或 ~/webclaw）
  AILY_WORKDIR          当前任务工作目录（伙伴空间平台注入；可作 WEBCLAW_HOME）
  WEBCLAW_CHROME        Chromium 路径（默认探测；伙伴空间建议 /usr/bin/chromium-browser）
  WEBCLAW_CDP_PORT      CDP 端口（默认 19222，勿用平台 9222）
  WEBCLAW_USER_DATA     Chrome profile 目录
  WEBCLAW_HUMAN_DIR     人机交换目录（默认 <run>/human）
  WEBCLAW_CONTAINER=1   强制 --no-sandbox 等容器参数
  WEBCLAW_RUN_RELAY_WS  运行中继 WS（pair-redeem 用；默认公网）
  WEBCLAW_WORKER_URL    update 下载地址
"
    );
}

fn parse_params_json(raw: &str) -> Result<HashMap<String, String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw.trim()).map_err(|e| format!("--params JSON 无效: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "--params 须为 JSON 对象，如 {\"公司\":\"科技事业部\"}".to_string())?;
    let mut map = HashMap::new();
    for (k, val) in obj {
        let s = match val {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        map.insert(k.clone(), s);
    }
    Ok(map)
}

fn parse_cli(args: &[String]) -> Result<CliArgs, String> {
    if args.len() < 2 {
        print_usage();
        return Err("缺少子命令".into());
    }
    let cmd = args[1].as_str();
    match cmd {
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
                        return Err("help".into());
                    }
                    other => return Err(format!("未知参数: {other}")),
                }
                i += 1;
            }
            Ok(CliArgs {
                recipe: None,
                recipe_text: None,
                title: String::new(),
                run_id,
                status_only: true,
                params: HashMap::new(),
            })
        }
        "run" => {
            let mut recipe = None;
            let mut recipe_text = None;
            let mut title = "worker".to_string();
            let mut params = HashMap::new();
            let mut stdin = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--recipe" | "-r" => {
                        i += 1;
                        recipe = Some(PathBuf::from(args.get(i).ok_or("--recipe 需要路径")?));
                    }
                    "--title" | "-t" => {
                        i += 1;
                        title = args.get(i).ok_or("--title 需要名称")?.clone();
                    }
                    "--params" | "-p" => {
                        i += 1;
                        let raw = args.get(i).ok_or("--params 需要 JSON 字符串")?;
                        params = parse_params_json(raw)?;
                    }
                    "--stdin" => stdin = true,
                    "-h" | "--help" => {
                        print_usage();
                        return Err("help".into());
                    }
                    other => return Err(format!("未知参数: {other}")),
                }
                i += 1;
            }
            if stdin {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| e.to_string())?;
                recipe_text = Some(buf);
            }
            if recipe.is_none() && recipe_text.is_none() {
                return Err("请指定 --recipe <文件> 或 --stdin".into());
            }
            Ok(CliArgs {
                recipe,
                recipe_text,
                title,
                run_id: None,
                status_only: false,
                params,
            })
        }
        "-h" | "--help" | "help" => {
            print_usage();
            Err("help".into())
        }
        other => Err(format!(
            "未知子命令: {other}（用 run / status / accounts / job / secret-install / accounts-install / host / host-pair-install / pair-redeem）"
        )),
    }
}

fn ensure_worker_env() {
    if is_lite() {
        crate::lite::ensure_lite_env();
        return;
    }
    std::env::set_var("WEBCLAW_WORKER", "1");
    if std::env::var_os("WEBCLAW_CDP_PORT").is_none() {
        std::env::set_var("WEBCLAW_CDP_PORT", DEFAULT_CDP_PORT);
    }
    if std::env::var_os("WEBCLAW_CONTAINER").is_none() {
        std::env::set_var("WEBCLAW_CONTAINER", "1");
    }
    if std::env::var_os("WEBCLAW_HOME").is_none() {
        let dir = std::env::var("AILY_WORKDIR")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/home/gem".into());
                PathBuf::from(home).join("webclaw")
            });
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("WEBCLAW_HOME", dir);
    }
    if std::env::var_os("WEBCLAW_CHROME").is_none() {
        for cand in [
            "/usr/bin/chromium-browser",
            "/usr/bin/chromium",
            "/usr/bin/google-chrome-stable",
        ] {
            if Path::new(cand).is_file() {
                std::env::set_var("WEBCLAW_CHROME", cand);
                break;
            }
        }
    }
}

fn load_recipe_text(cli: &CliArgs) -> Result<String, String> {
    if let Some(t) = &cli.recipe_text {
        return Ok(t.clone());
    }
    let input = cli.recipe.as_ref().ok_or("无菜谱")?;
    let path = if is_lite() {
        crate::lite_recipes::resolve_recipe_path(input)
    } else {
        input.clone()
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("读菜谱失败 {}: {e}", path.display()))?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    // 兼容旧 .webclaw 带头 ---
    if let Some(idx) = raw.find("\n---\n") {
        return Ok(raw[idx + 5..].to_string());
    }
    if let Some(idx) = raw.find("\r\n---\r\n") {
        return Ok(raw[idx + 7..].to_string());
    }
    Ok(raw.to_string())
}

fn build_status_payload(run_id: &str) -> Result<serde_json::Value, String> {
    let inbox = crate::paths::app_data_dir().join("need_human.json");
    let last_img = crate::paths::app_data_dir().join("last-captcha.png");
    let inbox_val = if inbox.is_file() {
        std::fs::read_to_string(&inbox)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    } else {
        None
    };

    let last_need = crate::paths::app_data_dir().join("last-need-human.json");
    let last_need_val = if last_need.is_file() {
        std::fs::read_to_string(&last_need)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    } else {
        None
    };

    if run_id.trim().is_empty() {
        return Ok(serde_json::json!({
            "type": "status",
            "waitingHuman": inbox.is_file(),
            "needHuman": inbox_val,
            "lastNeedHuman": last_need_val,
            "inboxImage": if last_img.is_file() { Some(last_img.display().to_string()) } else { None },
        }));
    }

    let dir = crate::paths::runs_dir();
    let mut found = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir()
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.contains(run_id))
            {
                found = Some(p);
                break;
            }
        }
    }
    let run_dir = found.ok_or_else(|| format!("未找到 run-id 含「{run_id}」的目录"))?;
    let waiting = run_dir.join("human").join("waiting_human.json");
    Ok(serde_json::json!({
        "type": "status",
        "runDir": run_dir.display().to_string(),
        "waitingHuman": waiting.is_file() || inbox.is_file(),
        "waitingHumanPath": if waiting.is_file() { Some(waiting.display().to_string()) } else { None },
        "needHuman": inbox_val,
        "inboxImage": if last_img.is_file() { Some(last_img.display().to_string()) } else { None },
    }))
}

fn status_run(run_id: &str) -> Result<(), String> {
    let payload = build_status_payload(run_id)?;
    println!("{}", serde_json::to_string(&payload).unwrap_or_default());
    Ok(())
}

/// 供 lite 增强 status 字段。
pub fn status_run_for_lite(
    run_id: &str,
    enrich: impl FnOnce(serde_json::Value),
) -> Result<(), String> {
    let payload = build_status_payload(run_id)?;
    enrich(payload);
    Ok(())
}

async fn run_recipe_async(cli: CliArgs) -> Result<(), String> {
    let task_id = format!("worker-{}", uuid::Uuid::new_v4());
    run_recipe_async_inner(cli, true, task_id).await
}

/// 另一进程写入 `<WEBCLAW_HOME>/control/cancel` 时，打断当前 run（含 AI 等待与长步骤）。
fn spawn_external_cancel_watch(control: TaskControl) {
    let home = match std::env::var("WEBCLAW_HOME") {
        Ok(h) if !h.trim().is_empty() => h,
        _ => return,
    };
    let flag = PathBuf::from(home.trim()).join("control").join("cancel");
    tokio::spawn(async move {
        loop {
            if control.cancel.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = std::fs::remove_file(&flag);
                return;
            }
            if flag.is_file() {
                let _ = std::fs::remove_file(&flag);
                crate::task_run::cancel_current_job();
                crate::run_sink::emit_log("收到 cancel，正在停止当前任务");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    });
}

async fn run_recipe_async_inner(
    cli: CliArgs,
    install_jsonl_sink: bool,
    task_id: String,
) -> Result<(), String> {
    if install_jsonl_sink {
        run_sink::install_worker_sink(Arc::new(JsonlRunSink));
    }
    let content_raw = load_recipe_text(&cli)?;
    if is_lite() && content_raw.contains("技能") {
        match crate::corp_skills::sync_skills().await {
            Ok(n) => run_sink::emit_log(format!("已按当前登录同步技能 {n} 条")),
            Err(e) => {
                run_sink::emit_event(RunEvent::Done {
                    ok: 0,
                    fail: 1,
                    error: Some(e.clone()),
                    run_dir: None,
                });
                return Err(e);
            }
        }
    }

    // 不因缺账号/密码预检拦截：站点可能已有会话或不需要密码。
    // 「登录」步会先打开入口 URL；仅当页面需要填表且 vault 无凭据时再失败提示。

    let task_id = task_id;
    let control = TaskControl::new();
    crate::task_run::register_current_job(control.clone());
    spawn_external_cancel_watch(control.clone());
    let _job_guard = crate::task_run::CurrentJobGuard;

    run_via_shared_executor(&cli, &task_id, &control, &content_raw).await
}

/// 桌面 feature 下 worker 模块仍会被编译，但 worker 二进制始终 `--no-default-features`。
#[cfg(not(feature = "desktop"))]
async fn run_via_shared_executor(
    cli: &CliArgs,
    task_id: &str,
    control: &TaskControl,
    content_raw: &str,
) -> Result<(), String> {
    let state = crate::AppState {
        logs: Arc::new(parking_lot::Mutex::new(Vec::new())),
        chrome_child: Arc::new(parking_lot::Mutex::new(None)),
        cancel_flag: control.cancel.clone(),
        ai_stream_cancels: Arc::new(parking_lot::Mutex::new(Default::default())),
        task_registry: Arc::new(crate::task_run::TaskRegistry::new()),
        active_browser: Arc::new(parking_lot::Mutex::new(None)),
        last_main_target: Arc::new(parking_lot::Mutex::new(None)),
        last_browser_snapshot: Arc::new(parking_lot::Mutex::new(None)),
        captcha_waiters: captcha::new_waiters(),
    };
    let app = crate::ui_host::AppHandle::<crate::ui_host::Wry>::default();

    // 与桌面共用 executor：页面恢复重试 + AI 修复 + 失败分析
    let result = crate::executor::execute_recipe_for_task_from_with_params(
        &app,
        &state,
        task_id,
        control,
        Some(&cli.title),
        content_raw,
        None,
        None,
        &cli.params,
    )
    .await?;

    if result.starts_with("failed:") {
        run_sink::emit_event(RunEvent::Done {
            ok: 0,
            fail: 1,
            error: Some(result.clone()),
            run_dir: control
                .run_session
                .lock()
                .as_ref()
                .map(|s| s.run_dir.display().to_string()),
        });
        return Err(result);
    }
    if result.contains("fail=") {
        let fail = result
            .split("fail=")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if fail > 0 {
            return Err("recipe failed".into());
        }
    }
    Ok(())
}

#[cfg(feature = "desktop")]
async fn run_via_shared_executor(
    _cli: &CliArgs,
    _task_id: &str,
    _control: &TaskControl,
    _content_raw: &str,
) -> Result<(), String> {
    Err("webclaw-worker 请使用 --no-default-features 构建".into())
}

fn accounts_cli(args: &[String]) -> Result<(), String> {
    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "" => {
            let items = crate::web_systems::list_worker_accounts()?;
            println!(
                "{}",
                serde_json::json!({
                    "type": "accounts",
                    "items": items,
                })
            );
            Ok(())
        }
        "ensure" => {
            let mut id = None;
            let mut name = None;
            let mut url = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--id" => {
                        i += 1;
                        id = args.get(i).cloned();
                    }
                    "--name" => {
                        i += 1;
                        name = args.get(i).cloned();
                    }
                    "--url" => {
                        i += 1;
                        url = args.get(i).cloned();
                    }
                    other => return Err(format!("未知参数: {other}")),
                }
                i += 1;
            }
            let url = url.ok_or("accounts ensure 需要 --url")?;
            let saved = crate::web_systems::ensure_worker_system_entry(
                id.as_deref(),
                name.as_deref(),
                &url,
            )?;
            println!(
                "{}",
                serde_json::json!({
                    "type": "account_ensured",
                    "id": saved.id,
                    "name": saved.name,
                    "url": saved.url,
                    "username": saved.username,
                    "hasPassword": saved.has_password,
                })
            );
            Ok(())
        }
        "set" => {
            let mut id = None;
            let mut name = None;
            let mut url = None;
            let mut user = None;
            let mut password = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--id" => {
                        i += 1;
                        id = args.get(i).cloned();
                    }
                    "--name" => {
                        i += 1;
                        name = args.get(i).cloned();
                    }
                    "--url" => {
                        i += 1;
                        url = args.get(i).cloned();
                    }
                    "--user" | "--username" => {
                        i += 1;
                        user = args.get(i).cloned();
                    }
                    "--password" | "--pass" => {
                        i += 1;
                        password = args.get(i).cloned();
                    }
                    other => return Err(format!("未知参数: {other}")),
                }
                i += 1;
            }
            let user = user.ok_or("accounts set 需要 --user")?;
            let password = password.ok_or("accounts set 需要 --password")?;
            let saved = crate::web_systems::upsert_worker_account(
                id.as_deref(),
                name.as_deref(),
                url.as_deref(),
                &user,
                &password,
            )?;
            println!(
                "{}",
                serde_json::json!({
                    "type": "account_saved",
                    "id": saved.id,
                    "name": saved.name,
                    "url": saved.url,
                    "username": saved.username,
                    "hasPassword": saved.has_password,
                })
            );
            Ok(())
        }
        "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("未知 accounts 子命令: {other}（用 list / ensure / set）")),
    }
}

fn secret_install_cli(args: &[String]) -> Result<(), String> {
    let mut file = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                i += 1;
                file = Some(PathBuf::from(args.get(i).ok_or("--file 需要路径")?));
            }
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    let src = file.ok_or("secret-install 需要 --file")?;
    let dest = crate::remote_job::install_secret_from_file(&src)?;
    println!(
        "{}",
        serde_json::json!({
            "type": "secret_installed",
            "path": dest.display().to_string(),
        })
    );
    Ok(())
}

fn accounts_install_cli(args: &[String]) -> Result<(), String> {
    let mut file = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                i += 1;
                file = Some(PathBuf::from(args.get(i).ok_or("--file 需要路径")?));
            }
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    let src = file.ok_or("accounts-install 需要 --file")?;
    let n = crate::remote_job::install_accounts_from_file(&src)?;
    println!(
        "{}",
        serde_json::json!({
            "type": "accounts_installed",
            "count": n,
        })
    );
    Ok(())
}

fn pair_redeem_cli(args: &[String]) -> Result<(), String> {
    let mut code = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--code" | "-c" => {
                i += 1;
                code = Some(args.get(i).ok_or("--code 需要 6 位数字")?.trim().to_string());
            }
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    let code = code.ok_or("pair-redeem 需要 --code <6位数字>")?;
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err("配对码须为 6 位数字".into());
    }

    let ws = crate::run_relay::resolve_ws_url()
        .or_else(|| {
            Some(crate::run_relay::normalize_ws_url(
                &crate::settings::default_run_relay_ws_url(),
            ))
        })
        .ok_or("未配置运行中继")?;
    let http = crate::run_relay::relay_http_base_from_ws(&ws);
    let url = format!("{http}/api/pair/redeem");

    // 兑换必须在独立作用域：fork/daemonize 前销毁 tokio runtime，避免带着 runtime fork
    let body = {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| e.to_string())?;
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "code": code }))
                .send()
                .await
                .map_err(|e| format!("兑换配对码失败: {e}"))?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let v: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}));
            if !status.is_success() || v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
                let err = v
                    .get("error")
                    .and_then(|x| x.as_str())
                    .unwrap_or("INVALID_CODE");
                return Err(format!("兑换失败: {err}"));
            }
            Ok(v)
        })?
    };

    let owner = body
        .get("ownerClientId")
        .and_then(|x| x.as_str())
        .ok_or("中继未返回 ownerClientId")?
        .to_string();
    let host_token = body
        .get("hostToken")
        .and_then(|x| x.as_str())
        .ok_or("中继未返回 hostToken")?
        .to_string();
    let job_secret_b64 = body
        .get("jobSecret")
        .and_then(|x| x.as_str())
        .ok_or("中继未返回 jobSecret")?;
    let relay_ws = body
        .get("relayWsUrl")
        .and_then(|x| x.as_str())
        .unwrap_or(ws.as_str());

    use base64::Engine;
    let secret_raw = base64::engine::general_purpose::STANDARD
        .decode(job_secret_b64.trim())
        .map_err(|_| "jobSecret 无效".to_string())?;
    crate::remote_job::install_secret_from_bytes(&secret_raw)?;

    let pair = crate::remote_job::HostPairFile {
        version: 1,
        relay_ws_url: crate::run_relay::normalize_ws_url(relay_ws),
        owner_client_id: owner,
        host_token,
    };
    let pair_json = serde_json::to_vec_pretty(&pair).map_err(|e| e.to_string())?;
    let pair_path = crate::remote_job::install_host_pair_from_bytes(&pair_json)?;

    // 先落盘成功就视为兑换成功；再拉起 host（父进程必须立刻返回，否则沙箱 30s timeout）
    let (host_daemon_pid, host_daemon_err) =
        match crate::host_agent::spawn_detached_host(Some(&pair_path)) {
            Ok(pid) => (Some(pid), None),
            Err(e) => (None, Some(e)),
        };

    println!(
        "{}",
        serde_json::json!({
            "type": "pair_redeemed",
            "hostPairPath": pair_path.display().to_string(),
            "hostDaemonPid": host_daemon_pid,
            "hostDaemonError": host_daemon_err,
        })
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // 强制退出：跳过可能残留的后台线程 join，避免「兑换已成功仍挂满 30s」
    std::process::exit(0);
}

fn host_pair_install_cli(args: &[String]) -> Result<(), String> {
    let mut file = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                i += 1;
                file = Some(PathBuf::from(args.get(i).ok_or("--file 需要路径")?));
            }
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    let src = file.ok_or("host-pair-install 需要 --file")?;
    let dest = crate::remote_job::install_host_pair_from_file(&src)?;
    let (host_daemon_pid, host_daemon_err) = match crate::host_agent::spawn_detached_host(Some(&dest))
    {
        Ok(pid) => (Some(pid), None),
        Err(e) => (None, Some(e)),
    };
    println!(
        "{}",
        serde_json::json!({
            "type": "host_pair_installed",
            "path": dest.display().to_string(),
            "hostDaemonPid": host_daemon_pid,
            "hostDaemonError": host_daemon_err,
        })
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::process::exit(0);
}

fn job_file_from_args(args: &[String]) -> Result<PathBuf, String> {
    let mut file = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                i += 1;
                file = Some(PathBuf::from(args.get(i).ok_or("--file 需要路径")?));
            }
            "-h" | "--help" => return Err("help".into()),
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    file.ok_or_else(|| "job 需要 --file <file.webclawjob>".into())
}

async fn run_job_file_async(path: PathBuf) -> Result<(), String> {
    let packed = std::fs::read(&path).map_err(|e| format!("读任务包失败: {e}"))?;
    if packed.len() >= 4 && &packed[0..4] != b"WCJB" {
        return Err("不是 webclaw 远程任务包（.webclawjob）。不要用编辑器打开该文件。".into());
    }
    let key = crate::remote_job::read_secret_bytes(&crate::remote_job::secret_path())?;
    let job = crate::remote_job::unpack_job(&packed, &key)?;
    crate::remote_job::import_job_accounts(&job.accounts)?;

    let mut sinks: Vec<Arc<dyn crate::run_sink::RunSink>> = vec![Arc::new(JsonlRunSink)];
    let mut relay_session: Option<crate::run_relay::WorkerRelaySession> = None;
    let relay_ws = job
        .relay
        .as_ref()
        .map(|r| r.ws_url.clone())
        .or_else(crate::run_relay::resolve_ws_url);
    let relay_token = job.relay.as_ref().map(|r| r.run_token.clone());
    if let (Some(ws), Some(token)) = (relay_ws, relay_token) {
        match crate::run_relay::attach_worker(&ws, &job.job_id, &token).await {
            Ok(session) => {
                sinks.push(session.sink.clone());
                relay_session = Some(session);
                let short = &job.job_id[..8.min(job.job_id.len())];
                eprintln!("[webclaw-run-relay] worker 已挂上 job={short}");
            }
            Err(e) => {
                crate::run_relay::warn_relay(&format!("挂上失败（仍本地执行）: {e}"));
            }
        }
    }
    run_sink::install_worker_sink(Arc::new(FanoutRunSink { sinks }));

    if job.resume_browser {
        std::env::set_var("WEBCLAW_RESUME_BROWSER", "1");
    } else {
        std::env::remove_var("WEBCLAW_RESUME_BROWSER");
    }
    let task_id = {
        let t = job.run_task_id.trim();
        if t.is_empty() {
            format!("worker-{}", uuid::Uuid::new_v4())
        } else {
            t.to_string()
        }
    };

    let cli = CliArgs {
        recipe: None,
        recipe_text: Some(job.recipe),
        title: job.title,
        run_id: None,
        status_only: false,
        params: job.params,
    };
    let result = run_recipe_async_inner(cli, false, task_id).await;
    drop(relay_session);
    result
}

/// 供 host agent 调用。
pub async fn run_job_file_for_host(path: PathBuf) -> Result<(), String> {
    run_job_file_async(path).await
}

/// CLI 入口（由 `webclaw-worker` 二进制调用）。
pub fn run_cli() {
    ensure_worker_env();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("accounts") {
        match accounts_cli(&args) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("secret-install") {
        match secret_install_cli(&args) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("accounts-install") {
        match accounts_install_cli(&args) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("host-pair-install") {
        match host_pair_install_cli(&args) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("pair-redeem") {
        match pair_redeem_cli(&args) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("update") {
        match crate::host_agent::self_update() {
            Ok(v) => {
                println!("{v}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("last-log") {
        let mut lines = 120usize;
        let mut i = 2;
        while i < args.len() {
            match args[i].as_str() {
                "--lines" | "-n" => {
                    i += 1;
                    lines = args
                        .get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(120);
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
        print!("{}", crate::host_agent::last_log_text(lines));
        std::process::exit(0);
    }
    if args.get(1).map(|s| s.as_str()) == Some("host") {
        let mut pair = None;
        let mut daemon = false;
        let mut i = 2;
        while i < args.len() {
            match args[i].as_str() {
                "status" => {
                    println!("{}", crate::host_agent::service_status_json());
                    std::process::exit(0);
                }
                "stop" => {
                    println!("{}", crate::host_agent::stop_host_service());
                    std::process::exit(0);
                }
                "--pair" | "-p" => {
                    i += 1;
                    let p = match args.get(i) {
                        Some(s) => PathBuf::from(s),
                        None => {
                            eprintln!("--pair 需要路径");
                            std::process::exit(2);
                        }
                    };
                    pair = Some(p);
                }
                "--daemon" => daemon = true,
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
        if daemon {
            match crate::host_agent::spawn_detached_host(pair.as_deref()) {
                Ok(pid) => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "type": "host_daemon_started",
                            "pid": pid,
                        })
                    );
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            }
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(crate::host_agent::run_host_forever(pair));
    }
    if args.get(1).map(|s| s.as_str()) == Some("job") {
        let path = match job_file_from_args(&args) {
            Ok(p) => p,
            Err(e) => {
                if e != "help" {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
                print_usage();
                std::process::exit(0);
            }
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let code = match rt.block_on(run_job_file_async(path)) {
            Ok(()) => 0,
            Err(e) => {
                if e != "recipe failed" {
                    run_sink::install_worker_sink(Arc::new(JsonlRunSink));
                    run_sink::emit_event(RunEvent::Done {
                        ok: 0,
                        fail: 1,
                        error: Some(e),
                        run_dir: None,
                    });
                    run_sink::clear_worker_sink();
                }
                1
            }
        };
        std::process::exit(code);
    }
    let cli = match parse_cli(&args) {
        Ok(c) => c,
        Err(e) => {
            if e != "help" {
                eprintln!("{e}");
                std::process::exit(2);
            }
            std::process::exit(0);
        }
    };

    if cli.status_only {
        let id = cli.run_id.as_deref().unwrap_or("");
        if let Err(e) = status_run(id) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = match rt.block_on(run_recipe_async(cli)) {
        Ok(()) => 0,
        Err(e) => {
            if e != "recipe failed" {
                run_sink::emit_event(RunEvent::Done {
                    ok: 0,
                    fail: 1,
                    error: Some(e),
                    run_dir: None,
                });
            }
            1
        }
    };
    run_sink::clear_worker_sink();
    std::process::exit(code);
}
