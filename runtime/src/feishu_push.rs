//! 经飞书中继向已绑定用户推送文本 / 附件（HTTP，不依赖 UI WebSocket）。

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::path::Path;

use crate::feishu_relay;
use crate::settings;

const MAX_FILE_BYTES: u64 = 18 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct PushOk {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default, alias = "messageId")]
    message_id_camel: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PushErr {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

fn credentials() -> Result<(String, String), String> {
    let s = settings::load_settings()?;
    if !s.feishu_enabled {
        return Err("飞书功能未开启（设置 → 飞书）".into());
    }
    let id = s.feishu_client_id.trim().to_string();
    let secret = s.feishu_client_secret.trim().to_string();
    if id.is_empty() || secret.is_empty() {
        return Err("尚未注册飞书中继客户端，请先在客户端完成飞书绑定".into());
    }
    Ok((id, secret))
}

fn infer_kind(file_name: &str) -> &'static str {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => "image",
        _ => "file",
    }
}

async fn post_push(body: serde_json::Value) -> Result<String, String> {
    let url = feishu_relay::push_url(&feishu_relay::config());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("连接飞书中继失败: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取中继响应失败: {e}"))?;

    if status.is_success() {
        if let Ok(ok) = serde_json::from_str::<PushOk>(&text) {
            let mid = ok
                .message_id
                .or(ok.message_id_camel)
                .unwrap_or_default();
            return Ok(if mid.is_empty() {
                format!("已推送（{})", ok.kind.unwrap_or_else(|| "ok".into()))
            } else {
                format!(
                    "已推送（{}）message_id={mid}",
                    ok.kind.unwrap_or_else(|| "ok".into())
                )
            });
        }
        return Ok("已推送".into());
    }

    if let Ok(err) = serde_json::from_str::<PushErr>(&text) {
        let code = err.code.unwrap_or_default();
        let msg = err.error.unwrap_or(text);
        if code.is_empty() {
            return Err(format!("推送失败: {msg}"));
        }
        return Err(format!("推送失败[{code}]: {msg}"));
    }
    Err(format!("推送失败 HTTP {}: {text}", status.as_u16()))
}

/// 同步版本（Rhai / blocking 线程）
fn post_push_blocking(body: serde_json::Value) -> Result<String, String> {
    let url = feishu_relay::push_url(&feishu_relay::config());
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("连接飞书中继失败: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("读取中继响应失败: {e}"))?;

    if status.is_success() {
        if let Ok(ok) = serde_json::from_str::<PushOk>(&text) {
            let mid = ok
                .message_id
                .or(ok.message_id_camel)
                .unwrap_or_default();
            return Ok(if mid.is_empty() {
                format!("已推送（{})", ok.kind.unwrap_or_else(|| "ok".into()))
            } else {
                format!(
                    "已推送（{}）message_id={mid}",
                    ok.kind.unwrap_or_else(|| "ok".into())
                )
            });
        }
        return Ok("已推送".into());
    }

    if let Ok(err) = serde_json::from_str::<PushErr>(&text) {
        let code = err.code.unwrap_or_default();
        let msg = err.error.unwrap_or(text);
        if code.is_empty() {
            return Err(format!("推送失败: {msg}"));
        }
        return Err(format!("推送失败[{code}]: {msg}"));
    }
    Err(format!("推送失败 HTTP {}: {text}", status.as_u16()))
}

pub async fn push_text(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("推送消息内容不能为空".into());
    }
    let (client_id, client_secret) = credentials()?;
    post_push(json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "kind": "text",
        "text": text,
    }))
    .await
}

pub fn push_text_blocking(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("推送消息内容不能为空".into());
    }
    let (client_id, client_secret) = credentials()?;
    post_push_blocking(json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "kind": "text",
        "text": text,
    }))
}

pub async fn push_file_path(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| format!("读取文件失败: {e}"))?;
    if !meta.is_file() {
        return Err(format!("不是文件: {}", path.display()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "文件过大（{}MB），上限 {}MB",
            meta.len() / 1024 / 1024,
            MAX_FILE_BYTES / 1024 / 1024
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file.bin")
        .to_string();
    push_file_bytes(&file_name, &bytes).await
}

pub fn push_file_path_blocking(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| format!("读取文件失败: {e}"))?;
    if !meta.is_file() {
        return Err(format!("不是文件: {}", path.display()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "文件过大（{}MB），上限 {}MB",
            meta.len() / 1024 / 1024,
            MAX_FILE_BYTES / 1024 / 1024
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file.bin")
        .to_string();
    push_file_bytes_blocking(&file_name, &bytes)
}

pub async fn push_file_bytes(file_name: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("文件内容为空".into());
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!(
            "文件过大（{}MB），上限 {}MB",
            bytes.len() / 1024 / 1024,
            MAX_FILE_BYTES / 1024 / 1024
        ));
    }
    let (client_id, client_secret) = credentials()?;
    let kind = infer_kind(file_name);
    let data_b64 = B64.encode(bytes);
    post_push(json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "kind": kind,
        "fileName": file_name,
        "dataBase64": data_b64,
    }))
    .await
}

pub fn push_file_bytes_blocking(file_name: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("文件内容为空".into());
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!(
            "文件过大（{}MB），上限 {}MB",
            bytes.len() / 1024 / 1024,
            MAX_FILE_BYTES / 1024 / 1024
        ));
    }
    let (client_id, client_secret) = credentials()?;
    let kind = infer_kind(file_name);
    let data_b64 = B64.encode(bytes);
    post_push_blocking(json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "kind": kind,
        "fileName": file_name,
        "dataBase64": data_b64,
    }))
}

/// 按空格 / 逗号 / 顿号 / 分号 / 换行拆多附件查询
pub fn split_attachment_queries(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in args.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | ',' | '，' | '、' | ';' | '；') {
            let t = cur.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_attachment_queries_ordinary_separators() {
        assert_eq!(
            split_attachment_queries("考勤 月报#2"),
            vec!["考勤".to_string(), "月报#2".to_string()]
        );
        assert_eq!(
            split_attachment_queries("考勤，月报#2、结果.xlsx"),
            vec![
                "考勤".to_string(),
                "月报#2".to_string(),
                "结果.xlsx".to_string()
            ]
        );
        assert!(split_attachment_queries("  ").is_empty());
    }

    #[test]
    fn infer_kind_image_vs_file() {
        assert_eq!(infer_kind("a.PNG"), "image");
        assert_eq!(infer_kind("a.xlsx"), "file");
    }
}
