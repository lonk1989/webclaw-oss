use crate::paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Script,
    Macro,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    /// 完全由 JavaScript 在页面执行（可返回坐标给 Rust 点击）
    #[serde(alias = "javascript", alias = "js")]
    JavaScript,
    /// 关闭当前标签并新建标签打开 URL
    #[serde(alias = "open", alias = "rust-open")]
    RustOpen,
    /// 等待秒数
    #[serde(alias = "wait", alias = "rust-wait")]
    RustWait,
    /// 刷新当前标签页
    #[serde(alias = "reload", alias = "rust-reload")]
    RustReload,
    /// CDP 新建标签并导航
    #[serde(alias = "new_tab", alias = "rust-new-tab")]
    RustNewTab,
    /// 关闭当前浏览器标签（至少保留 1 个）
    #[serde(alias = "close_tab", alias = "rust-close-tab", alias = "rust_close_tab")]
    RustCloseTab,
    /// 点击：先跑 script 取坐标再 CDP 点击
    #[serde(alias = "click", alias = "rust-click")]
    RustClick,
    /// 输入：先跑 script 聚焦再 insertText
    #[serde(alias = "input", alias = "rust-input")]
    RustInput,
    /// 登录：关当前标签后按网站账号打开登录页并填表
    #[serde(alias = "login", alias = "rust-login")]
    RustLogin,
    /// Rhai 脚本在本机执行（Excel 数据处理等；遗留自定义指令）
    #[serde(alias = "rhai")]
    Rhai,
    /// 调用「脚本」库中的命名脚本：脚本合并考勤数据
    #[serde(alias = "named_script", alias = "script_invoke")]
    NamedScript,
    /// 调用「断言」库中的命名断言脚本（浏览器 XPath / URL / 标题）
    #[serde(alias = "named_assert", alias = "assert_invoke")]
    NamedAssert,
    /// 飞书多维表格 Open API（新增/更新等）
    #[serde(alias = "feishu-bitable", alias = "feishu_bitable")]
    FeishuBitable,
    /// 飞书云盘 Open API 上传（从当前页文件夹 URL 解析 token）
    #[serde(alias = "feishu-drive", alias = "feishu_drive")]
    FeishuDrive,
    /// 向当前页文件选择器上传产物目录中的本地文件（CDP 注入；菜谱含本指令时全程屏蔽系统框）
    #[serde(alias = "upload", alias = "rust-upload", alias = "rust_upload")]
    RustUpload,
    /// 按序号切换 / 关闭浏览器页签，并激活活跃标签
    #[serde(alias = "tab", alias = "rust-tab", alias = "rust_tab")]
    RustTab,
    /// 系统剪切板：复制 / 粘贴到当前焦点
    #[serde(alias = "clipboard", alias = "rust-clipboard", alias = "rust_clipboard")]
    RustClipboard,
    /// 经飞书中继向已绑定用户推送消息 / 附件 / AI 总结
    #[serde(alias = "feishu-push", alias = "feishu_push")]
    FeishuPush,
    /// 人机验证：置前浏览器，等人完成滑块/勾选等后再继续
    #[serde(alias = "human_assist", alias = "rust-human-assist", alias = "rust_human_assist")]
    RustHumanAssist,
}

impl RunnerKind {
    /// 执行前是否跟随浏览器「前台活动标签」（与探针打分规则同源：`hasFocus` 优先）。
    ///
    /// **默认跟随。** 新增 `RunnerKind` 变体时必须在本 match 里显式归类，否则无法编译——
    /// 这样以后加指令不必再到处搜「要不要 sync_to_active_tab」。
    ///
    /// - **跟随**：读/点/采当前页、依赖当前页 URL 的 API（飞书等）
    /// - **不跟随**：纯等待、本机脚本、或指令自己管理标签（新建/关闭/页签#N）
    ///
    /// 步骤级额外豁免见 [`crate::commands::should_follow_active_tab`]（如「等待下载」）。
    pub fn follows_active_tab(self) -> bool {
        match self {
            // —— opt-out：不读当前页，或自己决定标签 ——
            Self::RustWait | Self::RustNewTab | Self::RustCloseTab | Self::RustTab => false,
            Self::Rhai | Self::NamedScript | Self::FeishuPush => false,
            // —— 默认：跟随活动标签 ——
            Self::JavaScript
            | Self::RustOpen
            | Self::RustReload
            | Self::RustClick
            | Self::RustInput
            | Self::RustLogin
            | Self::NamedAssert
            | Self::FeishuBitable
            | Self::FeishuDrive
            | Self::RustUpload
            | Self::RustClipboard
            | Self::RustHumanAssist => true,
        }
    }

    /// 是否允许拉起浏览器进程（仅「打开」「登录」）。
    pub fn opens_browser(self) -> bool {
        matches!(self, Self::RustOpen | Self::RustLogin)
    }

    /// 步骤是否依赖已连接的浏览器（本机脚本/纯等待不需要）。
    pub fn needs_browser(self) -> bool {
        !matches!(
            self,
            Self::Rhai | Self::NamedScript | Self::RustWait | Self::FeishuPush
        )
    }
}

/// 「参数」类不走指令表；其余按 runner 判断是否需要浏览器。
pub fn keyword_needs_browser(keyword: &str) -> bool {
    let kw = keyword.trim();
    // 硬编码：本机执行、不连 Chrome（避免用户库里 runner 配错导致误判）
    if matches!(kw, "参数" | "动态参数" | "脚本" | "AI" | "宏" | "技能" | "推送消息" | "推送附件" | "推送AI") {
        return false;
    }
    match get_by_keyword(kw) {
        Ok(Some(cmd)) => cmd.runner.needs_browser(),
        _ => true,
    }
}

/// 「等待」参数是否无需浏览器（纯延时 / 等下载文件）
pub fn wait_args_browser_free(keyword: &str, args: &str) -> bool {
    if keyword.trim() != "等待" {
        return false;
    }
    let a = args.trim();
    if a.is_empty() {
        return true;
    }
    if a.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return true;
    }
    // 「等待下载」「等待下载60」等：等本机文件，不依赖页面
    a.starts_with("下载")
}

/// 步骤级：是否需要已连接的浏览器（含参数感知）
pub fn step_needs_browser(keyword: &str, args: &str) -> bool {
    if wait_args_browser_free(keyword, args) {
        return false;
    }
    keyword_needs_browser(keyword)
}

/// 仅「打开」「登录」可主动启动浏览器。
pub fn keyword_opens_browser(keyword: &str) -> bool {
    match get_by_keyword(keyword) {
        Ok(Some(cmd)) => cmd.runner.opens_browser(),
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    pub id: String,
    pub name: String,
    /// 菜谱中的动词，如「点击」「打开标签」
    pub keyword: String,
    pub kind: CommandKind,
    pub runner: RunnerKind,
    pub builtin: bool,
    pub description: String,
    #[serde(default)]
    pub script: String,
    /// 宏命令：展开为菜谱正文
    #[serde(default)]
    pub macro_body: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CommandDb {
    commands: Vec<CommandDef>,
}

/// 历史上 kind/runner 可能被写反或写成简写，加载时自动纠正
fn repair_command_value(cmd: &mut serde_json::Value) -> bool {
    let Some(obj) = cmd.as_object_mut() else {
        return false;
    };
    let mut repaired = false;

    let kind_raw = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("script")
        .to_string();

    if kind_raw != "script" && kind_raw != "macro" {
        repaired = true;
        obj.insert("kind".to_string(), serde_json::json!("script"));
        let runner = obj
            .get("runner")
            .and_then(|v| v.as_str())
            .filter(|r| is_valid_runner_str(r));
        if runner.is_none() {
            obj.insert(
                "runner".to_string(),
                serde_json::json!(runner_str_from_legacy(&kind_raw)),
            );
        }
    }

    if let Some(runner) = obj.get("runner").and_then(|v| v.as_str()) {
        if !is_valid_runner_str(runner) {
            obj.insert(
                "runner".to_string(),
                serde_json::json!(runner_str_from_legacy(runner)),
            );
            repaired = true;
        }
    }

    if obj.get("kind").and_then(|v| v.as_str()) == Some("macro")
        && !obj.contains_key("macro_body")
    {
        obj.insert("macro_body".to_string(), serde_json::json!(""));
        repaired = true;
    }

    repaired
}

fn is_valid_runner_str(r: &str) -> bool {
    matches!(
        r,
        "java_script"
            | "javascript"
            | "js"
            | "rust_open"
            | "open"
            | "rust_wait"
            | "wait"
            | "rust_reload"
            | "reload"
            | "rust_new_tab"
            | "new_tab"
            | "rust_close_tab"
            | "close_tab"
            | "rust_click"
            | "click"
            | "rust_input"
            | "input"
            | "rust_login"
            | "login"
            | "rhai"
            | "named_script"
            | "script_invoke"
            | "named_assert"
            | "assert_invoke"
            | "feishu_bitable"
            | "feishu-bitable"
            | "feishu_drive"
            | "feishu-drive"
            | "rust_upload"
            | "upload"
            | "rust-upload"
            | "rust_tab"
            | "tab"
            | "rust-tab"
            | "rust_clipboard"
            | "clipboard"
            | "rust-clipboard"
    )
}

fn runner_str_from_legacy(s: &str) -> &'static str {
    match s {
        "open" | "rust_open" | "rust-open" => "rust_open",
        "wait" | "rust_wait" | "rust-wait" => "rust_wait",
        "reload" | "rust_reload" | "rust-reload" => "rust_reload",
        "new_tab" | "rust_new_tab" | "rust-new-tab" | "open_tab" => "rust_new_tab",
        "close_tab" | "rust_close_tab" | "rust-close-tab" => "rust_close_tab",
        "click" | "rust_click" | "rust-click" => "rust_click",
        "input" | "rust_input" | "rust-input" => "rust_input",
        "login" | "rust_login" | "rust-login" => "rust_login",
        "scroll" | "javascript" | "java_script" | "js" => "java_script",
        "rhai" => "rhai",
        "named_script" | "script_invoke" => "named_script",
        "named_assert" | "assert_invoke" => "named_assert",
        "feishu_bitable" | "feishu-bitable" => "feishu_bitable",
        "feishu_drive" | "feishu-drive" => "feishu_drive",
        "upload" | "rust_upload" | "rust-upload" => "rust_upload",
        "tab" | "rust_tab" | "rust-tab" => "rust_tab",
        "clipboard" | "rust_clipboard" | "rust-clipboard" => "rust_clipboard",
        _ => "java_script",
    }
}

fn parse_updated_at(obj: &serde_json::Map<String, serde_json::Value>) -> DateTime<Utc> {
    obj.get("updated_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

fn is_legacy_command(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    obj.contains_key("verb")
        || obj.contains_key("param_hint")
        || obj.contains_key("macro_template")
        || (!obj.contains_key("name") && !obj.contains_key("keyword"))
}

fn legacy_kind_runner(kind_raw: &str) -> (CommandKind, RunnerKind) {
    if kind_raw == "macro" {
        return (CommandKind::Macro, RunnerKind::JavaScript);
    }
    let runner = match runner_str_from_legacy(kind_raw) {
        "rust_open" => RunnerKind::RustOpen,
        "rust_wait" => RunnerKind::RustWait,
        "rust_reload" => RunnerKind::RustReload,
        "rust_new_tab" => RunnerKind::RustNewTab,
        "rust_close_tab" => RunnerKind::RustCloseTab,
        "rust_click" => RunnerKind::RustClick,
        "rust_input" => RunnerKind::RustInput,
        "rust_login" => RunnerKind::RustLogin,
        "rhai" => RunnerKind::Rhai,
        "named_script" => RunnerKind::NamedScript,
        "named_assert" => RunnerKind::NamedAssert,
        "feishu_bitable" => RunnerKind::FeishuBitable,
        "feishu_drive" => RunnerKind::FeishuDrive,
        "rust_upload" => RunnerKind::RustUpload,
        "rust_tab" => RunnerKind::RustTab,
        "rust_clipboard" => RunnerKind::RustClipboard,
        "feishu_push" => RunnerKind::FeishuPush,
        _ => RunnerKind::JavaScript,
    };
    (CommandKind::Script, runner)
}

fn normalize_command_entry(
    raw: &serde_json::Value,
    defaults: &HashMap<String, CommandDef>,
) -> Option<CommandDef> {
    let obj = raw.as_object()?;

    let mut repaired = raw.clone();
    repair_command_value(&mut repaired);
    if let Ok(cmd) = serde_json::from_value::<CommandDef>(repaired) {
        return Some(cmd);
    }

    let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !id.is_empty() {
        if let Some(def) = defaults.get(&id) {
            let mut cmd = def.clone();
            if let Some(d) = obj.get("description").and_then(|v| v.as_str()) {
                cmd.description = d.to_string();
            }
            if let Some(s) = obj.get("script").and_then(|v| v.as_str()) {
                if !s.trim().is_empty() {
                    cmd.script = s.to_string();
                }
            }
            cmd.updated_at = parse_updated_at(obj);
            return Some(cmd);
        }
    }

    let label = obj
        .get("verb")
        .or_else(|| obj.get("keyword"))
        .or_else(|| obj.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("自定义")
        .trim()
        .to_string();
    if label.is_empty() {
        return None;
    }

    let kind_raw = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("script");
    let (kind, runner) = if kind_raw == "script" || kind_raw == "macro" {
        let runner = obj
            .get("runner")
            .and_then(|v| v.as_str())
            .map(runner_str_from_legacy)
            .map(|s| match s {
                "rust_open" => RunnerKind::RustOpen,
                "rust_wait" => RunnerKind::RustWait,
                "rust_reload" => RunnerKind::RustReload,
                "rust_new_tab" => RunnerKind::RustNewTab,
                "rust_close_tab" => RunnerKind::RustCloseTab,
                "rust_click" => RunnerKind::RustClick,
                "rust_input" => RunnerKind::RustInput,
                "rust_login" => RunnerKind::RustLogin,
                "rhai" => RunnerKind::Rhai,
                "named_script" => RunnerKind::NamedScript,
                "named_assert" => RunnerKind::NamedAssert,
                "feishu_bitable" => RunnerKind::FeishuBitable,
                "feishu_drive" => RunnerKind::FeishuDrive,
                "rust_upload" => RunnerKind::RustUpload,
                "rust_tab" => RunnerKind::RustTab,
                "rust_clipboard" => RunnerKind::RustClipboard,
                "feishu_push" => RunnerKind::FeishuPush,
                _ => RunnerKind::JavaScript,
            })
            .unwrap_or(RunnerKind::JavaScript);
        if kind_raw == "macro" {
            (CommandKind::Macro, RunnerKind::JavaScript)
        } else {
            (CommandKind::Script, runner)
        }
    } else {
        legacy_kind_runner(kind_raw)
    };

    let macro_body = obj
        .get("macro_body")
        .or_else(|| obj.get("macro_template"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(CommandDef {
        id: if id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            id
        },
        name: label.clone(),
        keyword: label,
        kind,
        runner,
        builtin: obj.get("builtin").and_then(|v| v.as_bool()).unwrap_or(false),
        description: obj
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        script: obj
            .get("script")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        macro_body,
        updated_at: parse_updated_at(obj),
    })
}

fn default_commands_map() -> HashMap<String, CommandDef> {
    super::builtin::default_commands()
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect()
}

fn load_db_from_json(data: &str) -> Result<(CommandDb, bool), String> {
    let root: serde_json::Value =
        serde_json::from_str(data).map_err(|e| format!("commands.json 不是合法 JSON: {e}"))?;

    let defaults = default_commands_map();
    let mut repaired = false;
    let mut commands = Vec::new();

    if let Some(list) = root.get("commands").and_then(|v| v.as_array()) {
        for item in list {
            if item.as_object().is_some_and(is_legacy_command) {
                repaired = true;
            }
            if let Some(cmd) = normalize_command_entry(item, &defaults) {
                commands.push(cmd);
            } else {
                repaired = true;
            }
        }
    }

    if commands.is_empty() {
        commands = super::builtin::default_commands();
        repaired = true;
    } else {
        merge_missing_builtins(&mut commands);
        if repaired {
            // merged may add items even if not legacy
        }
    }

    Ok((CommandDb { commands }, repaired))
}

fn merge_missing_builtins(commands: &mut Vec<CommandDef>) {
    for def in super::builtin::default_commands() {
        if !commands.iter().any(|c| c.id == def.id) {
            commands.push(def);
        }
    }
}

pub const BUILTIN_CLICK_DESC: &str = "按可见文字、图标名、像素坐标或结构化目标点击；坐标写「100 200」（px，空格分隔，无参默认 0 0）；图标须写「图标:close-circle」（class/data-icon/aria/业务 class 兜底）；结构化如：按钮#2";

/// 将已保存的旧版「点击」内置脚本升级到 click-target-v6.16 + click-text-ranking-v6
/// （像素坐标点击在 Rust `run_click`，不在 JS 脚本内；description 仍须同步）
fn refresh_builtin_click_script(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_click" && c.builtin)
    {
        // v6.15：同源 iframe 用 topX/topY；v6：下拉选项点条目自身
        let script_stale = !cmd.script.contains("click-target-v6.16")
            || !cmd.script.contains("toTopViewportCoords")
            || !cmd.script.contains("click-text-ranking-v6")
            || !cmd.script.contains("isOptionLikeElement")
            || !cmd.script.contains("isComboDropdownPanel")
            || !cmd.script.contains("getOwnContinuousText")
            || !cmd.script.contains("coordOnly")
            || !cmd.script.contains("parseIconClickArgs")
            || !cmd.script.contains("formatIconClickCmd")
            || !cmd.script.contains("extractBareIconToken")
            || !cmd.script.contains("extractClassFallbackHint")
            || !cmd.script.contains("isLibraryShellClass")
            || !cmd.script.contains("isCancelRequested")
            || !cmd.script.contains("normalizeIconName")
            || !cmd.script.contains("structural-target-v19")
            || !cmd.script.contains("只比最终抽取名")
            || cmd.script.contains("extractLastClassFallback");
        let desc_stale = cmd.description != BUILTIN_CLICK_DESC;
        if script_stale {
            cmd.script = super::scripts::compose_click_script(super::scripts::CLICK_DOM);
            changed = true;
        }
        if desc_stale {
            cmd.description = BUILTIN_CLICK_DESC.into();
            changed = true;
        }
    }
    changed
}

/// 将已保存的旧版「输入」内置脚本升级到 input-target-v13
fn refresh_builtin_input_script(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_input" && c.builtin)
    {
        if !cmd.script.contains("input-target-v14")
            || !cmd.script.contains("structuralIsTrulyDisabled")
            || !cmd.script.contains("isCancelRequested")
            || !cmd.script.contains("structural-target-v19")
            || cmd.script.contains("click-text-ranking-v1")
        {
            cmd.script = super::scripts::compose_script(super::scripts::INPUT_FIELD);
            cmd.description =
                "输入文本（CDP 键入 + 焦点锚点）；写法：字段=内容，如 密码=abc、月份=2026-05、输入框#2=关键词；class 含 disabled 不挡输入".into();
            changed = true;
        }
    }
    changed
}

/// 将旧版「按」脚本升级到 press-key-v3（焦点锚点 + 通用 DOM 回退）
fn refresh_builtin_press_script(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_press_key" && c.builtin)
    {
        if !cmd.script.contains("press-key-v3")
            || !cmd.script.contains("structural-target-v19")
            || cmd.script.contains("click-text-ranking-v1")
        {
            cmd.script = super::scripts::compose_script(super::scripts::PRESS_KEY);
            cmd.description =
                "CDP 级按键：按回车/Tab/Esc/空格/方向键；继承上一步输入焦点；支持 按回车到输入框#1".into();
            changed = true;
        }
    }
    changed
}

/// 将旧版固定 sleep「等待」升级为 wait-smart-v7 可编辑脚本
fn refresh_builtin_wait_script(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_wait" && c.builtin)
    {
        if cmd.runner != RunnerKind::JavaScript {
            cmd.runner = RunnerKind::JavaScript;
            changed = true;
        }
        if !cmd.script.contains("wait-smart-v7")
            || !cmd.script.contains("structural-target-v19")
            || cmd.script.contains("click-text-ranking-v1")
        {
            cmd.script = super::scripts::compose_script(super::scripts::WAIT_PAGE);
            cmd.description =
                "智能等待：2 | 文字登录10 | 选择器#btn15 | 按钮#2 10 | 加载10 | 下载（默认200）；文字不要求滚进视口".into();
            changed = true;
        }
    }
    changed
}

/// 将已保存的旧版「登录」内置脚本升级到 login-system-v7
fn refresh_builtin_login_script(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_login" && c.builtin)
    {
        if !cmd.script.contains("login-system-v7") {
            cmd.script = super::scripts::LOGIN_SYSTEM.to_string();
            changed = true;
        }
        if cmd.runner != RunnerKind::RustLogin {
            cmd.runner = RunnerKind::RustLogin;
            changed = true;
        }
        let want_desc =
            "关闭当前标签后按网站账号登录（释放旧页渲染进程）；已登录直达业务页时跳过填表与校验。失败则中断。如：登录EHR系统、登录oa";
        if cmd.description != want_desc {
            cmd.description = want_desc.into();
            changed = true;
        }
    }
    changed
}

/// 确保内置「脚本」指令使用 NamedScript 运行方式
fn refresh_builtin_named_script(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_script" && c.builtin)
    {
        if cmd.runner != RunnerKind::NamedScript {
            cmd.runner = RunnerKind::NamedScript;
            changed = true;
        }
        if cmd.keyword != "脚本" {
            cmd.keyword = "脚本".into();
            changed = true;
        }
        if !cmd.description.contains("脚本合并考勤数据") {
            cmd.description = "运行设置→脚本中的命名脚本，如：脚本合并考勤数据".into();
            changed = true;
        }
    }
    changed
}

/// 确保内置「断言」指令使用 NamedAssert
fn refresh_builtin_named_assert(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_assert" && c.builtin)
    {
        if cmd.runner != RunnerKind::NamedAssert {
            cmd.runner = RunnerKind::NamedAssert;
            changed = true;
        }
        if cmd.keyword != "断言" {
            cmd.keyword = "断言".into();
            changed = true;
        }
        let want = "运行设置→断言脚本中的命名脚本（浏览器 XPath/URL/标题），如：断言检查页面标题 登录；失败不刷新页面重试";
        if cmd.description != want {
            cmd.description = want.into();
            changed = true;
        }
    }
    changed
}

fn refresh_builtin_upload_desc(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_upload" && c.builtin)
    {
        if cmd.runner != RunnerKind::RustUpload {
            cmd.runner = RunnerKind::RustUpload;
            changed = true;
        }
        let want =
            "把 files/、downloads/ 附件或本机模板写入页面文件框。写法同「等待下载」：先点入口再上传。例：点击上传，点击上传文件，上传（单文件可省略名）；上传 考勤 / 上传 模板名 / 上传 月报#2。含本指令时执行全程屏蔽系统选文件窗口";
        if cmd.description != want {
            cmd.description = want.into();
            changed = true;
        }
    }
    changed
}

/// 旧版把飞书多维表/云盘 stub 存成了 java_script，需改回专用 runner，否则会报「未知 delegate」
fn refresh_builtin_feishu_runners(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    for cmd in commands.iter_mut().filter(|c| c.builtin) {
        let want_runner = match cmd.id.as_str() {
            "builtin_feishu_bitable_create"
            | "builtin_feishu_bitable_update"
            | "builtin_feishu_bitable_delete"
            | "builtin_feishu_bitable_query"
            | "builtin_feishu_bitable_sync" => Some(RunnerKind::FeishuBitable),
            "builtin_feishu_drive_upload" => Some(RunnerKind::FeishuDrive),
            "builtin_feishu_push_message"
            | "builtin_feishu_push_attachment"
            | "builtin_feishu_push_ai" => Some(RunnerKind::FeishuPush),
            _ => None,
        };
        if let Some(want) = want_runner {
            if cmd.runner != want {
                cmd.runner = want;
                changed = true;
            }
        }
        let want_desc = match cmd.id.as_str() {
            "builtin_feishu_bitable_create" => Some(
                "向浏览器当前打开的多维表格新增记录。字段可用 {{参数}} / {{页签地址}}。例：参数公司=科技事业部，飞书新增 项目名称={{公司}} 资料存放链接={{页签地址}}（空格或逗号分隔；可选 @别名）",
            ),
            "builtin_feishu_bitable_update" => Some(
                "按唯一键更新或新增当前页多维表记录；字段同样支持 {{参数}} / {{页签地址}}；需在登记项配置唯一键，或 @别名 引用已有唯一键",
            ),
            _ => None,
        };
        if let Some(want) = want_desc {
            if cmd.description != want {
                cmd.description = want.into();
                changed = true;
            }
        }
    }
    changed
}

fn refresh_builtin_param_desc(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_param" && c.builtin)
    {
        let want = "写入或覆盖菜谱变量（显示在顶部参数栏）。例：参数公司=科技事业部；正文用 {{公司}} 引用。支持动态：参数云盘地址={{页签地址}}（执行时捕获当前页签 URL），也可直接写 输入={{页签地址}} / {{页签标题}} / {{剪切板}}。仅执行期保留、不进顶部栏请用「动态参数」";
        if cmd.description != want {
            cmd.description = want.into();
            changed = true;
        }
    }
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_dynamic_param" && c.builtin)
    {
        let want = "执行过程中写入/覆盖变量，供 {{名称}} 引用，但不回写顶部参数栏。例：动态参数云盘地址={{页签地址}}，输入={{云盘地址}}";
        if cmd.description != want {
            cmd.description = want.into();
            changed = true;
        }
    }
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_open" && c.builtin)
    {
        let want = "关闭当前标签并新建标签打开 URL（释放旧页渲染进程）";
        if cmd.description != want {
            cmd.description = want.into();
            changed = true;
        }
    }
    changed
}

/// 「人机协同」→「人机验证」关键词迁移
fn refresh_builtin_human_assist(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_human_assist" && c.builtin)
    {
        let want_name = "人机验证";
        let want_kw = "人机验证";
        let want_desc = "暂停并置前浏览器，等人完成滑块/勾选等（登录按钮常因此置灰）；完成后在弹窗点「我已完成」。可写提示：人机验证 请拖动滑块；可选超时秒：人机验证300 请完成验证";
        if cmd.name != want_name {
            cmd.name = want_name.into();
            changed = true;
        }
        if cmd.keyword != want_kw {
            cmd.keyword = want_kw.into();
            changed = true;
        }
        if cmd.description != want_desc {
            cmd.description = want_desc.into();
            changed = true;
        }
        if cmd.runner != RunnerKind::RustHumanAssist {
            cmd.runner = RunnerKind::RustHumanAssist;
            changed = true;
        }
    }
    changed
}

fn refresh_builtin_collect_desc(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_collect_page" && c.builtin)
    {
        let want =
            "采集当前页可见文字与表格（含同源 iframe 与跨域 iframe；飞书多维表格页写入「表格」）。无参数=全部；可写分类：采集表格、采集按钮、采集标题、采集链接、采集输入框、采集其它文字（可并列：采集表格、按钮）";
        if cmd.description != want {
            cmd.description = want.into();
            changed = true;
        }
    }
    changed
}

fn refresh_builtin_ai_desc(commands: &mut Vec<CommandDef>) -> bool {
    let mut changed = false;
    if let Some(cmd) = commands
        .iter_mut()
        .find(|c| c.id == "builtin_ai_analyze" && c.builtin)
    {
        let want = "基于用户提问，AI 分析本次运行产物中的文本/表格/Word 文件（.md/.log/.txt/.csv/.xlsx/.xls/.docx），结果写入 documents/ 供预览。无参数=自动总结；例：AI 哪些记录异常？、AI 对比表格差异";
        if cmd.description != want {
            cmd.description = want.into();
            changed = true;
        }
    }
    changed
}

fn purge_retired_builtins(commands: &mut Vec<CommandDef>) -> bool {
    let before = commands.len();
    commands.retain(|c| !super::builtin::is_retired_builtin(c));
    commands.len() != before
}

fn finalize_db(mut db: CommandDb) -> Result<CommandDb, String> {
    // 先确保脚本库已从旧指令迁移「合并考勤数据」（需在 purge 前读取 commands.json）
    let _ = crate::scripts::list_scripts();
    let mut changed = purge_retired_builtins(&mut db.commands);
    if db.commands.is_empty() {
        db.commands = super::builtin::default_commands();
        changed = true;
    } else {
        let n = db.commands.len();
        merge_missing_builtins(&mut db.commands);
        if db.commands.len() != n {
            changed = true;
        }
        if refresh_builtin_click_script(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_input_script(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_wait_script(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_press_script(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_login_script(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_named_script(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_named_assert(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_upload_desc(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_feishu_runners(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_param_desc(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_human_assist(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_collect_desc(&mut db.commands) {
            changed = true;
        }
        if refresh_builtin_ai_desc(&mut db.commands) {
            changed = true;
        }
    }
    if changed {
        save_db(&db)?;
    }
    Ok(db)
}

fn load_db() -> Result<CommandDb, String> {
    let path = paths::commands_file();
    if !path.exists() {
        return finalize_db(CommandDb::default());
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;

    match serde_json::from_str::<CommandDb>(&data) {
        Ok(db) => finalize_db(db),
        Err(first_err) => {
            let path = paths::commands_file();
            match load_db_from_json(&data) {
                Ok((db, _)) => {
                    let _ = std::fs::copy(&path, path.with_extension("json.bak"));
                    finalize_db(db)
                }
                Err(repair_err) => {
                    let _ = std::fs::copy(&path, path.with_extension("json.bak"));
                    let db = CommandDb {
                        commands: super::builtin::default_commands(),
                    };
                    eprintln!(
                        "commands.json 已损坏，已恢复内置指令（备份 commands.json.bak）。\
                         原错误: {first_err}；迁移失败: {repair_err}"
                    );
                    finalize_db(db)
                }
            }
        }
    }
}

fn normalize_command(cmd: &mut CommandDef) {
    if cmd.kind == CommandKind::Macro {
        cmd.runner = RunnerKind::JavaScript;
    }
}

fn save_db(db: &CommandDb) -> Result<(), String> {
    std::fs::create_dir_all(paths::app_data_dir()).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    std::fs::write(paths::commands_file(), data).map_err(|e| e.to_string())
}

pub fn list_commands() -> Result<Vec<CommandDef>, String> {
    let mut db = load_db()?;
    db.commands.sort_by(|a, b| a.keyword.cmp(&b.keyword));
    Ok(db.commands)
}

pub fn get_command(id: &str) -> Result<Option<CommandDef>, String> {
    Ok(list_commands()?.into_iter().find(|c| c.id == id))
}

pub fn get_by_keyword(keyword: &str) -> Result<Option<CommandDef>, String> {
    Ok(list_commands()?.into_iter().find(|c| c.keyword == keyword))
}

pub fn save_command(mut cmd: CommandDef) -> Result<CommandDef, String> {
    if super::builtin::is_retired_builtin(&cmd) {
        return Err("内置宏「关闭弹窗」已移除，无法保存".into());
    }
    if cmd.keyword.trim().is_empty() {
        return Err("指令关键词不能为空".into());
    }
    cmd.keyword = cmd.keyword.trim().to_string();
    normalize_command(&mut cmd);
    cmd.updated_at = Utc::now();

    let mut db = load_db()?;
    if db.commands.is_empty() {
        db.commands = super::builtin::default_commands();
    }

    if let Some(existing) = db.commands.iter_mut().find(|c| c.id == cmd.id) {
        if existing.builtin && cmd.kind != existing.kind {
            return Err("内置指令类型不可修改".into());
        }
        *existing = cmd.clone();
    } else {
        if db.commands.iter().any(|c| c.keyword == cmd.keyword) {
            return Err(format!("关键词「{}」已存在", cmd.keyword));
        }
        if cmd.id.is_empty() {
            cmd.id = Uuid::new_v4().to_string();
        }
        db.commands.push(cmd.clone());
    }
    save_db(&db)?;
    Ok(cmd)
}

pub fn delete_command(id: &str) -> Result<(), String> {
    let mut db = load_db()?;
    let cmd = db
        .commands
        .iter()
        .find(|c| c.id == id)
        .ok_or("指令不存在")?;
    if cmd.builtin {
        return Err("内置指令不可删除，可编辑脚本后保存".into());
    }
    db.commands.retain(|c| c.id != id);
    save_db(&db)
}

pub fn reset_builtin_script(id: &str) -> Result<CommandDef, String> {
    let defaults = super::builtin::default_commands();
    let def = defaults
        .into_iter()
        .find(|c| c.id == id)
        .ok_or("非内置指令")?;
    save_command(def)
}

pub fn all_keywords() -> Result<Vec<String>, String> {
    let mut kws: Vec<String> = list_commands()?
        .into_iter()
        .map(|c| c.keyword)
        .collect();
    kws.push("宏".into());
    kws.sort_by(|a, b| b.len().cmp(&a.len()));
    Ok(kws)
}

/// 解析阶段是否允许该关键词无参数（与 runtime 行为一致）
pub fn keyword_allows_empty_args(kw: &str) -> bool {
    if matches!(
        kw,
        "点击" | "等待" | "刷新" | "截图" | "采集" | "AI" | "关闭标签" | "飞书查询" | "上传"
            | "飞书上传" | "人机验证" | "推送附件" | "推送AI"
    ) {
        return true;
    }
    // 解析期展开类指令必须带名称
    if matches!(kw, "技能" | "宏" | "脚本" | "断言") {
        return false;
    }
    let Ok(Some(cmd)) = get_by_keyword(kw) else {
        return false;
    };
    matches!(
        cmd.runner,
        RunnerKind::JavaScript
            | RunnerKind::RustNewTab
            | RunnerKind::RustCloseTab
            | RunnerKind::RustWait
            | RunnerKind::RustReload
            | RunnerKind::Rhai
            | RunnerKind::RustUpload
            | RunnerKind::FeishuPush
            | RunnerKind::RustHumanAssist
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_active_tab_defaults_and_opt_outs() {
        assert!(RunnerKind::JavaScript.follows_active_tab());
        assert!(RunnerKind::RustClick.follows_active_tab());
        assert!(RunnerKind::RustClipboard.follows_active_tab());
        assert!(!RunnerKind::RustWait.follows_active_tab());
        assert!(!RunnerKind::RustTab.follows_active_tab());
        assert!(!RunnerKind::RustNewTab.follows_active_tab());
        assert!(!RunnerKind::NamedScript.follows_active_tab());
        assert!(!RunnerKind::FeishuPush.follows_active_tab());
        assert!(!RunnerKind::FeishuPush.needs_browser());
        assert!(RunnerKind::NamedAssert.follows_active_tab());
        assert!(RunnerKind::NamedAssert.needs_browser());
    }

    #[test]
    fn script_keyword_never_needs_or_opens_browser() {
        assert!(!keyword_needs_browser("脚本"));
        assert!(!step_needs_browser("脚本", "合并考勤数据"));
        assert!(!keyword_opens_browser("脚本"));
        assert!(!step_needs_browser("等待", "2"));
        assert!(!step_needs_browser("等待", "下载60"));
        assert!(!keyword_needs_browser("AI"));
        assert!(!keyword_allows_empty_args("断言"));
        assert!(keyword_needs_browser("断言"));
        assert!(step_needs_browser("断言", "检查页面标题"));
    }

    #[test]
    fn refresh_click_upgrades_from_v63_ranking_v1() {
        let mut commands = vec![CommandDef {
            id: "builtin_click".into(),
            name: "点击".into(),
            keyword: "点击".into(),
            kind: CommandKind::Script,
            runner: RunnerKind::RustClick,
            builtin: true,
            description: "old".into(),
            script: "// click-target-v6.3\n// click-text-ranking-v1\nold".into(),
            macro_body: String::new(),
            updated_at: Utc::now(),
        }];
        assert!(refresh_builtin_click_script(&mut commands));
        let s = &commands[0].script;
        assert!(s.contains("click-target-v6.16"));
        assert!(s.contains("toTopViewportCoords"));
        assert!(s.contains("parseIconClickArgs"));
        assert!(s.contains("formatIconClickCmd"));
        assert!(s.contains("normalizeIconName"));
        assert!(s.contains("extractBareIconToken"));
        assert!(s.contains("extractClassFallbackHint"));
        assert!(s.contains("isLibraryShellClass"));
        assert!(s.contains("isCancelRequested"));
        assert!(s.contains("structural-target-v19"));
        assert!(s.contains("只比最终抽取名"));
        assert!(!s.contains("extractLastClassFallback"));
        assert!(s.contains("click-text-ranking-v6"));
        assert!(s.contains("isOptionLikeElement"));
        assert!(s.contains("getOwnContinuousText"));
        assert!(!refresh_builtin_click_script(&mut commands));
    }

    #[test]
    fn refresh_click_syncs_description_when_script_already_v615() {
        let mut commands = vec![CommandDef {
            id: "builtin_click".into(),
            name: "点击".into(),
            keyword: "点击".into(),
            kind: CommandKind::Script,
            runner: RunnerKind::RustClick,
            builtin: true,
            description: "按可见文字或结构化目标点击".into(),
            script: format!(
                "// click-target-v6.16\ntoTopViewportCoords\n// click-text-ranking-v6\nisOptionLikeElement\nisComboDropdownPanel\ngetOwnContinuousText\ncoordOnly\nparseIconClickArgs\nformatIconClickCmd\nextractBareIconToken\nextractClassFallbackHint\nisLibraryShellClass\nisCancelRequested\nnormalizeIconName\nstructural-target-v19\n只比最终抽取名\n"
            ),
            macro_body: String::new(),
            updated_at: Utc::now(),
        }];
        assert!(refresh_builtin_click_script(&mut commands));
        assert!(commands[0].description.contains("像素坐标"));
        assert!(!refresh_builtin_click_script(&mut commands));
    }

    #[test]
    fn refresh_click_upgrades_from_v65_timeout_bug() {
        let mut commands = vec![CommandDef {
            id: "builtin_click".into(),
            name: "点击".into(),
            keyword: "点击".into(),
            kind: CommandKind::Script,
            runner: RunnerKind::RustClick,
            builtin: true,
            description: "old".into(),
            script: "// click-target-v6.5\n// click-text-ranking-v4\ngetOwnContinuousText\ncoordOnly\nif (ctx.orProbe) {\n  const n = Number(ctx.elementWaitTimeout);\n}".into(),
            macro_body: String::new(),
            updated_at: Utc::now(),
        }];
        assert!(refresh_builtin_click_script(&mut commands));
        assert!(commands[0].script.contains("click-target-v6.16"));
        assert!(commands[0].script.contains("toTopViewportCoords"));
        assert!(commands[0].script.contains("formatIconClickCmd"));
        assert!(commands[0].script.contains("extractClassFallbackHint"));
        assert!(!commands[0].script.contains("extractLastClassFallback"));
        assert!(commands[0].script.contains("isLibraryShellClass"));
        assert!(commands[0].script.contains("isCancelRequested"));
        assert!(commands[0].script.contains("if (n > 0) elementWaitTimeout = n"));
    }

    #[test]
    fn refresh_input_upgrades_to_v14() {
        let mut commands = vec![CommandDef {
            id: "builtin_input".into(),
            name: "输入".into(),
            keyword: "输入".into(),
            kind: CommandKind::Script,
            runner: RunnerKind::RustInput,
            builtin: true,
            description: "old".into(),
            script: "// input-target-v13\nstructural-target-v14\nstructuralIsTrulyDisabled".into(),
            macro_body: String::new(),
            updated_at: Utc::now(),
        }];
        assert!(refresh_builtin_input_script(&mut commands));
        assert!(commands[0].script.contains("input-target-v14"));
        assert!(commands[0].script.contains("isCancelRequested"));
        assert!(commands[0].script.contains("structuralIsTrulyDisabled"));
        assert!(commands[0].script.contains("structural-target-v19"));
        assert!(!refresh_builtin_input_script(&mut commands));
    }
}
