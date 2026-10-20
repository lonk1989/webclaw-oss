use crate::paths;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AppTheme {
    Light,
    Dark,
}

impl Default for AppTheme {
    fn default() -> Self {
        AppTheme::Light
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// 开启后：自然语言先经 AI 转为菜谱再执行；支持聊天记录总结菜谱
    #[serde(default = "default_ai_enhance")]
    pub ai_enhance: bool,
    /// 界面外观：浅色 / 深色，默认浅色
    #[serde(default)]
    pub theme: AppTheme,
    /// 本机在中继服务上的客户端凭证（register 后保存）
    #[serde(default)]
    pub feishu_client_id: String,
    #[serde(default)]
    pub feishu_client_secret: String,
    /// 是否启用飞书（关闭则断开中继；启用后启动时自动连接）
    #[serde(default = "default_feishu_enabled")]
    pub feishu_enabled: bool,
    /// 飞书消息写入当前对话
    #[serde(default = "default_feishu_sync_incoming")]
    pub feishu_sync_incoming: bool,
    /// 手动指定的 Chrome/Edge 可执行文件路径；空则自动检测
    #[serde(default)]
    pub chrome_executable: String,
    /// 后台/并行任务以无头模式启动 Chrome（--headless=new）；关闭则显示浏览器窗口
    #[serde(default = "default_background_chrome_headless")]
    pub background_chrome_headless: bool,
    /// 在线更新基址（latest.json / webclaw.exe 所在目录 URL）；空则用内置默认
    #[serde(default = "default_update_base_url")]
    pub update_base_url: String,
    /// 兼容旧配置保留；当前助手运行包流程不再使用该字段
    #[serde(default)]
    pub remote_job_folder: String,
    /// 启用远程运行进度中继（apps/webclaw-run-relay）
    #[serde(default = "default_run_relay_enabled")]
    pub run_relay_enabled: bool,
    /// 中继 WebSocket 基址或完整 `/ws` 地址；空则用内置公网默认
    #[serde(default = "default_run_relay_ws_url")]
    pub run_relay_ws_url: String,
    /// 本机在运行中继上的凭证（register 后自动写入）
    #[serde(default)]
    pub run_relay_client_id: String,
    #[serde(default)]
    pub run_relay_client_secret: String,
    /// 宿主机 host_hello 凭证（register/auth 后由中继下发）
    #[serde(default)]
    pub run_relay_host_token: String,
    /// 是否显示定时任务桌面悬浮窗
    #[serde(default = "default_schedule_floater_enabled")]
    pub schedule_floater_enabled: bool,
    /// 帮助页连点版本号解锁后为 true（测试工程师高级能力入口）
    #[serde(default)]
    pub assert_unlocked: bool,
    /// 是否启用断言能力（需先 unlock；关闭时隐藏相关 UI 并拒绝执行「断言」）
    #[serde(default)]
    pub assert_enabled: bool,
}

fn default_background_chrome_headless() -> bool {
    true
}

fn default_schedule_floater_enabled() -> bool {
    false
}

fn default_ai_enhance() -> bool {
    true
}

/// 是否跑步骤失败后的 AI 修复 / 失败分析。
/// 轻客户端默认关闭（会空等模型数十秒且没有桌面「继续」按钮）；`WEBCLAW_AI=1` 可打开。
pub fn ai_enhance_enabled() -> bool {
    if std::env::var_os("WEBCLAW_LITE").is_some() && std::env::var_os("WEBCLAW_AI").is_none() {
        return false;
    }
    load_settings().map(|s| s.ai_enhance).unwrap_or(true)
}

fn default_feishu_enabled() -> bool {
    false
}

fn default_feishu_sync_incoming() -> bool {
    false
}

pub fn default_update_base_url() -> String {
    String::new()
}

fn default_run_relay_enabled() -> bool {
    false
}

/// Optional run-relay WebSocket (disabled by default in OSS).
pub fn default_run_relay_ws_url() -> String {
    String::new()
}

/// 解析实际使用的更新基址（去尾斜杠；空配置回退默认）
pub fn resolve_update_base_url(settings: &AppSettings) -> String {
    let u = settings.update_base_url.trim().trim_end_matches('/').to_string();
    if u.is_empty() {
        default_update_base_url()
            .trim()
            .trim_end_matches('/')
            .to_string()
    } else {
        u
    }
}

/// 解析运行中继 WS（空配置回退内置公网）
pub fn resolve_run_relay_ws_url(settings: &AppSettings) -> String {
    let u = settings.run_relay_ws_url.trim().to_string();
    if u.is_empty() {
        default_run_relay_ws_url()
    } else {
        u
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            ai_enhance: true,
            theme: AppTheme::default(),
            feishu_client_id: String::new(),
            feishu_client_secret: String::new(),
            feishu_enabled: default_feishu_enabled(),
            feishu_sync_incoming: default_feishu_sync_incoming(),
            chrome_executable: String::new(),
            background_chrome_headless: default_background_chrome_headless(),
            update_base_url: default_update_base_url(),
            remote_job_folder: String::new(),
            run_relay_enabled: default_run_relay_enabled(),
            run_relay_ws_url: default_run_relay_ws_url(),
            run_relay_client_id: String::new(),
            run_relay_client_secret: String::new(),
            run_relay_host_token: String::new(),
            schedule_floater_enabled: default_schedule_floater_enabled(),
            assert_unlocked: false,
            assert_enabled: false,
        }
    }
}

/// 后台/并行任务是否使用无头 Chrome
pub fn background_chrome_headless() -> bool {
    load_settings()
        .map(|s| s.background_chrome_headless)
        .unwrap_or(true)
}

/// 断言能力是否对用户可用（已解锁且已开启）
pub fn assert_feature_enabled() -> bool {
    load_settings()
        .map(|s| s.assert_unlocked && s.assert_enabled)
        .unwrap_or(false)
}

pub fn load_settings() -> Result<AppSettings, String> {
    let path = paths::settings_file();
    if !path.exists() {
        let s = AppSettings::default();
        save_settings(&s)?;
        return Ok(s);
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut s: AppSettings =
        serde_json::from_str(&data).map_err(|e| format!("settings.json 解析失败: {e}"))?;
    let mut dirty = false;
    if s.run_relay_ws_url.trim().is_empty() {
        s.run_relay_ws_url = default_run_relay_ws_url();
        s.run_relay_enabled = true;
        dirty = true;
    }
    if dirty {
        save_settings(&s)?;
    }
    Ok(s)
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    std::fs::create_dir_all(paths::app_data_dir()).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(paths::settings_file(), data).map_err(|e| e.to_string())
}
