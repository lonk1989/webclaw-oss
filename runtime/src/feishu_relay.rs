use serde::Serialize;

/// 飞书中继 endpoint（内置，不暴露给用户配置）
#[derive(Clone, Serialize)]
pub struct FeishuRelayConfig {
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn config() -> FeishuRelayConfig {
    // OSS: no built-in relay host; configure via product overlay if needed.
    FeishuRelayConfig {
        host: String::new(),
        port: 0,
        path: "/webclaw".into(),
    }
}

pub fn normalize_path(path: &str) -> String {
    let raw = path.trim();
    if raw.is_empty() || raw == "/" {
        return String::new();
    }
    let with_slash = if raw.starts_with('/') {
        raw.to_string()
    } else {
        format!("/{raw}")
    };
    with_slash.trim_end_matches('/').to_string()
}

pub fn health_url(cfg: &FeishuRelayConfig) -> String {
    let base = normalize_path(&cfg.path);
    format!("https://{}:{}{}/health", cfg.host, cfg.port, base)
}

pub fn push_url(cfg: &FeishuRelayConfig) -> String {
    let base = normalize_path(&cfg.path);
    format!("https://{}:{}{}/api/clients/push", cfg.host, cfg.port, base)
}
