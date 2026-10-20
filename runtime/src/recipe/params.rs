//! 菜谱参数：`参数公司=科技事业部` 声明，正文用 `{{公司}}` 引用。
//! 内置动态占位：`{{页签地址}}` / `{{页签标题}}` / `{{剪切板}}`（执行时读取）。

use crate::commands;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use super::parser::{
    normalize_automation_text, parse_one_segment, split_segments, Step,
};

static VAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{([^{}]+)\}\}").expect("recipe param var regex"));

/// 执行时可用的动态上下文（页签 URL/标题、系统剪切板）
#[derive(Debug, Clone, Copy)]
pub struct RuntimeParamCtx<'a> {
    pub tab_url: &'a str,
    pub tab_title: &'a str,
    pub clipboard: &'a str,
}

/// 内置动态参数名（不可用「参数」覆盖）
pub fn is_builtin_runtime_param(name: &str) -> bool {
    matches!(
        normalize_runtime_param_name(name).as_str(),
        "页签地址" | "页签标题" | "剪切板"
    )
}

fn normalize_runtime_param_name(name: &str) -> String {
    name.trim().to_string()
}

fn lookup_builtin_runtime(name: &str, ctx: &RuntimeParamCtx<'_>) -> Option<String> {
    match normalize_runtime_param_name(name).as_str() {
        "页签地址" => Some(ctx.tab_url.to_string()),
        "页签标题" => Some(ctx.tab_title.to_string()),
        "剪切板" => Some(ctx.clipboard.to_string()),
        _ => None,
    }
}

fn value_has_placeholder(value: &str) -> bool {
    value.contains("{{")
}

/// 收集正文中的 `{{名称}}`（排除带点网站账号、内置动态名）。
pub fn collect_recipe_placeholder_names(content: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if !content.contains("{{") {
        return out;
    }
    for caps in VAR_RE.captures_iter(content) {
        let inner = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if inner.is_empty() || inner.contains('.') || is_builtin_runtime_param(inner) {
            continue;
        }
        out.insert(inner.to_string());
    }
    out
}

/// 解析「参数」步骤参数：`公司=科技事业部`
pub fn parse_param_assignment(args: &str) -> Result<(String, String), String> {
    let args = args.trim();
    if args.is_empty() {
        return Err("「参数」写法应为：参数名称=值，如：参数公司=科技事业部".into());
    }
    let (name, value) = args
        .split_once('=')
        .or_else(|| args.split_once('＝'))
        .ok_or_else(|| "「参数」写法应为：参数名称=值，如：参数公司=科技事业部".to_string())?;
    let name = name.trim().to_string();
    let value = value.trim().to_string();
    if name.is_empty() {
        return Err("参数名称不能为空，如：参数公司=科技事业部".into());
    }
    if name.contains('.') {
        return Err(format!(
            "参数名称「{name}」不能含「.」（带点占位符留给网站账号，如 {{{{ehr.url}}}}）"
        ));
    }
    if is_builtin_runtime_param(&name) {
        return Err(format!(
            "「{name}」是内置动态参数，请直接写 {{{{页签地址}}}} / {{{{页签标题}}}} / {{{{剪切板}}}}，不要用「参数」覆盖"
        ));
    }
    Ok((name, value))
}

fn format_step_segment(step: &Step) -> String {
    let mut args = String::new();
    for c in step.args.chars() {
        match c {
            '\\' => args.push_str("\\\\"),
            ',' => args.push_str("\\,"),
            '，' => args.push_str("\\，"),
            '.' => args.push_str("\\."),
            '。' => args.push_str("\\。"),
            _ => args.push(c),
        }
    }
    format!("{}{}", step.keyword, args)
}

fn keywords_with_param() -> Result<Vec<String>, String> {
    let mut keywords = commands::all_keywords().unwrap_or_default();
    if !keywords.iter().any(|k| k == "参数") {
        keywords.push("参数".into());
    }
    if !keywords.iter().any(|k| k == "动态参数") {
        keywords.push("动态参数".into());
    }
    keywords.sort_by(|a, b| b.len().cmp(&a.len()));
    Ok(keywords)
}

fn is_param_keyword(keyword: &str) -> bool {
    keyword == "参数" || keyword == "动态参数"
}

/// 用已声明参数替换 `{{名称}}`；带点的 `{{ehr.url}}` 留给网站账号替换。
/// `runtime` 为 None 时：内置动态名视为已定义（校验用，占位符原样保留）。
pub fn substitute_recipe_params(
    content: &str,
    params: &HashMap<String, String>,
) -> Result<String, String> {
    substitute_recipe_params_ex(content, params, None)
}

/// 执行期替换：可注入当前页签 URL/标题；参数值若仍含 `{{…}}` 会再展开（最多若干轮）。
pub fn substitute_recipe_params_ex(
    content: &str,
    params: &HashMap<String, String>,
    runtime: Option<&RuntimeParamCtx<'_>>,
) -> Result<String, String> {
    if !content.contains("{{") {
        return Ok(content.to_string());
    }
    let mut current = content.to_string();
    let mut last_missing: Vec<String> = Vec::new();
    for _ in 0..8 {
        if !current.contains("{{") {
            return Ok(current);
        }
        let mut missing: Vec<String> = Vec::new();
        let next = VAR_RE
            .replace_all(&current, |caps: &regex::Captures| {
                let inner = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
                if inner.is_empty() {
                    return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
                }
                if inner.contains('.') {
                    return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
                }
                if is_builtin_runtime_param(inner) {
                    if let Some(ctx) = runtime {
                        if let Some(v) = lookup_builtin_runtime(inner, ctx) {
                            return v;
                        }
                    }
                    // 校验阶段或尚无页签信息：保留占位符
                    return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
                }
                if let Some(v) = params.get(inner) {
                    return v.clone();
                }
                if let Some((_, v)) =
                    params.iter().find(|(k, _)| k.eq_ignore_ascii_case(inner))
                {
                    return v.clone();
                }
                missing.push(inner.to_string());
                caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string()
            })
            .into_owned();
        last_missing = missing;
        if next == current {
            break;
        }
        current = next;
    }
    if !last_missing.is_empty() {
        last_missing.sort();
        last_missing.dedup();
        return Err(format!(
            "未定义参数：{}。请先写 参数名称=值，或在设置→公共参数中配置；内置可用 {{{{页签地址}}}} / {{{{页签标题}}}} / {{{{剪切板}}}}",
            last_missing.join("、")
        ));
    }
    // 仍残留的只能是校验期未展开的内置动态占位或带点网站账号占位
    Ok(current)
}

/// 有序参数条目（供 UI / IPC）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecipeParamKv {
    pub name: String,
    pub value: String,
}

pub fn params_to_ordered_vec(params: &HashMap<String, String>) -> Vec<RecipeParamKv> {
    let mut keys: Vec<_> = params.keys().cloned().collect();
    keys.sort();
    keys.into_iter()
        .map(|name| RecipeParamKv {
            value: params.get(&name).cloned().unwrap_or_default(),
            name,
        })
        .collect()
}

pub fn ordered_vec_to_params(entries: &[RecipeParamKv]) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for e in entries {
        let name = e.name.trim();
        if name.is_empty() {
            return Err("参数名称不能为空".into());
        }
        if name.contains('.') {
            return Err(format!(
                "参数名称「{name}」不能含「.」（带点占位符留给网站账号，如 {{{{ehr.url}}}}）"
            ));
        }
        if is_builtin_runtime_param(name) {
            return Err(format!(
                "「{name}」是内置动态参数，不能写入参数表"
            ));
        }
        if map.contains_key(name) {
            return Err(format!("参数「{name}」重复"));
        }
        map.insert(name.to_string(), e.value.clone());
    }
    Ok(map)
}

/// 去掉正文中的「参数」声明步骤，不校验 `{{占位符}}`（供 UI 回写用）。
/// 「动态参数」保留在正文中，不进入顶部参数栏。
pub fn strip_recipe_param_steps(
    content: &str,
) -> Result<(String, HashMap<String, String>), String> {
    let content = normalize_automation_text(content);
    let mut params = HashMap::new();
    if !content.contains("参数") {
        return Ok((content, params));
    }
    let keywords = keywords_with_param()?;
    let segments = split_segments(&content, &keywords);
    if segments.is_empty() {
        return Ok((String::new(), params));
    }
    let mut kept: Vec<Step> = Vec::new();
    for seg in &segments {
        let step = parse_one_segment(seg, &keywords)?;
        if step.keyword == "参数" {
            let (name, value) = parse_param_assignment(&step.args)?;
            params.insert(name, value);
        } else {
            // 含「动态参数」及其它指令：保留原文
            kept.push(step);
        }
    }
    if kept.is_empty() {
        return Ok((String::new(), params));
    }
    let rejoined = kept
        .iter()
        .map(format_step_segment)
        .collect::<Vec<_>>()
        .join("，");
    Ok((rejoined, params))
}

fn rename_recipe_placeholders(body: &str, from: &str, to: &str) -> String {
    let from = from.trim();
    let to = to.trim();
    if from.is_empty() || to.is_empty() || from == to {
        return body.to_string();
    }
    VAR_RE
        .replace_all(body, |caps: &regex::Captures| {
            let inner = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if inner.eq_ignore_ascii_case(from) {
                format!("{{{{{to}}}}}")
            } else {
                caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string()
            }
        })
        .into_owned()
}

/// 用 UI 参数表重写正文：去掉原有「参数」声明后，按表重新前置。
/// `validate_placeholders` 为 true 时校验正文 `{{名}}` 均已在表中（执行前用）；
/// 编辑同步时可传 false，允许暂存未齐的占位符。
/// `rename_from` / `rename_to`：同步改名时替换正文中的 `{{旧名}}`。
pub fn merge_params_into_content(
    content: &str,
    entries: &[RecipeParamKv],
) -> Result<String, String> {
    merge_params_into_content_ex(content, entries, true, None, None)
}

pub fn merge_params_into_content_ex(
    content: &str,
    entries: &[RecipeParamKv],
    validate_placeholders: bool,
    rename_from: Option<&str>,
    rename_to: Option<&str>,
) -> Result<String, String> {
    let (mut body, _) = strip_recipe_param_steps(content)?;
    if let (Some(from), Some(to)) = (rename_from, rename_to) {
        body = rename_recipe_placeholders(&body, from, to);
    }
    let mut map = ordered_vec_to_params(entries)?;
    crate::public_params::fill_missing_from_public_for(&body, &mut map);
    // 正文保留的「动态参数」参与占位符校验（不进入顶部栏）
    fold_dynamic_param_defs(&body, &mut map)?;
    if validate_placeholders && !body.trim().is_empty() {
        let _ = substitute_recipe_params(&body, &map)?;
    }
    if entries.is_empty() {
        return Ok(body);
    }
    let prefix = entries
        .iter()
        .map(|e| format!("参数{}={}", e.name.trim(), e.value))
        .collect::<Vec<_>>()
        .join("，");
    if body.trim().is_empty() {
        return Ok(prefix);
    }
    Ok(format!("{prefix}，{body}"))
}

/// 把正文中的「动态参数名=值」并入校验用参数表（不改写正文）
fn fold_dynamic_param_defs(
    content: &str,
    map: &mut HashMap<String, String>,
) -> Result<(), String> {
    if !content.contains("动态参数") {
        return Ok(());
    }
    let keywords = keywords_with_param()?;
    let segments = split_segments(content, &keywords);
    for seg in &segments {
        let step = parse_one_segment(seg, &keywords)?;
        if step.keyword != "动态参数" {
            continue;
        }
        let (name, value) = parse_param_assignment(&step.args)?;
        map.insert(name, value);
    }
    Ok(())
}

/// 仅抽取正文中的 `参数名=值`（不校验占位符），供顶部参数栏展示。
/// 「动态参数」不抽取，避免回写顶部栏。
pub fn extract_recipe_params_only(content: &str) -> Result<Vec<RecipeParamKv>, String> {
    let content = normalize_automation_text(content);
    if !content.contains("参数") {
        return Ok(Vec::new());
    }
    let keywords = keywords_with_param()?;
    let segments = split_segments(&content, &keywords);
    let mut out: Vec<RecipeParamKv> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for seg in &segments {
        let step = parse_one_segment(seg, &keywords)?;
        if step.keyword != "参数" {
            continue;
        }
        let (name, value) = parse_param_assignment(&step.args)?;
        if let Some(&idx) = seen.get(&name) {
            out[idx].value = value;
        } else {
            seen.insert(name.clone(), out.len());
            out.push(RecipeParamKv { name, value });
        }
    }
    Ok(out)
}

/// 抽取参数并校验 `{{名}}` 均已定义；正文保留占位符供执行时绑定。
/// - 「参数」：静态步骤剥离；值含 `{{…}}` 的保留以便执行时捕获；从 runtime_only 移除
/// - 「动态参数」：同样写入执行期参数表，并标记为 runtime_only（不回写顶部栏）
pub fn apply_recipe_params(
    content: &str,
    existing: &HashMap<String, String>,
) -> Result<(String, HashMap<String, String>, HashSet<String>), String> {
    apply_recipe_params_ex(content, existing, &HashSet::new())
}

pub fn apply_recipe_params_ex(
    content: &str,
    existing: &HashMap<String, String>,
    existing_runtime_only: &HashSet<String>,
) -> Result<(String, HashMap<String, String>, HashSet<String>), String> {
    let content = normalize_automation_text(content);
    let mut params = crate::public_params::base_params_with_public(&content, existing);
    let mut runtime_only = existing_runtime_only.clone();

    if !content.contains("参数") {
        // 无声明时仍校验已有参数能否覆盖占位符（内置动态名允许）
        let _ = substitute_recipe_params(&content, &params)?;
        return Ok((content, params, runtime_only));
    }

    let keywords = keywords_with_param()?;
    let segments = split_segments(&content, &keywords);
    if segments.is_empty() {
        return Err("快捷指令内容为空".into());
    }

    let mut kept: Vec<Step> = Vec::new();
    for seg in &segments {
        let step = parse_one_segment(seg, &keywords)?;
        if !is_param_keyword(&step.keyword) {
            kept.push(step);
            continue;
        }
        let (name, value) = parse_param_assignment(&step.args)?;
        let dynamic = value_has_placeholder(&value);
        if step.keyword == "动态参数" {
            runtime_only.insert(name.clone());
        } else {
            runtime_only.remove(&name);
        }
        params.insert(name, value);
        if dynamic {
            // 执行期再展开并写入，便于「参数/动态参数 云盘地址={{页签地址}}」捕获当时 URL
            kept.push(step);
        }
    }

    if kept.is_empty() {
        // 允许仅声明/覆盖参数（如：参数公司=山西阳泉），无其它可执行步骤
        return Ok((String::new(), params, runtime_only));
    }

    let rejoined = kept
        .iter()
        .map(format_step_segment)
        .collect::<Vec<_>>()
        .join("，");
    // 校验占位符，但返回仍含 {{名}} 的正文（晚绑定）；内置 {{页签地址}} 等视为已定义
    let _ = substitute_recipe_params(&rejoined, &params)?;
    Ok((rejoined, params, runtime_only))
}

/// 用当前参数表生成步骤展示文案
pub fn resolve_step_args(args: &str, params: &HashMap<String, String>) -> String {
    resolve_step_args_ex(args, params, None)
}

pub fn resolve_step_args_ex(
    args: &str,
    params: &HashMap<String, String>,
    runtime: Option<&RuntimeParamCtx<'_>>,
) -> String {
    substitute_recipe_params_ex(args, params, runtime).unwrap_or_else(|_| args.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_assignment() {
        let (n, v) = parse_param_assignment("公司=科技事业部").unwrap();
        assert_eq!(n, "公司");
        assert_eq!(v, "科技事业部");
    }

    #[test]
    fn parse_rejects_builtin_name() {
        assert!(parse_param_assignment("页签地址=http://x").is_err());
        assert!(parse_param_assignment("剪切板=abc").is_err());
    }

    #[test]
    fn substitute_simple() {
        let mut m = HashMap::new();
        m.insert("公司".into(), "科技事业部".into());
        m.insert("月份".into(), "2026-06".into());
        let out = substitute_recipe_params("点击{{公司}}，输入月份={{月份}}", &m).unwrap();
        assert_eq!(out, "点击科技事业部，输入月份=2026-06");
    }

    #[test]
    fn substitute_runtime_tab_url() {
        let m = HashMap::new();
        let ctx = RuntimeParamCtx {
            tab_url: "https://feishu.cn/drive/folder/ABC",
            tab_title: "云盘",
            clipboard: "clip-text",
        };
        let out = substitute_recipe_params_ex("输入={{页签地址}}", &m, Some(&ctx)).unwrap();
        assert_eq!(out, "输入=https://feishu.cn/drive/folder/ABC");
        let out2 = substitute_recipe_params_ex("点{{页签标题}}", &m, Some(&ctx)).unwrap();
        assert_eq!(out2, "点云盘");
        let out3 = substitute_recipe_params_ex("输入={{剪切板}}", &m, Some(&ctx)).unwrap();
        assert_eq!(out3, "输入=clip-text");
    }

    #[test]
    fn substitute_nested_param_to_runtime() {
        let mut m = HashMap::new();
        m.insert("云盘地址".into(), "{{页签地址}}".into());
        let ctx = RuntimeParamCtx {
            tab_url: "https://example.com/folder",
            tab_title: "",
            clipboard: "",
        };
        let out = substitute_recipe_params_ex("打开{{云盘地址}}", &m, Some(&ctx)).unwrap();
        assert_eq!(out, "打开https://example.com/folder");
    }

    #[test]
    fn substitute_allows_builtin_without_runtime() {
        let m = HashMap::new();
        let out = substitute_recipe_params("打开{{页签地址}}", &m).unwrap();
        assert_eq!(out, "打开{{页签地址}}");
        let out2 = substitute_recipe_params("输入={{剪切板}}", &m).unwrap();
        assert_eq!(out2, "输入={{剪切板}}");
    }

    #[test]
    fn apply_strips_static_keeps_dynamic() {
        let _ = commands::list_commands();
        let (out, params, runtime_only) = apply_recipe_params(
            "参数公司=科技事业部，参数云盘地址={{页签地址}}，点击{{公司}}，输入={{云盘地址}}",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(params.get("公司").map(String::as_str), Some("科技事业部"));
        assert_eq!(
            params.get("云盘地址").map(String::as_str),
            Some("{{页签地址}}")
        );
        assert!(runtime_only.is_empty());
        assert!(!out.contains("参数公司"));
        assert!(out.contains("参数云盘地址={{页签地址}}"));
        assert!(out.contains("点击{{公司}}"));
    }

    #[test]
    fn apply_dynamic_param_runtime_only_not_in_extract() {
        let _ = commands::list_commands();
        let content = "动态参数云盘地址={{页签地址}}，参数公司=科技，输入={{云盘地址}}，点击{{公司}}";
        let (out, params, runtime_only) =
            apply_recipe_params(content, &HashMap::new()).unwrap();
        assert_eq!(params.get("公司").map(String::as_str), Some("科技"));
        assert_eq!(
            params.get("云盘地址").map(String::as_str),
            Some("{{页签地址}}")
        );
        assert!(runtime_only.contains("云盘地址"));
        assert!(!runtime_only.contains("公司"));
        assert!(out.contains("动态参数云盘地址={{页签地址}}"));
        assert!(!out.contains("参数公司"));

        let bar = extract_recipe_params_only(content).unwrap();
        assert_eq!(bar.len(), 1);
        assert_eq!(bar[0].name, "公司");
        assert!(!bar.iter().any(|p| p.name == "云盘地址"));
    }

    #[test]
    fn apply_strips_param_steps() {
        let _ = commands::list_commands();
        let (out, params, _) = apply_recipe_params(
            "参数公司=科技事业部，参数月份=2026-06，点击{{公司}}，输入月份={{月份}}",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(params.get("公司").map(String::as_str), Some("科技事业部"));
        assert_eq!(params.get("月份").map(String::as_str), Some("2026-06"));
        assert!(!out.contains("参数公司"));
        assert!(out.contains("点击{{公司}}"));
        assert!(out.contains("输入月份={{月份}}"));
        let resolved = substitute_recipe_params(&out, &params).unwrap();
        assert!(resolved.contains("点击科技事业部"));
        assert!(resolved.contains("输入月份=2026-06"));
    }

    #[test]
    fn apply_params_only_ok_and_overwrite() {
        let _ = commands::list_commands();
        let mut existing = HashMap::new();
        existing.insert("公司".into(), "旧值".into());
        let (out, params, _) =
            apply_recipe_params("参数公司=山西阳泉", &existing).unwrap();
        assert!(out.is_empty());
        assert_eq!(params.get("公司").map(String::as_str), Some("山西阳泉"));
    }

    #[test]
    fn merge_keeps_dynamic_param_and_validates_refs() {
        let _ = commands::list_commands();
        let content = "参数公司=科技，动态参数云盘地址={{页签地址}}，输入={{云盘地址}}，点击{{公司}}";
        let out = merge_params_into_content(
            content,
            &[RecipeParamKv {
                name: "公司".into(),
                value: "科技".into(),
            }],
        )
        .unwrap();
        assert!(out.starts_with("参数公司=科技"));
        assert!(out.contains("动态参数云盘地址={{页签地址}}"));
        assert!(out.contains("输入={{云盘地址}}"));
        assert_eq!(out.matches("动态参数").count(), 1);
    }

    #[test]
    fn merge_params_only_content() {
        let _ = commands::list_commands();
        let entries = vec![RecipeParamKv {
            name: "公司".into(),
            value: "山西阳泉".into(),
        }];
        let out = merge_params_into_content("参数公司=山西阳泉", &entries).unwrap();
        assert_eq!(out, "参数公司=山西阳泉");
    }

    #[test]
    fn merge_soft_delete_and_rename() {
        let _ = commands::list_commands();
        let content = "参数公司=科技，参数月份=2026-06，点击{{公司}}，输入={{月份}}";
        let after_del = merge_params_into_content_ex(
            content,
            &[RecipeParamKv {
                name: "月份".into(),
                value: "2026-06".into(),
            }],
            false,
            None,
            None,
        )
        .unwrap();
        assert!(after_del.starts_with("参数月份=2026-06"));
        assert!(!after_del.contains("参数公司="));
        assert!(after_del.contains("点击{{公司}}"));

        let after_empty = merge_params_into_content_ex(&after_del, &[], false, None, None).unwrap();
        assert!(!after_empty.contains("参数"));
        assert!(after_empty.contains("点击{{公司}}"));

        let renamed = merge_params_into_content_ex(
            content,
            &[
                RecipeParamKv {
                    name: "企业".into(),
                    value: "科技".into(),
                },
                RecipeParamKv {
                    name: "月份".into(),
                    value: "2026-06".into(),
                },
            ],
            false,
            Some("公司"),
            Some("企业"),
        )
        .unwrap();
        assert!(renamed.contains("{{企业}}"));
        assert!(!renamed.contains("{{公司}}"));
    }

    #[test]
    fn apply_includes_referenced_public_params() {
        let (out, params, _) = apply_recipe_params("点击{{公司}}", &HashMap::new()).unwrap();
        assert_eq!(params.get("公司").map(String::as_str), Some("山西阳泉"));
        assert_eq!(out, "点击{{公司}}");
        let expanded = substitute_recipe_params("点击{{公司}}", &params).unwrap();
        assert_eq!(expanded, "点击山西阳泉");
    }

    #[test]
    fn apply_skips_unreferenced_public_params() {
        let (_out, params, _) =
            apply_recipe_params("参数月份=2026-06，点击登录", &HashMap::new()).unwrap();
        assert_eq!(params.get("月份").map(String::as_str), Some("2026-06"));
        assert!(!params.contains_key("公司"));
        assert!(!params.contains_key("日期"));
    }

    #[test]
    fn step_param_overrides_public() {
        let (out, params, _) =
            apply_recipe_params("参数公司=临时公司，点击{{公司}}", &HashMap::new()).unwrap();
        assert_eq!(params.get("公司").map(String::as_str), Some("临时公司"));
        assert_eq!(out, "点击{{公司}}");
        let expanded = substitute_recipe_params(&out, &params).unwrap();
        assert_eq!(expanded, "点击临时公司");
    }
}
