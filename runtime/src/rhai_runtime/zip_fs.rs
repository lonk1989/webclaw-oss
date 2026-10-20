//! ZIP 解压（国内压缩包常用 GBK 文件名，无 UTF-8 标志时按 GBK 解码）

use rhai::{Array, Dynamic, EvalAltResult};
use std::fs::File;
use std::io::{copy, BufReader};
use std::path::{Component, Path, PathBuf};
use zip::read::ZipArchive;

fn runtime_err(msg: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(msg.into(), rhai::Position::NONE))
}

fn looks_like_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 优先用原始字节按 GBK 解（Windows 中文 zip）；失败或无中文时回退库解码名。
fn decode_entry_name(raw: &[u8], fallback: &str) -> String {
    let (gbk, _, had_errors) = encoding_rs::GBK.decode(raw);
    let gbk = gbk.into_owned();
    if !had_errors && (looks_like_cjk(&gbk) || gbk == fallback) {
        return gbk;
    }
    if looks_like_cjk(fallback) {
        return fallback.to_string();
    }
    if !had_errors {
        return gbk;
    }
    fallback.to_string()
}

fn safe_join(dest: &Path, entry_name: &str) -> Result<PathBuf, Box<EvalAltResult>> {
    let mut out = dest.to_path_buf();
    for part in entry_name.replace('\\', "/").split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(runtime_err(format!("非法 ZIP 路径: {entry_name}")));
        }
        for c in Path::new(part).components() {
            match c {
                Component::Normal(s) => out.push(s),
                Component::CurDir => {}
                _ => return Err(runtime_err(format!("非法 ZIP 路径: {entry_name}"))),
            }
        }
    }
    if !out.starts_with(dest) {
        return Err(runtime_err(format!("非法 ZIP 路径: {entry_name}")));
    }
    Ok(out)
}

/// 解压 zip 到 dest，返回相对 dest 的文件路径列表（`/` 分隔）。
pub fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<Vec<String>, Box<EvalAltResult>> {
    if !zip_path.is_file() {
        return Err(runtime_err(format!("ZIP 不存在: {}", zip_path.display())));
    }
    std::fs::create_dir_all(dest_dir).map_err(|e| runtime_err(format!("创建解压目录失败: {e}")))?;

    let file = File::open(zip_path).map_err(|e| runtime_err(format!("打开 ZIP 失败: {e}")))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|e| runtime_err(format!("读取 ZIP 失败: {e}")))?;

    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| runtime_err(format!("读取 ZIP 条目失败: {e}")))?;
        let name = decode_entry_name(entry.name_raw(), entry.name()).replace('\\', "/");

        if name.ends_with('/') {
            let dir = safe_join(dest_dir, &name)?;
            std::fs::create_dir_all(&dir).map_err(|e| runtime_err(format!("创建目录失败: {e}")))?;
            continue;
        }

        let out_path = safe_join(dest_dir, &name)?;
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| runtime_err(format!("创建目录失败: {e}")))?;
        }
        let mut outfile =
            File::create(&out_path).map_err(|e| runtime_err(format!("写出解压文件失败: {e}")))?;
        copy(&mut entry, &mut outfile).map_err(|e| runtime_err(format!("解压写入失败: {e}")))?;

        let rel = out_path
            .strip_prefix(dest_dir)
            .unwrap_or(&out_path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push(rel);
    }
    Ok(out)
}

pub fn extract_zip_to_array(zip_path: &Path, dest_dir: &Path) -> Result<Array, Box<EvalAltResult>> {
    let paths = extract_zip(zip_path, dest_dir)?;
    Ok(paths.into_iter().map(Dynamic::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    #[test]
    fn extract_zip_with_chinese_utf8_name() {
        let dir = std::env::temp_dir().join(format!("webclaw-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("t.zip");
        let dest = dir.join("out");

        {
            let file = File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let name = "阳泉高新区2026年6月考勤记录表.xlsx";
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(b"pk").unwrap();
            zip.finish().unwrap();
        }

        let files = extract_zip(&zip_path, &dest).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].contains("考勤记录表"), "got {}", files[0]);
        assert!(dest.join(&files[0]).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_gbk_raw_name() {
        // 「测试.xlsx」GBK
        let raw = encoding_rs::GBK.encode("测试.xlsx").0;
        let name = decode_entry_name(&raw, "??.xlsx");
        assert_eq!(name, "测试.xlsx");
    }
}
