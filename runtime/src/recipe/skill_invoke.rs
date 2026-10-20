//! 「技能」调用：最长名称匹配 + 空格分隔的位置参数；供展开与 UI 填参弹窗使用。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::params::{extract_recipe_params_only, merge_params_into_content, RecipeParamKv};
use super::parser::{normalize_automation_text, parse_one_segment, split_content_pieces, ContentPiece};
use super::store::{find_recipe_by_name, list_recipes, list_skills_cache};

/// 列出可用于最长匹配的技能名称（缓存 + 本地快捷指令）
pub fn list_skill_names() -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in list_skills_cache()? {
        let n = s.name.trim().to_string();
        if n.is_empty() || !seen.insert(n.clone()) {
            continue;
        }
        names.push(n);
    }
    // 本地 recipes.json 损坏时不应挡住云端技能执行（历史上 trailing characters 会导致「技能请假」整段失败）
    if let Ok(recipes) = list_recipes() {
        for r in recipes {
            let n = r.name.trim().to_string();
            if n.is_empty() || !seen.insert(n.clone()) {
                continue;
            }
            names.push(n);
        }
    }
    // 长名优先，便于 longest match
    names.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
    Ok(names)
}

/// 解析「技能」参数段：最长技能名 + 其余按空格拆成位置参数
pub fn resolve_skill_invocation(args: &str) -> Result<(String, Vec<String>), String> {
    let args = args.trim();
    if args.is_empty() {
        return Err("「技能」缺少名称，如：技能申请年假".into());
    }
    let names = list_skill_names()?;
    let mut best: Option<String> = None;
    for name in &names {
        if args == name.as_str() || args.starts_with(&format!("{name} ")) {
            let nlen = name.chars().count();
            if best.as_ref().map(|b| b.chars().count()).unwrap_or(0) < nlen {
                best = Some(name.clone());
            }
        }
    }
    let name = if let Some(n) = best {
        n
    } else {
        // 无登记名命中：整段当作名称（兼容旧写法；无位置参数）
        return Ok((args.to_string(), Vec::new()));
    };
    let rest = args[name.len()..].trim();
    let positional: Vec<String> = if rest.is_empty() {
        Vec::new()
    } else {
        rest.split_whitespace().map(|s| s.to_string()).collect()
    };
    Ok((name, positional))
}

/// 将位置参数按声明顺序写入技能正文中的「参数」步骤
pub fn apply_positional_params_to_skill_content(
    content: &str,
    positional: &[String],
) -> Result<String, String> {
    if positional.is_empty() {
        return Ok(content.to_string());
    }
    let mut params = extract_recipe_params_only(content)?;
    if params.is_empty() {
        return Err(format!(
            "该技能未声明「参数」，但提供了 {} 个参数值（空格分隔）",
            positional.len()
        ));
    }
    if positional.len() > params.len() {
        return Err(format!(
            "参数过多：技能需要 {} 个参数，却提供了 {}",
            params.len(),
            positional.len()
        ));
    }
    for (i, val) in positional.iter().enumerate() {
        params[i].value = val.clone();
    }
    merge_params_into_content(content, &params)
}

fn defaults_for_skill_content(content: &str) -> Result<Vec<RecipeParamKv>, String> {
    let mut params = extract_recipe_params_only(content)?;
    if params.is_empty() {
        return Ok(params);
    }
    let recipe_map: HashMap<String, String> = params
        .iter()
        .map(|p| (p.name.clone(), p.value.clone()))
        .collect();
    let mut public_map = HashMap::new();
    if let Ok(list) = crate::public_params::load_personal_public_params() {
        for p in list {
            let name = p.name.trim();
            if !name.is_empty() {
                public_map.insert(name.to_string(), p.value);
            }
        }
    }
    // 与定时任务一致：公共参数 → 技能「参数」声明（非空覆盖）
    for p in &mut params {
        if let Some(v) = public_map.get(&p.name) {
            p.value = v.clone();
        }
        if let Some(v) = recipe_map.get(&p.name) {
            if !v.trim().is_empty() {
                p.value = v.clone();
            }
        }
    }
    Ok(params)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInvocationInfo {
    pub index: u32,
    pub raw_args: String,
    pub skill_name: String,
    pub positional: Vec<String>,
    pub params: Vec<RecipeParamKv>,
    pub needs_prompt: bool,
}

/// 扫描顶层步骤中的「技能」调用（含试探组两侧），不展开嵌套技能
pub fn list_skill_invocations(content: &str) -> Result<Vec<SkillInvocationInfo>, String> {
    let content = normalize_automation_text(content);
    if !content.contains("技能") {
        return Ok(Vec::new());
    }
    let keywords = crate::commands::all_keywords()?;
    let pieces = split_content_pieces(&content, &keywords)?;
    let mut out = Vec::new();
    let mut index = 0u32;

    let mut visit_body = |body: &str| -> Result<(), String> {
        let segments = super::parser::split_segments(body, &keywords);
        for seg in &segments {
            let step = parse_one_segment(seg, &keywords)?;
            if step.keyword != "技能" {
                continue;
            }
            let raw_args = step.args.trim().to_string();
            let (skill_name, positional) = resolve_skill_invocation(&raw_args)?;
            crate::recipe::ensure_skill_executable(&skill_name)?;
            let recipe = find_recipe_by_name(&skill_name)?.ok_or_else(|| {
                format!(
                    "未找到技能「{skill_name}」。请确认左侧技能列表已同步，且名称与列表完全一致"
                )
            })?;
            let mut params = defaults_for_skill_content(&recipe.content)?;
            for (i, val) in positional.iter().enumerate() {
                if i < params.len() {
                    params[i].value = val.clone();
                }
            }
            let needs_prompt = positional.is_empty() && !params.is_empty();
            out.push(SkillInvocationInfo {
                index,
                raw_args,
                skill_name,
                positional,
                params,
                needs_prompt,
            });
            index += 1;
        }
        Ok(())
    };

    for piece in pieces {
        match piece {
            ContentPiece::Plain(text) => visit_body(&text)?,
            ContentPiece::OrGroup {
                try_body,
                fallback_body,
            } => {
                visit_body(&try_body)?;
                if !fallback_body.trim().is_empty() {
                    visit_body(&fallback_body)?;
                }
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillParamBinding {
    pub index: u32,
    /// 位置参数值（按技能「参数」声明顺序）；空表示不改写该调用
    pub values: Vec<String>,
}

/// 将指定「技能」调用的参数改写为「技能名 值1 值2…」（仅空格分隔）
pub fn bind_skill_invocation_params(
    content: &str,
    bindings: &[SkillParamBinding],
) -> Result<String, String> {
    if bindings.is_empty() {
        return Ok(content.to_string());
    }
    let map: HashMap<u32, Vec<String>> = bindings
        .iter()
        .map(|b| (b.index, b.values.clone()))
        .collect();

    let content = normalize_automation_text(content);
    let keywords = crate::commands::all_keywords()?;
    let pieces = split_content_pieces(&content, &keywords)?;
    let mut index = 0u32;

    let rewrite_body = |body: &str, index: &mut u32| -> Result<String, String> {
        let segments = super::parser::split_segments(body, &keywords);
        let mut out_segs = Vec::new();
        for seg in &segments {
            let step = parse_one_segment(seg, &keywords)?;
            if step.keyword == "技能" {
                if let Some(values) = map.get(index) {
                    let (skill_name, _) = resolve_skill_invocation(step.args.trim())?;
                    let new_args = if values.is_empty() {
                        skill_name
                    } else {
                        format!("{skill_name} {}", values.join(" "))
                    };
                    out_segs.push(format!("技能{new_args}"));
                    *index += 1;
                    continue;
                }
                *index += 1;
            }
            out_segs.push(seg.trim().to_string());
        }
        Ok(out_segs.join("，"))
    };

    let mut parts = Vec::new();
    for piece in pieces {
        match piece {
            ContentPiece::Plain(text) => {
                parts.push(rewrite_body(&text, &mut index)?);
            }
            ContentPiece::OrGroup {
                try_body,
                fallback_body,
            } => {
                let try_r = rewrite_body(&try_body, &mut index)?;
                if fallback_body.trim().is_empty() {
                    parts.push(format!("（{try_r}？）"));
                } else {
                    let fb = rewrite_body(&fallback_body, &mut index)?;
                    parts.push(format!("（{try_r}？{fb}）"));
                }
            }
        }
    }
    Ok(parts.join("，"))
}
