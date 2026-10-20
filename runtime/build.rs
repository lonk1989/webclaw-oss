//! 按目标平台从 platform-versions/<os>.json 注入应用版本。
//! macOS → macos.json；Windows → windows.json。
//!
//! Windows：把 ort 附带的 DirectML.dll 同步到 resources/，供 NSIS 打进安装包。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let version_file = match target_os.as_str() {
        "macos" => Some(manifest_dir.join("platform-versions/macos.json")),
        "windows" => Some(manifest_dir.join("platform-versions/windows.json")),
        _ => None,
    };

    let version = if let Some(path) = version_file.as_ref() {
        println!("cargo:rerun-if-changed={}", path.display());
        read_version_json(path).unwrap_or_else(|e| {
            panic!(
                "读取平台版本失败（{}）：{e}。请确认存在 {{\"version\": \"x.y.z\"}}",
                path.display()
            )
        })
    } else {
        env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into())
    };

    println!("cargo:rustc-env=WEBCLAW_APP_VERSION={version}");
    #[cfg(feature = "desktop")]
    {
        if target_os == "windows" {
            // Must run before tauri_build: resources existence is checked inside build().
            stage_directml_resource(&manifest_dir);
        }
        // Override tauri.conf.json version so Info.plist / package version match.
        env::set_var("TAURI_CONFIG", format!(r#"{{"version":"{version}"}}"#));
        tauri_build::build();
    }
}

/// ort 会把 DirectML.dll 拷到 target；再同步到 resources/ 供 NSIS 打包。
/// 若尚未编译出该文件，则写一个占位文件，避免 `cargo check` 因 resources 缺失失败；
/// 正式 `tauri build` 前由 scripts/stage-directml.ps1（仅 Windows）覆盖为真实 DLL。
fn stage_directml_resource(manifest_dir: &PathBuf) {
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let mut candidates = Vec::new();
    if let Ok(tdir) = env::var("CARGO_TARGET_DIR") {
        candidates.push(PathBuf::from(tdir).join(&profile).join("DirectML.dll"));
    }
    candidates.push(manifest_dir.join("target").join(&profile).join("DirectML.dll"));
    candidates.push(
        manifest_dir
            .join("..")
            .join("target")
            .join(&profile)
            .join("DirectML.dll"),
    );

    let dest_dir = manifest_dir.join("resources");
    let _ = fs::create_dir_all(&dest_dir);
    let dest = dest_dir.join("DirectML.dll");

    for src in candidates {
        if src.is_file() {
            match fs::copy(&src, &dest) {
                Ok(_) => {
                    println!(
                        "cargo:warning=staged DirectML.dll {} -> {}",
                        src.display(),
                        dest.display()
                    );
                    return;
                }
                Err(e) => println!(
                    "cargo:warning=copy DirectML.dll failed {}: {e}",
                    src.display()
                ),
            }
        }
    }

    if !dest.is_file() {
        // 占位，满足 tauri resources 存在性检查；安装钩子会跳过空文件
        if let Err(e) = fs::write(&dest, b"") {
            println!("cargo:warning=create DirectML.dll placeholder failed: {e}");
        } else {
            println!(
                "cargo:warning=DirectML.dll placeholder at {} (run release build before NSIS)",
                dest.display()
            );
        }
    }
}

fn read_version_json(path: &PathBuf) -> Result<String, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v = extract_version_field(&text).ok_or_else(|| "缺少 version 字段".to_string())?;
    validate_semver(&v)?;
    Ok(v)
}

fn extract_version_field(text: &str) -> Option<String> {
    // 轻量解析：匹配 "version": "x.y.z"
    let key = "\"version\"";
    let idx = text.find(key)?;
    let after = &text[idx + key.len()..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].trim().to_string())
}

fn validate_semver(v: &str) -> Result<(), String> {
    let parts: Vec<_> = v.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("版本号须为 x.y.z，收到：{v}"));
    }
    for p in parts {
        if p.parse::<u64>().is_err() {
            return Err(format!("版本号须为 x.y.z，收到：{v}"));
        }
    }
    Ok(())
}
