//! 飞书云盘（Drive）Open API：从当前页文件夹 URL 解析 token 并上传本地文件。
//! 能力对齐官方 lark-cli `drive +upload`，但走与多维表相同的内置应用凭证，无需本机安装 CLI。

use reqwest::multipart::{Form, Part};
use serde_json::Value;
use std::path::Path;
use url::Url;

use super::bitable_client::{api_root, shared_auth_token, shared_http_client};

/// 全量上传上限（字节）；更大文件走分片接口
const UPLOAD_ALL_MAX: u64 = 20 * 1024 * 1024;
const UPLOAD_PART_SIZE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DriveUploadResult {
    pub file_token: String,
    pub file_name: String,
    pub folder_token: String,
    pub size: u64,
}

/// 从开放平台下载 URL 解析 file_token：
/// `…/drive/v1/files/{token}/download`
pub fn parse_open_api_file_token(url: &str) -> Option<String> {
    let parsed = Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if !(host.contains("feishu.cn")
        || host.contains("larksuite.com")
        || host.contains("feishu.net"))
    {
        return None;
    }
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    // …/drive/v1/files/{token}/download 或 …/files/{token}/download
    let files_idx = segments.iter().position(|s| *s == "files")?;
    let token = segments.get(files_idx + 1)?.trim();
    let next = segments.get(files_idx + 2).copied().unwrap_or("");
    if token.is_empty() || next != "download" {
        return None;
    }
    Some(token.to_string())
}

/// 用内置应用 tenant_access_token 下载云盘文件（需文件对应用可见）。
pub async fn download_file_by_token(file_token: &str) -> Result<(Vec<u8>, String), String> {
    let file_token = file_token.trim();
    if file_token.is_empty() {
        return Err("file_token 为空".into());
    }
    let token = shared_auth_token().await?;
    let resp = shared_http_client()
        .get(format!(
            "{}/drive/v1/files/{}/download",
            api_root(),
            file_token
        ))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("飞书文件下载请求失败: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let http = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<Value>(&body) {
            return Err(ensure_ok_msg("飞书文件下载失败", http, &v));
        }
        return Err(format!(
            "飞书文件下载失败 HTTP {http}: {}",
            body.chars().take(200).collect::<String>()
        ));
    }

    let filename = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(filename_from_cd)
        .unwrap_or_else(|| format!("{file_token}.bin"));
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取飞书文件失败: {e}"))?;
    if bytes.is_empty() {
        return Err("飞书文件下载响应为空".into());
    }
    Ok((bytes.to_vec(), filename))
}

fn filename_from_cd(raw: &str) -> Option<String> {
    // filename*=UTF-8''… 或 filename="…"
    if let Some(idx) = raw.to_ascii_lowercase().find("filename*") {
        let rest = &raw[idx..];
        if let Some(eq) = rest.find("''") {
            let enc = rest[eq + 2..].trim().trim_matches('"').trim_matches(';');
            let decoded = urlencoding_decode(enc);
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    if let Some(idx) = raw.to_ascii_lowercase().find("filename=") {
        let mut v = raw[idx + 9..].trim();
        if let Some(semi) = v.find(';') {
            v = &v[..semi];
        }
        v = v.trim().trim_matches('"');
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

fn urlencoding_decode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn ensure_ok_msg(prefix: &str, http_status: u16, body: &Value) -> String {
    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    let msg = body
        .get("msg")
        .and_then(|v| v.as_str())
        .unwrap_or("未知错误");
    let hint = match code {
        _ if http_status == 401 || http_status == 403 || code == 99991668 || code == 99991672 => {
            "\n提示：开放平台应用鉴权失败或无权下载该文件。请确认文件已分享给应用，或改用浏览器登录态下载。"
        }
        _ => "",
    };
    format!("{prefix} ({code}): {msg}{hint}")
}

/// 从飞书云盘文件夹 URL 或裸 token 解析 folder_token。
///
/// 支持：
/// - `https://xxx.feishu.cn/drive/folder/{token}?…`
/// - `https://xxx.feishu.cn/folder/{token}`
/// - 纯 token（无斜杠）
pub fn parse_drive_folder_token(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("文件夹地址为空".into());
    }
    if !raw.contains("://") && !raw.contains('/') && !raw.contains('\\') {
        return Ok(raw.to_string());
    }

    let parsed = Url::parse(raw).map_err(|e| format!("无法解析云盘文件夹地址: {e}"))?;
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if !(host.contains("feishu.cn")
        || host.contains("larksuite.com")
        || host.contains("feishu.net"))
    {
        return Err(format!("当前页不是飞书域名：{host}"));
    }

    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();

    // …/drive/folder/{token} 或 …/folder/{token}
    if let Some(idx) = segments.iter().position(|s| *s == "folder") {
        if let Some(tok) = segments.get(idx + 1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            return Ok((*tok).to_string());
        }
    }

    Err(
        "当前页不是云盘文件夹（URL 需包含 /drive/folder/{{token}}）。请先在浏览器打开目标文件夹。"
            .into(),
    )
}

/// 上传本地文件到指定云盘文件夹（≤20MB 全量；更大自动分片）。
pub async fn upload_file_to_folder(
    folder_token: &str,
    path: &Path,
) -> Result<DriveUploadResult, String> {
    let folder_token = folder_token.trim();
    if folder_token.is_empty() {
        return Err("folder_token 为空".into());
    }
    if !path.is_file() {
        return Err(format!("文件不存在: {}", path.display()));
    }

    let meta = std::fs::metadata(path).map_err(|e| format!("读取文件信息失败: {e}"))?;
    let size = meta.len();
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "无法取得文件名".to_string())?
        .to_string();

    let file_token = if size <= UPLOAD_ALL_MAX {
        upload_all(folder_token, &file_name, path, size).await?
    } else {
        upload_multipart(folder_token, &file_name, path, size).await?
    };

    Ok(DriveUploadResult {
        file_token,
        file_name,
        folder_token: folder_token.to_string(),
        size,
    })
}

async fn upload_all(
    folder_token: &str,
    file_name: &str,
    path: &Path,
    size: u64,
) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let token = shared_auth_token().await?;

    let file_part = Part::bytes(bytes)
        .file_name(file_name.to_string())
        .mime_str("application/octet-stream")
        .map_err(|e| format!("构造上传表单失败: {e}"))?;

    let form = Form::new()
        .text("file_name", file_name.to_string())
        .text("parent_type", "explorer".to_string())
        .text("parent_node", folder_token.to_string())
        .text("size", size.to_string())
        .part("file", file_part);

    let resp = shared_http_client()
        .post(format!("{}/drive/v1/files/upload_all", api_root()))
        .bearer_auth(&token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("飞书上传请求失败: {e}"))?;

    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("解析飞书上传响应失败: {e}"))?;
    ensure_ok("飞书上传失败", status.as_u16(), &body)?;
    body.get("data")
        .and_then(|d| d.get("file_token"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "飞书上传响应缺少 file_token".into())
}

async fn upload_multipart(
    folder_token: &str,
    file_name: &str,
    path: &Path,
    size: u64,
) -> Result<String, String> {
    let token = shared_auth_token().await?;
    let prepare_body = serde_json::json!({
        "file_name": file_name,
        "parent_type": "explorer",
        "parent_node": folder_token,
        "size": size,
    });
    let prepare: Value = shared_http_client()
        .post(format!("{}/drive/v1/files/upload_prepare", api_root()))
        .bearer_auth(&token)
        .json(&prepare_body)
        .send()
        .await
        .map_err(|e| format!("飞书分片准备失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("解析分片准备响应失败: {e}"))?;
    ensure_ok("飞书分片准备失败", 200, &prepare)?;

    let data = prepare.get("data").cloned().unwrap_or(Value::Null);
    let upload_id = data
        .get("upload_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "分片准备缺少 upload_id".to_string())?
        .to_string();
    let block_size = data
        .get("block_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(UPLOAD_PART_SIZE as u64) as usize;
    let block_num = data
        .get("block_num")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            let bs = block_size.max(1) as u64;
            (size + bs - 1) / bs
        }) as usize;

    let file_bytes = std::fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    for seq in 0..block_num {
        let start = seq * block_size;
        if start >= file_bytes.len() {
            break;
        }
        let end = (start + block_size).min(file_bytes.len());
        let chunk = file_bytes[start..end].to_vec();
        let chunk_len = chunk.len();
        let part = Part::bytes(chunk)
            .file_name(format!("part-{seq}"))
            .mime_str("application/octet-stream")
            .map_err(|e| format!("构造分片表单失败: {e}"))?;
        let form = Form::new()
            .text("upload_id", upload_id.clone())
            .text("seq", seq.to_string())
            .text("size", chunk_len.to_string())
            .part("file", part);
        let part_resp: Value = shared_http_client()
            .post(format!("{}/drive/v1/files/upload_part", api_root()))
            .bearer_auth(&token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("飞书分片上传失败 (seq={seq}): {e}"))?
            .json()
            .await
            .map_err(|e| format!("解析分片上传响应失败: {e}"))?;
        ensure_ok(&format!("飞书分片上传失败 (seq={seq})"), 200, &part_resp)?;
    }

    let finish_body = serde_json::json!({
        "upload_id": upload_id,
        "block_num": block_num,
    });
    let finish: Value = shared_http_client()
        .post(format!("{}/drive/v1/files/upload_finish", api_root()))
        .bearer_auth(&token)
        .json(&finish_body)
        .send()
        .await
        .map_err(|e| format!("飞书分片完成失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("解析分片完成响应失败: {e}"))?;
    ensure_ok("飞书分片完成失败", 200, &finish)?;
    finish
        .get("data")
        .and_then(|d| d.get("file_token"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "飞书分片完成响应缺少 file_token".into())
}

fn ensure_ok(prefix: &str, http_status: u16, body: &Value) -> Result<(), String> {
    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code == 0 {
        return Ok(());
    }
    let msg = body
        .get("msg")
        .and_then(|v| v.as_str())
        .unwrap_or("未知错误");
    let hint = match code {
        1061002 | 1061004 | 99991672 => {
            "\n提示：请确认开放平台应用已开通「上传文件」等云盘权限，并将该文件夹分享给应用（或应用所在企业可见）。"
        }
        1061044 => "\n提示：文件夹 token 无效或不存在，请确认浏览器已打开正确的云盘文件夹页。",
        _ if http_status == 401 || http_status == 403 => {
            "\n提示：鉴权失败，请检查应用凭证与云盘权限。"
        }
        _ => "",
    };
    Err(format!("{prefix} ({code}): {msg}{hint}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_folder_from_drive_url() {
        let tok = parse_drive_folder_token(
            "https://pcnzwk7m6zlv.feishu.cn/drive/folder/L7MGfvyGql4gZUdwc8AcjayXnhg?from=space_personal_filelist",
        )
        .unwrap();
        assert_eq!(tok, "L7MGfvyGql4gZUdwc8AcjayXnhg");
    }

    #[test]
    fn parse_bare_token() {
        assert_eq!(
            parse_drive_folder_token("L7MGfvyGql4gZUdwc8AcjayXnhg").unwrap(),
            "L7MGfvyGql4gZUdwc8AcjayXnhg"
        );
    }

    #[test]
    fn reject_non_folder_page() {
        let err = parse_drive_folder_token("https://xxx.feishu.cn/base/bascnXXX?table=tbl").unwrap_err();
        assert!(err.contains("云盘文件夹"));
    }

    #[test]
    fn parse_open_api_download_token() {
        let tok = parse_open_api_file_token(
            "https://open.feishu.cn/open-apis/drive/v1/files/boxcnABCDEFG/download",
        )
        .unwrap();
        assert_eq!(tok, "boxcnABCDEFG");
        assert!(parse_open_api_file_token(
            "https://xxx.feishu.cn/drive/folder/L7MGfvyGql4gZUdwc8AcjayXnhg"
        )
        .is_none());
    }
}
