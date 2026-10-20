//! 国内模型 CDN：目录结构与本地 `ocr-models` / `classify-models` 一致。

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

/// Optional models CDN — set `WEBCLAW_MODELS_CDN` (empty disables remote model fetch).
pub fn models_cdn_base() -> String {
    std::env::var("WEBCLAW_MODELS_CDN").unwrap_or_default()
}

pub fn cdn_url(relative: &str) -> String {
    let base = models_cdn_base();
    if base.trim().is_empty() {
        return String::new();
    }
    let rel = relative.trim_start_matches('/');
    format!("{}/{rel}", base.trim_end_matches('/'))
}

/// 下载到 `dest`（先写 `.part` 再改名）。成功返回 Ok。
pub fn download_to_file(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = dest.with_file_name(format!(
        "{}.part",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
    ));

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent("webclaw-model-cdn/1.0")
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("请求失败 {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}: {url}", resp.status()));
    }
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp, &mut file).map_err(|e| format!("写入失败: {e}"))?;
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("保存失败: {e}")
    })?;
    Ok(())
}

/// 按相对路径（如 `ocr-models/xx.onnx`）从 CDN 拉取；失败返回 Err。
pub fn download_cdn_relative(relative: &str, dest: &Path) -> Result<(), String> {
    download_to_file(&cdn_url(relative), dest)
}

/// 校验本地文件 sha256（小写 hex）；不匹配则删除并返回 Err。
pub fn verify_sha256_or_remove(path: &Path, expected_hex: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected_hex {
        let _ = std::fs::remove_file(path);
        return Err(format!(
            "sha256 不匹配 {}: expected {expected_hex}, got {actual}",
            path.display()
        ));
    }
    Ok(())
}
