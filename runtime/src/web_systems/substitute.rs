use super::store::{all_systems_for_substitute, WebSystem};
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

static VAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{([^{}]+)\}\}").expect("web system var regex"));

fn field_value(system: &WebSystem, field: &str) -> Option<String> {
    match field.trim().to_lowercase().as_str() {
        "url" | "地址" | "链接" | "link" | "网址" => Some(system.url.clone()),
        "username" | "账号" | "用户名" | "account" | "user" | "手机" | "手机号" => {
            Some(system.username.clone())
        }
        "password" | "密码" | "pwd" | "pass" => Some(system.password.clone()),
        _ => None,
    }
}

fn lookup_system<'a>(systems: &'a [WebSystem], key: &str) -> Option<&'a WebSystem> {
    let key = key.trim();
    systems
        .iter()
        .find(|s| s.id.eq_ignore_ascii_case(key) || s.name == key)
}

pub fn substitute_web_system_vars(content: &str) -> Result<String, String> {
    let systems = all_systems_for_substitute()?;
    if !content.contains("{{") {
        return Ok(content.to_string());
    }

    let mut cache: HashMap<String, String> = HashMap::new();

    let result = VAR_RE.replace_all(content, |caps: &regex::Captures| {
        let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let parts: Vec<&str> = inner.splitn(2, '.').collect();
        if parts.len() != 2 {
            return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
        }
        let cache_key = format!("{}.{}", parts[0].trim(), parts[1].trim());
        if let Some(v) = cache.get(&cache_key) {
            return v.clone();
        }
        let Some(system) = lookup_system(&systems, parts[0]) else {
            return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
        };
        let Some(value) = field_value(system, parts[1]) else {
            return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
        };
        cache.insert(cache_key, value.clone());
        value
    });

    Ok(result.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_known_fields() {
        let systems = vec![WebSystem {
            id: "ehr".into(),
            name: "EHR系统".into(),
            kind: super::super::store::WebSystemKind::Internal,
            url: "https://example.com/".into(),
            username: "user1".into(),
            password: "pass1".into(),
            updated_at: chrono::Utc::now(),
            source: super::super::store::WebSystemSource::Local,
            remark: String::new(),
            remote_status: None,
            has_password: true,
        }];
        let input = "打开{{ehr.url}}login，输入手机={{ehr.账号}}";
        let out = substitute_with_systems(input, &systems);
        assert_eq!(out, "打开https://example.com/login，输入手机=user1");
    }

    fn substitute_with_systems(content: &str, systems: &[WebSystem]) -> String {
        let mut cache: HashMap<String, String> = HashMap::new();
        VAR_RE
            .replace_all(content, |caps: &regex::Captures| {
                let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let parts: Vec<&str> = inner.splitn(2, '.').collect();
                if parts.len() != 2 {
                    return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
                }
                let cache_key = format!("{}.{}", parts[0].trim(), parts[1].trim());
                if let Some(v) = cache.get(&cache_key) {
                    return v.clone();
                }
                let Some(system) = lookup_system(systems, parts[0]) else {
                    return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
                };
                let Some(value) = field_value(system, parts[1]) else {
                    return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
                };
                cache.insert(cache_key, value.clone());
                value
            })
            .into_owned()
    }
}
