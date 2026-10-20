use crate::commands::{self, CommandKind};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static RE_MD_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").expect("md link regex"));
static RE_ANGLE_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<((?:https?://|www\.)[^>]+)>").expect("angle link regex")
});

/// 飞书等渠道会把 URL 变成 [text](url)，执行前还原为纯文本
pub fn normalize_automation_text(raw: &str) -> String {
    let md = RE_MD_LINK.replace_all(raw, |caps: &regex::Captures| {
        caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string()
    });
    RE_ANGLE_LINK.replace_all(&md, "$1").into_owned()
}

/// UI 文案匹配：去掉全部空白并小写（「登 录」→「登录」）
pub fn norm_ui_text(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// 归一化后的子串包含（用于步骤参数是否提及「登录」等）
pub fn norm_ui_contains(haystack: &str, needle: &str) -> bool {
    norm_ui_text(haystack).contains(&norm_ui_text(needle))
}

/// 「点击 …」是否表示**提交登录**（需验证码/滑块/登录成败校验）。
/// 仅精确匹配「登录 / login / signin」（空白已归一），避免「密码登录」等被误判。
pub fn is_login_submit_click_target(args: &str) -> bool {
    matches!(
        norm_ui_text(args).as_str(),
        "登录" | "login" | "signin"
    )
}

/// 括号试探组分支：`(试探？备选)` / `（试探?备选）`
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrBranch {
    /// 问号前：成功则跳过备选；失败不触发页面恢复重试，改走备选
    Try,
    /// 问号后：仅在试探失败时执行
    Fallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrGroupMeta {
    pub id: u32,
    pub branch: OrBranch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub keyword: String,
    pub args: String,
    /// 括号 `（试探？备选）` 展开后的分组元数据
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub or_group: Option<OrGroupMeta>,
}

impl Step {
    pub fn new(keyword: impl Into<String>, args: impl Into<String>) -> Self {
        Self {
            keyword: keyword.into(),
            args: args.into(),
            or_group: None,
        }
    }

    pub fn is_or_try(&self) -> bool {
        matches!(
            self.or_group.as_ref(),
            Some(OrGroupMeta {
                branch: OrBranch::Try,
                ..
            })
        )
    }

    pub fn is_or_fallback(&self) -> bool {
        matches!(
            self.or_group.as_ref(),
            Some(OrGroupMeta {
                branch: OrBranch::Fallback,
                ..
            })
        )
    }
}

pub(crate) enum ContentPiece {
    /// 普通正文（可含步骤分隔符）
    Plain(String),
    /// `（试探？备选）`：问号前后各解析为若干步
    OrGroup { try_body: String, fallback_body: String },
}

pub fn parse_recipe_steps(content: &str) -> Result<Vec<Step>, String> {
    let mut stack = Vec::new();
    parse_recipe_steps_with_stack(content, &mut stack)
}

fn parse_recipe_steps_with_stack(
    content: &str,
    expand_stack: &mut Vec<String>,
) -> Result<Vec<Step>, String> {
    let content = normalize_automation_text(content);
    let keywords = commands::all_keywords()?;
    let pieces = split_content_pieces(&content, &keywords)?;
    let mut steps = Vec::new();
    let mut next_group_id = 1u32;

    for piece in pieces {
        match piece {
            ContentPiece::Plain(text) => {
                steps.extend(parse_plain_body(&text, &keywords)?);
            }
            ContentPiece::OrGroup {
                try_body,
                fallback_body,
            } => {
                let try_steps = parse_plain_body(&try_body, &keywords)?;
                if try_steps.is_empty() {
                    return Err(
                        "括号试探语法错误：问号前不能为空，如：（点击文件夹？）或（点击文件夹？点击新建）"
                            .into(),
                    );
                }
                // 备选可省略：（点击已有项？）仅试探，失败则跳过并继续后续步骤
                let fallback_steps = if fallback_body.trim().is_empty() {
                    Vec::new()
                } else {
                    parse_plain_body(&fallback_body, &keywords)?
                };
                let gid = next_group_id;
                next_group_id += 1;
                for mut s in try_steps {
                    s.or_group = Some(OrGroupMeta {
                        id: gid,
                        branch: OrBranch::Try,
                    });
                    steps.push(s);
                }
                for mut s in fallback_steps {
                    s.or_group = Some(OrGroupMeta {
                        id: gid,
                        branch: OrBranch::Fallback,
                    });
                    steps.push(s);
                }
            }
        }
    }

    if steps.is_empty() {
        return Err("快捷指令内容为空".into());
    }
    expand_composites(steps, expand_stack)
}

fn parse_plain_body(body: &str, keywords: &[String]) -> Result<Vec<Step>, String> {
    let segments = split_segments(body, keywords);
    let mut steps = Vec::new();
    for s in &segments {
        steps.push(parse_one_segment(s, keywords)?);
    }
    Ok(steps)
}

fn is_open_paren(c: char) -> bool {
    c == '(' || c == '（'
}

fn is_close_paren(c: char) -> bool {
    c == ')' || c == '）'
}

fn is_or_separator(c: char) -> bool {
    c == '?' || c == '？'
}

/// 将正文拆成普通片段与括号试探组。
/// 括号内顶层含 `？`/`?`、问号前像指令步骤时识别为试探；备选可省略。
/// 否则保留完整括号原文（如「点击考勤月报（新版）」）。
pub(crate) fn split_content_pieces(content: &str, keywords: &[String]) -> Result<Vec<ContentPiece>, String> {
    let mut pieces = Vec::new();
    let mut plain = String::new();
    let mut chars = content.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if !is_open_paren(c) {
            plain.push(c);
            continue;
        }

        let close_at = find_matching_close(content, i).ok_or_else(|| {
            format!("括号未闭合：从「{c}」起找不到配对的右括号（支持 () 与 （））")
        })?;
        let inner_start = i + c.len_utf8();
        let inner = content[inner_start..close_at].trim();
        let close_c = content[close_at..].chars().next().unwrap_or(')');

        // 跳过到右括号之后
        while let Some(&(j, _)) = chars.peek() {
            if j <= close_at {
                chars.next();
            } else {
                break;
            }
        }

        if let Some((try_body, fallback_body)) = split_or_inner(inner, keywords) {
            // 试探组前的逗号只是组间分隔，不能留在「打开」等步骤参数里
            push_plain_piece(&mut pieces, &mut plain, true);
            pieces.push(ContentPiece::OrGroup {
                try_body,
                fallback_body,
            });
        } else {
            // 非试探：原样保留左右括号与内文
            plain.push(c);
            plain.push_str(&content[inner_start..close_at]);
            plain.push(close_c);
        }
    }

    // 文末不剥尾部分隔符（如 输入密码=abc.）
    push_plain_piece(&mut pieces, &mut plain, false);
    Ok(pieces)
}

fn push_plain_piece(pieces: &mut Vec<ContentPiece>, plain: &mut String, trim_trailing_seps: bool) {
    let mut s = std::mem::take(plain);
    // 组后的前导逗号/空白不是步骤内容
    s = s
        .trim_start_matches(|c: char| c.is_whitespace() || c == '\r' || is_step_separator_char(c))
        .to_string();
    if trim_trailing_seps {
        s = s
            .trim_end_matches(|c: char| c.is_whitespace() || c == '\r' || is_step_separator_char(c))
            .to_string();
    } else {
        s = s
            .trim_end_matches(|c: char| c.is_whitespace() || c == '\r')
            .to_string();
    }
    if plain_has_step_content(&s) {
        pieces.push(ContentPiece::Plain(s));
    }
}

fn plain_has_step_content(s: &str) -> bool {
    s.chars()
        .any(|c| !c.is_whitespace() && c != '\r' && !is_step_separator_char(c))
}

fn find_matching_close(s: &str, open_byte: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s[open_byte..].char_indices() {
        let abs = open_byte + i;
        if is_open_paren(c) {
            depth += 1;
        } else if is_close_paren(c) {
            depth -= 1;
            if depth == 0 {
                return Some(abs);
            }
        }
    }
    None
}

/// 分支正文是否像一步快捷指令（以已注册关键词开头）
fn branch_looks_like_steps(body: &str, keywords: &[String]) -> bool {
    let t = body.trim();
    if t.is_empty() {
        return false;
    }
    // 多步时看第一段
    let first = t
        .split(|c: char| is_step_separator_char(c) || c.is_whitespace())
        .map(str::trim)
        .find(|p| !p.is_empty())
        .unwrap_or(t);
    for kw in keywords {
        if kw == "宏" {
            if first.starts_with("宏:") || first.starts_with("宏：") {
                return true;
            }
            continue;
        }
        if first.starts_with(kw) {
            return true;
        }
    }
    false
}

/// 在括号内文顶层按首个 `？`/`?` 拆成试探 / 备选。
/// 问号前须非空且像指令步骤；问号后可为空（仅试探）；非空时也须像指令步骤。
fn split_or_inner(inner: &str, keywords: &[String]) -> Option<(String, String)> {
    let mut depth = 0i32;
    for (i, c) in inner.char_indices() {
        if is_open_paren(c) {
            depth += 1;
        } else if is_close_paren(c) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && is_or_separator(c) {
            let try_body = inner[..i].trim();
            let fallback_body = inner[i + c.len_utf8()..].trim();
            if try_body.is_empty() || !branch_looks_like_steps(try_body, keywords) {
                return None;
            }
            if !fallback_body.is_empty() && !branch_looks_like_steps(fallback_body, keywords) {
                return None;
            }
            return Some((try_body.to_string(), fallback_body.to_string()));
        }
    }
    None
}

/// 参数内的逗号/句号用 `\,` `\，` `\.` `\。` 表示字面量，不作为步骤分隔符
const COMMA_PLACEHOLDER: char = '\u{E000}';
const PERIOD_PLACEHOLDER: char = '\u{E001}';

fn is_step_separator_char(c: char) -> bool {
    matches!(c, ',' | '，' | '、' | ';' | '；' | '.' | '。' | '\n')
}

/// 去掉步骤参数尾部的分隔符/空白（文末「点击确定，」中的逗号）
fn strip_trailing_step_separators(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_whitespace() || c == '\r' || is_step_separator_char(c))
}

/// 分隔符后（跳过空白）若仍是分隔符或已结束，则该分隔符视为输入内容；否则才拆步
fn following_has_step_content(mut rest: impl Iterator<Item = char>) -> bool {
    while let Some(c) = rest.next() {
        if c == '\r' || c.is_whitespace() {
            continue;
        }
        return !is_step_separator_char(c);
    }
    false
}

fn normalize_recipe_raw(raw: &str) -> String {
    let mut normalized = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    // 括号/方括号内的逗号、句号属于表达式（如 XPath contains(.,'查询')），不拆步
    let mut paren_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == ',' || next == '，' {
                    chars.next();
                    normalized.push(COMMA_PLACEHOLDER);
                    continue;
                }
                if next == '.' || next == '。' {
                    chars.next();
                    normalized.push(PERIOD_PLACEHOLDER);
                    continue;
                }
                if next == '\\' {
                    chars.next();
                    normalized.push('\\');
                    continue;
                }
            }
            normalized.push(c);
            continue;
        }
        match c {
            '(' | '（' => {
                paren_depth += 1;
                normalized.push(c);
            }
            ')' | '）' => {
                paren_depth = paren_depth.saturating_sub(1);
                normalized.push(c);
            }
            '[' => {
                bracket_depth += 1;
                normalized.push(c);
            }
            ']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                normalized.push(c);
            }
            '，' | ',' | '、' | ';' | '；' | '。' => {
                if paren_depth > 0 || bracket_depth > 0 {
                    normalized.push(c);
                } else if following_has_step_content(chars.clone()) {
                    normalized.push('\n');
                } else {
                    // 后面无实质内容（或紧跟另一分隔符）→ 保留为输入
                    normalized.push(c);
                }
            }
            // 英文句号：后接字母/数字时保留（URL、小数、版本号）；
            // 括号/方括号内保留（XPath contains(.,'…')）；后接表达式符号时也保留
            '.' => {
                let keep_as_literal = paren_depth > 0
                    || bracket_depth > 0
                    || matches!(
                        chars.peek(),
                        Some(n) if n.is_ascii_alphanumeric()
                            || matches!(n, ',' | '\'' | '"' | ')' | ']' | '(')
                    )
                    || !following_has_step_content(chars.clone());
                if keep_as_literal {
                    normalized.push('.');
                } else {
                    normalized.push('\n');
                }
            }
            '\r' => {}
            _ => normalized.push(c),
        }
    }
    normalized
}

pub(crate) fn restore_escaped_commas(s: &str) -> String {
    s.replace(COMMA_PLACEHOLDER, ",")
        .replace(PERIOD_PLACEHOLDER, ".")
}

pub(crate) fn split_segments(raw: &str, keywords: &[String]) -> Vec<String> {
    let normalized = normalize_recipe_raw(raw);

    let mut starts: Vec<usize> = Vec::new();
    for (i, _) in normalized.char_indices() {
        for kw in keywords {
            if kw == "宏" {
                if normalized[i..].starts_with("宏:") || normalized[i..].starts_with("宏：") {
                    let prev_ok = i == 0
                        || normalized[..i]
                            .chars()
                            .last()
                            .map(|c| c.is_whitespace() || c == '\n')
                            .unwrap_or(true);
                    if prev_ok {
                        starts.push(i);
                    }
                }
                continue;
            }
            if !normalized[i..].starts_with(kw) {
                continue;
            }
            let prev_ok = if i == 0 {
                true
            } else {
                normalized[..i]
                    .chars()
                    .last()
                    .map(|c| c.is_whitespace() || c == '\n')
                    .unwrap_or(true)
            };
            if prev_ok {
                starts.push(i);
                break;
            }
        }
    }

    starts.sort_unstable();
    starts.dedup();

    let mut out = Vec::new();
    for &start in starts.iter() {
        let end = starts
            .iter()
            .copied()
            .find(|&x| x > start)
            .unwrap_or(normalized.len());
        let seg = restore_escaped_commas(normalized[start..end].trim());
        if !seg.is_empty() {
            out.push(seg);
        }
    }

    if out.is_empty() {
        let t = restore_escaped_commas(normalized.trim());
        if !t.is_empty() {
            out.push(t);
        }
    }
    out
}

pub(crate) fn parse_one_segment(seg: &str, keywords: &[String]) -> Result<Step, String> {
    let seg = seg.trim();

    if let Some(rest) = seg.strip_prefix("宏:").or_else(|| seg.strip_prefix("宏：")) {
        let name = rest.trim();
        if name.is_empty() {
            return Err("宏命令缺少名称，格式：宏:宏名称".into());
        }
        return Ok(Step::new("宏", name));
    }

    for kw in keywords {
        if kw == "宏" {
            continue;
        }
        if seg.starts_with(kw) {
            let mut args = seg[kw.len()..].trim().to_string();
            // 「点击确定，」文末分隔符不是目标文字的一部分
            if kw == "点击" {
                args = strip_trailing_step_separators(&args).to_string();
            }
            if args.is_empty() && !commands::keyword_allows_empty_args(kw) {
                return Err(format!("「{kw}」缺少参数"));
            }
            return Ok(Step::new(kw.clone(), args));
        }
    }

    let avail = commands::list_commands()?
        .iter()
        .map(|c| c.keyword.as_str())
        .collect::<Vec<_>>()
        .join("、");
    Err(format!(
        "无法识别的指令: {seg}。已注册关键词: {avail}、宏:名称、技能名称"
    ))
}

const MAX_COMPOSITE_EXPAND_DEPTH: usize = 16;

fn max_or_group_id(steps: &[Step]) -> u32 {
    steps
        .iter()
        .filter_map(|s| s.or_group.as_ref().map(|g| g.id))
        .max()
        .unwrap_or(0)
}

fn shift_or_group_ids(mut steps: Vec<Step>, offset: u32) -> Vec<Step> {
    if offset == 0 {
        return steps;
    }
    for s in &mut steps {
        if let Some(og) = &mut s.or_group {
            og.id = og.id.saturating_add(offset);
        }
    }
    steps
}

/// 展开「宏:名称」与「技能名称」为子步骤（支持嵌套，检测循环）
fn expand_composites(
    steps: Vec<Step>,
    expand_stack: &mut Vec<String>,
) -> Result<Vec<Step>, String> {
    if expand_stack.len() > MAX_COMPOSITE_EXPAND_DEPTH {
        return Err("技能/宏嵌套过深（超过 16 层）".into());
    }
    let mut out = Vec::new();
    let all = commands::list_commands()?;

    for step in steps {
        match step.keyword.as_str() {
            "宏" => {
                let key = format!("宏:{}", step.args);
                if expand_stack.iter().any(|k| k == &key) {
                    return Err(format!("宏「{}」存在循环引用", step.args));
                }
                let macro_def = all
                    .iter()
                    .find(|c| c.kind == CommandKind::Macro && c.name == step.args)
                    .ok_or_else(|| format!("未找到宏「{}」", step.args))?;

                if macro_def.macro_body.trim().is_empty() {
                    return Err(format!("宏「{}」内容为空", step.args));
                }
                expand_stack.push(key);
                let inner = parse_recipe_steps_with_stack(&macro_def.macro_body, expand_stack)?;
                expand_stack.pop();
                let offset = max_or_group_id(&out);
                out.extend(shift_or_group_ids(inner, offset));
            }
            "技能" => {
                let (name, positional) = crate::recipe::resolve_skill_invocation(&step.args)?;
                if name.is_empty() {
                    return Err("「技能」缺少名称，如：技能申请年假".into());
                }
                crate::recipe::ensure_skill_executable(&name)?;
                let key = format!("技能:{name}");
                if expand_stack.iter().any(|k| k == &key) {
                    return Err(format!("技能「{name}」存在循环引用"));
                }
                let recipe = crate::recipe::find_recipe_by_name(&name)?
                    .ok_or_else(|| {
                        format!(
                            "未找到技能「{name}」。请确认左侧技能列表已同步，且名称与列表完全一致（如：技能请假）"
                        )
                    })?;
                if recipe.content.trim().is_empty() {
                    return Err(format!("技能「{name}」内容为空"));
                }
                let skill_content = crate::recipe::apply_positional_params_to_skill_content(
                    &recipe.content,
                    &positional,
                )?;
                expand_stack.push(key);
                let inner = parse_recipe_steps_with_stack(&skill_content, expand_stack)?;
                expand_stack.pop();
                let offset = max_or_group_id(&out);
                out.extend(shift_or_group_ids(inner, offset));
            }
            _ => out.push(step),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_period_as_separator() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("点击登录。等待2。截图").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].keyword, "点击");
        assert_eq!(steps[0].args, "登录");
        assert_eq!(steps[1].keyword, "等待");
        assert_eq!(steps[1].args, "2");
        assert_eq!(steps[2].keyword, "截图");
    }

    #[test]
    fn parse_password_trailing_period_before_comma() {
        let _ = commands::list_commands();
        // `.` 后紧跟 `，`（无实质内容）→ 句号属于密码；`，` 后有「点击」→ 逗号才是分隔
        let steps = parse_recipe_steps("输入密码=047960Zhj.，点击登录").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "输入");
        assert_eq!(steps[0].args, "密码=047960Zhj.");
        assert_eq!(steps[1].keyword, "点击");
        assert_eq!(steps[1].args, "登录");
    }

    #[test]
    fn xpath_contains_dot_not_split_as_step() {
        let _ = commands::list_commands();
        // XPath contains(.,'…') 中的 `.` 不能当成步骤分隔符
        let steps =
            parse_recipe_steps("断言元素存在 //button[contains(.,'查询')]").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].keyword, "断言");
        assert_eq!(steps[0].args, "元素存在 //button[contains(.,'查询')]");
    }

    #[test]
    fn parse_trailing_separator_kept_as_input() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("输入密码=abc.").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].args, "密码=abc.");
    }

    #[test]
    fn parse_english_period_keeps_url_dots() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("打开https://example.com/login.等待2").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].args, "https://example.com/login");
        assert_eq!(steps[1].keyword, "等待");
    }

    #[test]
    fn parse_escaped_period_in_password() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps(r"输入密码=abc\.，点击登录").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].args, "密码=abc.");
    }

    #[test]
    fn parse_with_builtin_verbs() {
        let _ = commands::list_commands();
        let content = "打开https://example.com ，点击登录，等待2";
        let steps = parse_recipe_steps(content).unwrap();
        assert!(steps.len() >= 3);
    }

    #[test]
    fn parse_escaped_comma_in_args() {
        let _ = commands::list_commands();
        let content = "输入hello\\,world到手机，点击登录";
        let steps = parse_recipe_steps(content).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "输入");
        assert_eq!(steps[0].args, "hello,world到手机");
        assert_eq!(steps[1].keyword, "点击");
        assert_eq!(steps[1].args, "登录");
    }

    #[test]
    fn parse_escaped_comma_in_url() {
        let _ = commands::list_commands();
        let content = "打开https://example.com?a=1\\,b=2，等待1";
        let steps = parse_recipe_steps(content).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].args, "https://example.com?a=1,b=2");
    }

    #[test]
    fn parse_screenshot_without_args() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("打开https://example.com，截图，点击登录").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[1].keyword, "截图");
        assert_eq!(steps[1].args, "");
    }

    #[test]
    fn parse_collect_without_args() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("刷新，采集").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "刷新");
        assert_eq!(steps[1].keyword, "采集");
        assert_eq!(steps[1].args, "");
    }

    #[test]
    fn parse_collect_with_category_arg() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("刷新，采集表格，点击导出").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[1].keyword, "采集");
        assert_eq!(steps[1].args, "表格");
    }

    #[test]
    fn parse_close_tab_without_args() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("打开标签https://example.com，关闭标签").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "打开标签");
        assert_eq!(steps[1].keyword, "关闭标签");
        assert_eq!(steps[1].args, "");
    }

    #[test]
    fn parse_reload_without_args() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("打开https://example.com，刷新，点击登录").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[1].keyword, "刷新");
        assert_eq!(steps[1].args, "");
    }

    #[test]
    fn parse_script_keyword_without_args() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("滚动").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].keyword, "滚动");
        assert_eq!(steps[0].args, "");
    }

    #[test]
    fn parse_click_with_match_index() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("点击考勤组织#2，点击确定").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "点击");
        assert_eq!(steps[0].args, "考勤组织#2");
        assert_eq!(steps[1].args, "确定");
    }

    #[test]
    fn click_strips_trailing_separators_in_args() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("点击确定，").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].keyword, "点击");
        assert_eq!(steps[0].args, "确定");

        let steps = parse_recipe_steps("点击确定。").unwrap();
        assert_eq!(steps[0].args, "确定");

        let steps = parse_recipe_steps("点击按钮#2，").unwrap();
        assert_eq!(steps[0].args, "按钮#2");

        let steps = parse_recipe_steps("点击确定#2，").unwrap();
        assert_eq!(steps[0].args, "确定#2");
    }

    #[test]
    fn parse_click_pixel_coords_and_empty() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("点击 120 80，点击").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "点击");
        assert_eq!(steps[0].args, "120 80");
        assert_eq!(steps[1].keyword, "点击");
        assert_eq!(steps[1].args, "");
    }

    #[test]
    fn parse_click_with_escaped_hash() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps(r"点击订单\#2024").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].args, r"订单\#2024");
    }

    #[test]
    fn parse_structural_click_target() {
        let _ = commands::list_commands();
        let steps =
            parse_recipe_steps("点击表格#1行#1图标按钮#2，点击确定").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].keyword, "点击");
        assert_eq!(steps[0].args, "表格#1行#1图标按钮#2");
        let cell_steps = parse_recipe_steps("点击表格#4行#1列#4，点击确定").unwrap();
        assert_eq!(cell_steps[0].args, "表格#4行#1列#4");
    }

    #[test]
    fn parse_input_field_equals_syntax() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("输入密码=abc，输入输入框#2=hello，按回车").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].args, "密码=abc");
        assert_eq!(steps[1].args, "输入框#2=hello");
    }

    #[test]
    fn parse_structural_input_and_press() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("输入输入框#2=hello，按回车").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].args, "输入框#2=hello");
        assert_eq!(steps[1].keyword, "按");
        assert_eq!(steps[1].args, "回车");
    }

    #[test]
    fn parse_input_legacy_dao_syntax() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("输入hello到输入框#2，按回车").unwrap();
        assert_eq!(steps[0].args, "hello到输入框#2");
    }

    #[test]
    fn norm_ui_text_collapses_whitespace() {
        assert_eq!(norm_ui_text("登 录"), "登录");
        assert_eq!(norm_ui_text("  Sign  In  "), "signin");
        assert!(norm_ui_contains("点击登 录", "登录"));
    }

    #[test]
    fn login_submit_click_only_exact_login() {
        assert!(is_login_submit_click_target("登录"));
        assert!(is_login_submit_click_target("登 录"));
        assert!(is_login_submit_click_target("Login"));
        assert!(is_login_submit_click_target("sign in"));
        assert!(!is_login_submit_click_target("密码登录"));
        assert!(!is_login_submit_click_target("扫描登录"));
        assert!(!is_login_submit_click_target("立即登录"));
        assert!(!is_login_submit_click_target("登录按钮"));
    }

    #[test]
    fn parse_press_key_with_explicit_target() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("按回车到输入框#1").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].keyword, "按");
        assert_eq!(steps[0].args, "回车到输入框#1");
    }

    #[test]
    fn parse_or_group_chinese_parens_and_question() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps(
            "（点击2026-06？点击新建，点击子文件夹，输入名称=2026-06，点击创建），点击上传",
        )
        .unwrap();
        assert_eq!(steps.len(), 6);
        assert!(steps[0].is_or_try());
        assert_eq!(steps[0].keyword, "点击");
        assert_eq!(steps[0].args, "2026-06");
        assert_eq!(steps[0].or_group.as_ref().unwrap().id, 1);

        assert!(steps[1].is_or_fallback());
        assert_eq!(steps[1].args, "新建");
        assert!(steps[2].is_or_fallback());
        assert_eq!(steps[2].args, "子文件夹");
        assert!(steps[3].is_or_fallback());
        assert_eq!(steps[3].args, "名称=2026-06");
        assert!(steps[4].is_or_fallback());
        assert_eq!(steps[4].args, "创建");
        assert_eq!(
            steps[1].or_group.as_ref().unwrap().id,
            steps[0].or_group.as_ref().unwrap().id
        );

        assert!(steps[5].or_group.is_none());
        assert_eq!(steps[5].keyword, "点击");
        assert_eq!(steps[5].args, "上传");
    }

    #[test]
    fn parse_open_before_or_group_strips_separator_from_url() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps(
            "打开https://example.com/path，（点击2026-06？点击新建，点击创建），点击上传",
        )
        .unwrap();
        assert_eq!(steps[0].keyword, "打开");
        assert_eq!(steps[0].args, "https://example.com/path");
        assert!(!steps[0].args.contains('，'));
        assert!(!steps[0].args.contains(','));
        assert!(steps[1].is_or_try());
        assert_eq!(steps[1].args, "2026-06");
        assert_eq!(steps.last().unwrap().args, "上传");
    }

    #[test]
    fn parse_or_group_english_symbols() {
        let _ = commands::list_commands();
        let steps =
            parse_recipe_steps("(点击科技事业部?点击新建，点击创建)，点击上传").unwrap();
        assert_eq!(steps.len(), 4);
        assert!(steps[0].is_or_try());
        assert_eq!(steps[0].args, "科技事业部");
        assert!(steps[1].is_or_fallback());
        assert_eq!(steps[1].args, "新建");
        assert!(steps[2].is_or_fallback());
        assert_eq!(steps[2].args, "创建");
        assert!(steps[3].or_group.is_none());
        assert_eq!(steps[3].args, "上传");
    }

    #[test]
    fn parse_or_group_mixed_symbols() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("（点击A?点击B）").unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps[0].is_or_try());
        assert!(steps[1].is_or_fallback());
    }

    #[test]
    fn parse_two_or_groups() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps(
            "（点击2026-06？点击新建），（点击科技事业部？点击新建），点击上传",
        )
        .unwrap();
        assert_eq!(steps.len(), 5);
        assert_eq!(steps[0].or_group.as_ref().unwrap().id, 1);
        assert_eq!(steps[1].or_group.as_ref().unwrap().id, 1);
        assert_eq!(steps[2].or_group.as_ref().unwrap().id, 2);
        assert_eq!(steps[3].or_group.as_ref().unwrap().id, 2);
        assert!(steps[4].or_group.is_none());
    }

    #[test]
    fn parse_parens_without_question_keep_literal() {
        let _ = commands::list_commands();
        // 普通括号是点击文案的一部分，必须保留
        let steps =
            parse_recipe_steps("点击时间管理，点击考勤月报（新版），点击图标按钮#10").unwrap();
        assert_eq!(steps.len(), 3);
        assert!(steps.iter().all(|s| s.or_group.is_none()));
        assert_eq!(steps[0].keyword, "点击");
        assert_eq!(steps[0].args, "时间管理");
        assert_eq!(steps[1].args, "考勤月报（新版）");
        assert_eq!(steps[2].args, "图标按钮#10");
    }

    #[test]
    fn parse_parens_without_question_are_not_or_group() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("点击登录（SSO），等待2").unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps[0].or_group.is_none());
        assert_eq!(steps[0].args, "登录（SSO）");
        assert_eq!(steps[1].keyword, "等待");
    }

    #[test]
    fn parse_incomplete_question_parens_not_or_group() {
        let _ = commands::list_commands();
        // 问号缺上下部分 → 不当试探，保留原文
        let steps = parse_recipe_steps("点击考勤（新？版）").unwrap();
        assert_eq!(steps.len(), 1);
        assert!(steps[0].or_group.is_none());
        assert_eq!(steps[0].args, "考勤（新？版）");
    }

    #[test]
    fn parse_or_group_empty_try_not_or_group() {
        let _ = commands::list_commands();
        // 仅问号后有内容：不成试探，整段保留为普通点击文案
        let steps = parse_recipe_steps("点击（？备选）").unwrap();
        assert_eq!(steps.len(), 1);
        assert!(steps[0].or_group.is_none());
        assert_eq!(steps[0].args, "（？备选）");
    }

    #[test]
    fn parse_or_group_try_only_without_fallback() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("（点击可选按钮？），点击下一步").unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps[0].is_or_try());
        assert_eq!(steps[0].args, "可选按钮");
        assert!(steps[1].or_group.is_none());
        assert_eq!(steps[1].keyword, "点击");
        assert_eq!(steps[1].args, "下一步");
    }

    #[test]
    fn parse_or_group_try_only_english() {
        let _ = commands::list_commands();
        let steps = parse_recipe_steps("(点击A?)，点击B").unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps[0].is_or_try());
        assert_eq!(steps[0].args, "A");
        assert!(steps[1].or_group.is_none());
        assert_eq!(steps[1].args, "B");
    }

    #[test]
    fn skill_missing_name_errors() {
        let _ = commands::list_commands();
        let err = parse_recipe_steps("技能").unwrap_err();
        assert!(err.contains("缺少") || err.contains("技能"));
    }

    #[test]
    fn skill_not_found_errors() {
        let _ = commands::list_commands();
        let err = parse_recipe_steps("技能__不存在的技能名_xyz_9f3a__").unwrap_err();
        assert!(err.contains("未找到技能"), "got: {err}");
        assert!(err.contains("左侧技能列表"), "got: {err}");
    }

    #[test]
    fn expand_skill_and_combine_with_other_steps() {
        let _ = commands::list_commands();
        let unique = format!(
            "__skill_expand_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let saved = crate::recipe::save_recipe(None, unique.clone(), "刷新，截图".into())
            .expect("save skill");
        let content = format!("技能{unique}，等待2");
        let result = parse_recipe_steps(&content);
        let _ = crate::recipe::delete_recipe(&saved.id);
        let steps = result.expect("parse skill expand");
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].keyword, "刷新");
        assert_eq!(steps[1].keyword, "截图");
        assert_eq!(steps[2].keyword, "等待");
        assert_eq!(steps[2].args, "2");
    }

    #[test]
    fn expand_skill_with_positional_params() {
        let _ = commands::list_commands();
        let unique = format!(
            "__skill_pos_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let body = "参数姓名=默认名，参数天数=1，刷新";
        let saved = crate::recipe::save_recipe(None, unique.clone(), body.into()).expect("save");
        let content = format!("技能{unique} 张三 5");
        let result = parse_recipe_steps(&content);
        let _ = crate::recipe::delete_recipe(&saved.id);
        let steps = result.expect("parse skill with positional");
        // 参数步骤在执行期处理；展开后应含刷新；参数已被写入正文后仍会作为步骤或已合并
        assert!(
            steps.iter().any(|s| s.keyword == "刷新"),
            "steps={:?}",
            steps.iter().map(|s| &s.keyword).collect::<Vec<_>>()
        );
    }

    #[test]
    fn expand_skill_from_skills_cache() {
        let _ = commands::list_commands();
        let unique = format!(
            "__skill_cache_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        crate::recipe::with_skills_cache_exclusive(|| {
            crate::recipe::replace_skills_cache(vec![crate::recipe::SkillCacheEntry {
                id: "cache-1".into(),
                name: unique.clone(),
                content: "刷新，截图".into(),
                status: Some(1),
            }])
            .expect("write skills cache");
            let content = format!("技能{unique}，等待2");
            let steps = parse_recipe_steps(&content).expect("parse from skills cache");
            assert_eq!(steps.len(), 3);
            assert_eq!(steps[0].keyword, "刷新");
            assert_eq!(steps[1].keyword, "截图");
            assert_eq!(steps[2].keyword, "等待");
            let _ = crate::recipe::replace_skills_cache(vec![]);
        });
    }

    #[test]
    fn reject_disabled_skill_from_skills_cache() {
        let _ = commands::list_commands();
        let unique = format!(
            "__skill_disabled_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        crate::recipe::with_skills_cache_exclusive(|| {
            crate::recipe::replace_skills_cache(vec![crate::recipe::SkillCacheEntry {
                id: "cache-disabled".into(),
                name: unique.clone(),
                content: "刷新".into(),
                status: Some(0),
            }])
            .expect("write skills cache");
            let content = format!("技能{unique}");
            let err = parse_recipe_steps(&content).expect_err("disabled skill must fail");
            assert!(
                err.contains(&format!("目前技能{unique}被禁止操作")),
                "unexpected err: {err}"
            );
            assert!(err.contains("联系公司管理员"), "unexpected err: {err}");
            let _ = crate::recipe::replace_skills_cache(vec![]);
        });
    }
}
