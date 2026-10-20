use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::OnceLock;

static CURRENT_USER: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static LEGACY_ROOT_CLEANED: OnceLock<()> = OnceLock::new();

fn current_user_lock() -> &'static Mutex<Option<String>> {
    CURRENT_USER.get_or_init(|| Mutex::new(None))
}

fn migrate_dir_if_needed(base: &PathBuf, old_name: &str, new_name: &str) -> PathBuf {
    let new_dir = base.join(new_name);
    let old_dir = base.join(old_name);
    if !new_dir.exists() && old_dir.exists() {
        let _ = std::fs::rename(&old_dir, &new_dir);
    }
    new_dir
}

/// 机器级根目录：`%LOCALAPPDATA%/webclaw`（仅放 users/、last-user、文件关联标记等）
pub fn app_root_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    migrate_dir_if_needed(&base, "vision-coord-client", "webclaw")
}

/// RapidOCR ONNX 模型缓存（机器级，与登录用户无关）
pub fn ocr_models_dir() -> PathBuf {
    let dir = app_root_dir().join("ocr-models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// CLIP 视觉分类模型缓存（机器级）
pub fn classify_models_dir() -> PathBuf {
    let dir = app_root_dir().join("classify-models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 微信 .dat 解密图片公共缓存（机器级）
pub fn wechat_img_cache_dir() -> PathBuf {
    let dir = app_root_dir().join("wechat-img-cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 将登录用户名规范为目录安全键
pub fn sanitize_user_key(raw: &str) -> String {
    let s: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if s.is_empty() {
        "_anonymous".into()
    } else {
        s
    }
}

fn last_user_file() -> PathBuf {
    app_root_dir().join("last-user.txt")
}

fn write_last_user(key: &str) {
    let root = app_root_dir();
    let _ = std::fs::create_dir_all(&root);
    let _ = std::fs::write(last_user_file(), key.as_bytes());
}

/// 读取上次登录用户（供无 UI 的任务启动回落）
pub fn load_last_user_key() -> Option<String> {
    let raw = std::fs::read_to_string(last_user_file()).ok()?;
    let key = sanitize_user_key(&raw);
    if key == "_anonymous" || key == "_default" {
        None
    } else {
        Some(key)
    }
}

/// 绑定当前登录用户；之后所有读写只落在 `users/<username>/`
pub fn set_current_user(username: Option<&str>) -> String {
    let key = match username.map(str::trim).filter(|s| !s.is_empty()) {
        Some(u) => sanitize_user_key(u),
        None => "_default".into(),
    };
    *current_user_lock().lock() = Some(key.clone());
    if key != "_default" && key != "_anonymous" {
        write_last_user(&key);
    }
    let dir = user_data_dir_for(&key);
    let _ = std::fs::create_dir_all(&dir);
    cleanup_legacy_root_data();
    key
}

/// 启动时恢复上次用户（若尚未 set）
pub fn ensure_user_scope_initialized() {
    let mut guard = current_user_lock().lock();
    if guard.is_some() {
        drop(guard);
        cleanup_legacy_root_data();
        return;
    }
    let key = load_last_user_key().unwrap_or_else(|| "_default".into());
    *guard = Some(key.clone());
    drop(guard);
    let dir = user_data_dir_for(&key);
    let _ = std::fs::create_dir_all(&dir);
    cleanup_legacy_root_data();
}

pub fn current_user_key() -> String {
    current_user_lock()
        .lock()
        .clone()
        .unwrap_or_else(|| "_default".into())
}

fn user_data_dir_for(user_key: &str) -> PathBuf {
    app_root_dir().join("users").join(user_key)
}

/// 当前登录用户的数据目录（唯一业务数据根）
pub fn app_data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("WEBCLAW_HOME") {
        let dir = PathBuf::from(home.trim());
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(&dir);
            return dir;
        }
    }
    ensure_user_scope_initialized();
    user_data_dir_for(&current_user_key())
}

/// 删除根目录下旧版扁平数据（不再迁入、不再读取）
pub fn cleanup_legacy_root_data() {
    if LEGACY_ROOT_CLEANED.get().is_some() {
        return;
    }
    let _ = LEGACY_ROOT_CLEANED.set(());

    let root = app_root_dir();
    if !root.is_dir() {
        return;
    }

    const LEGACY_FILES: &[&str] = &[
        "recipes.json",
        "commands.json",
        "commands.json.bak",
        "scripts.json",
        "settings.json",
        "web-systems.json",
        "templates.json",
        "bitables.json",
    ];
    for name in LEGACY_FILES {
        let p = root.join(name);
        if p.is_file() {
            let _ = std::fs::remove_file(&p);
        }
    }

    const LEGACY_DIRS: &[&str] = &["runs", "templates", "manual-cache"];
    for name in LEGACY_DIRS {
        let p = root.join(name);
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }

    // 各用户目录内的迁移标记已无意义
    let users = root.join("users");
    if users.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&users) {
            for entry in entries.flatten() {
                let marker = entry.path().join(".migrated-from-root-v1");
                if marker.is_file() {
                    let _ = std::fs::remove_file(marker);
                }
            }
        }
    }
}

pub fn recipes_file() -> PathBuf {
    app_data_dir().join("recipes.json")
}

pub fn scheduled_jobs_file() -> PathBuf {
    app_data_dir().join("scheduled-jobs.json")
}

/// 云端技能列表的本地镜像（供「技能名称」解析展开）
pub fn skills_cache_file() -> PathBuf {
    app_data_dir().join("skills-cache.json")
}

pub fn commands_file() -> PathBuf {
    app_data_dir().join("commands.json")
}

pub fn scripts_file() -> PathBuf {
    app_data_dir().join("scripts.json")
}

pub fn assert_scripts_file() -> PathBuf {
    app_data_dir().join("assert_scripts.json")
}

pub fn settings_file() -> PathBuf {
    app_data_dir().join("settings.json")
}

pub fn public_params_file() -> PathBuf {
    app_data_dir().join("public-params.json")
}

pub fn web_systems_file() -> PathBuf {
    app_data_dir().join("web-systems.json")
}

/// 每次快捷指令执行的产物根目录（其下按时间戳分子目录）
pub fn runs_dir() -> PathBuf {
    let dir = app_data_dir().join("runs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 首页对话 session 的稳定产物目录：`runs/sessions/<session_id>/`
pub fn chat_session_runs_dir(session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    let id = if safe.is_empty() {
        "unknown"
    } else {
        safe.as_str()
    };
    let dir = runs_dir().join("sessions").join(id);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn templates_dir() -> PathBuf {
    let dir = app_data_dir().join("templates");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn templates_meta_file() -> PathBuf {
    app_data_dir().join("templates.json")
}

pub fn bitables_file() -> PathBuf {
    app_data_dir().join("bitables.json")
}

/// 文件关联标记放在机器根目录（与登录用户无关）
pub fn file_assoc_marker_file() -> PathBuf {
    app_root_dir().join("file-assoc-registered.txt")
}

/// 当前用户的 Chrome 调试配置目录
pub fn chrome_debug_dir() -> PathBuf {
    let dir = app_data_dir().join("chrome-debug");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 当前用户的并行任务 Chrome 配置根目录
pub fn chrome_profiles_dir() -> PathBuf {
    let dir = app_data_dir().join("chrome-profiles");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
