//! 公共参数：个人预设（持久化）+ 内置动态占位说明。
//! 执行优先级：步骤/顶部栏「参数」> 公共个人参数 > 内置动态（页签地址等，执行时读取）。

use crate::recipe::{is_builtin_runtime_param, RecipeParamKv};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicParamBuiltin {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicParamsCatalog {
    pub builtin: Vec<PublicParamBuiltin>,
    pub personal: Vec<RecipeParamKv>,
}

fn default_personal() -> Vec<RecipeParamKv> {
    vec![
        RecipeParamKv {
            name: "公司".into(),
            value: "山西阳泉".into(),
        },
        RecipeParamKv {
            name: "日期".into(),
            value: "2026-07".into(),
        },
    ]
}

pub fn builtin_public_params() -> Vec<PublicParamBuiltin> {
    vec![
        PublicParamBuiltin {
            name: "页签地址".into(),
            description: "执行该步时当前浏览器页签 URL".into(),
        },
        PublicParamBuiltin {
            name: "页签标题".into(),
            description: "执行该步时当前浏览器页签标题".into(),
        },
        PublicParamBuiltin {
            name: "剪切板".into(),
            description: "执行该步时系统剪切板文本".into(),
        },
    ]
}

pub fn public_params_file() -> PathBuf {
    crate::paths::public_params_file()
}

pub fn load_personal_public_params() -> Result<Vec<RecipeParamKv>, String> {
    let path = public_params_file();
    if !path.is_file() {
        let defaults = default_personal();
        save_personal_public_params(&defaults)?;
        return Ok(defaults);
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("读取公共参数失败: {e}"))?;
    if raw.trim().is_empty() {
        let defaults = default_personal();
        save_personal_public_params(&defaults)?;
        return Ok(defaults);
    }
    let list: Vec<RecipeParamKv> =
        serde_json::from_str(&raw).map_err(|e| format!("解析公共参数失败: {e}"))?;
    Ok(list)
}

pub fn save_personal_public_params(entries: &[RecipeParamKv]) -> Result<(), String> {
    validate_personal_public_params(entries)?;
    let path = public_params_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("保存公共参数失败: {e}"))
}

pub fn validate_personal_public_params(entries: &[RecipeParamKv]) -> Result<(), String> {
    let mut seen = HashMap::new();
    for e in entries {
        let name = e.name.trim();
        if name.is_empty() {
            return Err("公共参数名称不能为空".into());
        }
        if name.contains('.') {
            return Err(format!(
                "公共参数「{name}」不能含「.」（带点占位符留给网站账号）"
            ));
        }
        if is_builtin_runtime_param(name) {
            return Err(format!(
                "「{name}」是内置动态参数，请在指令中直接写 {{{{{name}}}}}，勿加入个人公共参数"
            ));
        }
        if seen.insert(name.to_string(), ()).is_some() {
            return Err(format!("公共参数名称重复：{name}"));
        }
    }
    Ok(())
}

pub fn personal_public_params_map() -> HashMap<String, String> {
    load_personal_public_params()
        .unwrap_or_else(|_| default_personal())
        .into_iter()
        .map(|e| (e.name.trim().to_string(), e.value))
        .collect()
}

/// 在已有参数表上补全「正文实际引用到的」公共个人参数（不覆盖已有键）。
pub fn fill_missing_from_public_for(content: &str, map: &mut HashMap<String, String>) {
    let needed = crate::recipe::collect_recipe_placeholder_names(content);
    if needed.is_empty() {
        return;
    }
    for (k, v) in personal_public_params_map() {
        if needed.contains(&k) {
            map.entry(k).or_insert(v);
        }
    }
}

/// 执行前基础参数：仅注入本菜谱 `{{…}}` 用到的公共个人参数；`existing`（CLI/任务/远程）始终并入并优先。
pub fn base_params_with_public(
    content: &str,
    existing: &HashMap<String, String>,
) -> HashMap<String, String> {
    let needed = crate::recipe::collect_recipe_placeholder_names(content);
    let mut map = HashMap::new();
    if !needed.is_empty() {
        for (k, v) in personal_public_params_map() {
            if needed.contains(&k) {
                map.insert(k, v);
            }
        }
    }
    for (k, v) in existing {
        map.insert(k.clone(), v.clone());
    }
    map
}

pub fn list_public_params_catalog() -> Result<PublicParamsCatalog, String> {
    Ok(PublicParamsCatalog {
        builtin: builtin_public_params(),
        personal: load_personal_public_params()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_builtin_names() {
        let bad = vec![RecipeParamKv {
            name: "页签地址".into(),
            value: "x".into(),
        }];
        assert!(validate_personal_public_params(&bad).is_err());
    }

    #[test]
    fn base_params_step_overrides_public() {
        let mut existing = HashMap::new();
        existing.insert("公司".into(), "临时".into());
        let base = base_params_with_public("点击{{公司}}", &existing);
        assert_eq!(base.get("公司").map(String::as_str), Some("临时"));
    }

    #[test]
    fn base_params_skips_unreferenced_public() {
        let base = base_params_with_public("点击登录", &HashMap::new());
        assert!(base.is_empty());
    }

    #[test]
    fn base_params_keeps_explicit_existing_even_if_unreferenced() {
        let mut existing = HashMap::new();
        existing.insert("批次".into(), "A".into());
        let base = base_params_with_public("点击登录", &existing);
        assert_eq!(base.get("批次").map(String::as_str), Some("A"));
        assert!(!base.contains_key("公司"));
    }
}
