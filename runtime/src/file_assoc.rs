//! Windows：在用户级别（HKCU）注册 `.webclaw` 文件关联，无需管理员权限。

use crate::paths;
use std::path::{Path, PathBuf};

const PROG_ID: &str = "webclaw.task";
const EXT: &str = ".webclaw";
const TYPE_NAME: &str = "Webclaw 自动化任务";
fn marker_path() -> PathBuf {
    paths::file_assoc_marker_file()
}

fn read_marker_exe() -> Option<PathBuf> {
    let raw = std::fs::read_to_string(marker_path()).ok()?;
    let p = raw.trim();
    if p.is_empty() {
        return None;
    }
    Some(PathBuf::from(p))
}

fn write_marker_exe(exe: &Path) -> Result<(), String> {
    let dir = paths::app_root_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(marker_path(), exe.to_string_lossy().as_bytes()).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn notify_shell_association_changed() {
    use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
    unsafe {
        let _ = SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

#[cfg(windows)]
fn read_registered_command() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.open_subkey("Software\\Classes").ok()?;
    let prog = classes.open_subkey(PROG_ID).ok()?;
    let cmd = prog
        .open_subkey("shell\\open\\command")
        .ok()?
        .get_value::<String, _>("")
        .ok()?;
    Some(cmd)
}

#[cfg(windows)]
fn write_registry(exe: &Path) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let exe_display = exe.display().to_string();
    let command = format!("\"{exe_display}\" --run \"%1\"");

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (classes, _) = hkcu
        .create_subkey("Software\\Classes")
        .map_err(|e| format!("无法打开注册表 Classes: {e}"))?;

    let (ext_key, _) = classes
        .create_subkey(EXT)
        .map_err(|e| format!("无法创建 {EXT}: {e}"))?;
    ext_key
        .set_value("", &PROG_ID)
        .map_err(|e| e.to_string())?;

    let (prog, _) = classes
        .create_subkey(PROG_ID)
        .map_err(|e| format!("无法创建 {PROG_ID}: {e}"))?;
    prog.set_value("", &TYPE_NAME)
        .map_err(|e| e.to_string())?;

    let (icon, _) = prog
        .create_subkey("DefaultIcon")
        .map_err(|e| e.to_string())?;
    icon.set_value("", &format!("{exe_display},0"))
        .map_err(|e| e.to_string())?;

    let (shell, _) = prog.create_subkey("shell").map_err(|e| e.to_string())?;
    let (open, _) = shell.create_subkey("open").map_err(|e| e.to_string())?;
    let (cmd_key, _) = open.create_subkey("command").map_err(|e| e.to_string())?;
    cmd_key
        .set_value("", &command)
        .map_err(|e| e.to_string())?;

    if let Some(file_name) = exe.file_name().and_then(|s| s.to_str()) {
        let app_path = format!("Applications\\{file_name}");
        let (app_key, _) = classes
            .create_subkey(&app_path)
            .map_err(|e| e.to_string())?;
        app_key
            .set_value("FriendlyAppName", &"webclaw")
            .map_err(|e| e.to_string())?;
        let (app_open, _) = app_key.create_subkey("shell\\open\\command").map_err(|e| e.to_string())?;
        app_open
            .set_value("", &command)
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[cfg(windows)]
pub fn is_registered_for_current_exe() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(marker) = read_marker_exe() else {
        return false;
    };
    if marker != exe {
        return false;
    }
    let Some(cmd) = read_registered_command() else {
        return false;
    };
    let exe_display = exe.display().to_string();
    cmd.contains(&exe_display)
}

#[cfg(not(windows))]
pub fn is_registered_for_current_exe() -> bool {
    false
}

#[cfg(windows)]
pub fn register_if_needed() -> Result<String, String> {
    if is_registered_for_current_exe() {
        return Ok("已注册 .webclaw 文件关联（当前程序）".into());
    }
    register_force()
}

#[cfg(not(windows))]
pub fn register_if_needed() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        Ok("macOS 上 .webclaw 由安装包 Info.plist 声明关联；安装 .app 后可用 Finder 打开".into())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok("文件关联注册仅支持 Windows / macOS 安装包".into())
    }
}

#[cfg(windows)]
pub fn register_force() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法获取程序路径: {e}"))?;
    write_registry(&exe)?;
    write_marker_exe(&exe)?;
    notify_shell_association_changed();
    Ok(format!(
        "已注册 .webclaw → {}",
        exe.display()
    ))
}

#[cfg(not(windows))]
pub fn register_force() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        Ok("macOS 无需手动注册：请使用安装后的 webclaw.app，系统会按 Info.plist 关联 .webclaw".into())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("文件关联注册仅支持 Windows".into())
    }
}

pub fn association_status() -> serde_json::Value {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    #[cfg(windows)]
    {
        let registered = is_registered_for_current_exe();
        let command = read_registered_command().unwrap_or_default();
        return serde_json::json!({
            "platform": "windows",
            "registered": registered,
            "exe": exe,
            "progId": PROG_ID,
            "extension": EXT,
            "command": command,
        });
    }
    #[cfg(target_os = "macos")]
    {
        let bundled = exe.contains(".app/");
        return serde_json::json!({
            "platform": "macos",
            "registered": bundled,
            "bundleDeclared": true,
            "exe": exe,
            "progId": PROG_ID,
            "extension": EXT,
            "hint": if bundled {
                "已由应用包声明 .webclaw 关联"
            } else {
                "开发态无系统关联；打包安装 .app 后由 Info.plist 声明"
            },
        });
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        serde_json::json!({
            "platform": std::env::consts::OS,
            "registered": false,
            "exe": exe,
            "progId": PROG_ID,
            "extension": EXT,
        })
    }
}
