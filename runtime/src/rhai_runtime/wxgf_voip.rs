//! wxgf（微信自研图像封装）转码。
//!
//! 通过本机微信 `VoipEngine.dll` 解码。DLL 偶发访问冲突会拖垮进程，
//! 因此默认在独立子进程中解码；主进程只收结果，子进程崩了也不退出。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(1);

/// 若命令行为 `--wxgf-decode <in> <out>`，执行解码后直接退出（供主进程拉起）。
/// 返回 `true` 表示已处理并应终止当前进程。
pub fn run_cli_if_requested() -> bool {
    let mut args = std::env::args_os();
    let _exe = args.next();
    let Some(flag) = args.next() else {
        return false;
    };
    if flag != *"--wxgf-decode" {
        return false;
    }
    let Some(input) = args.next() else {
        eprintln!("wxgf-decode: missing input");
        std::process::exit(2);
    };
    let Some(output) = args.next() else {
        eprintln!("wxgf-decode: missing output");
        std::process::exit(2);
    };
    match decode_file(Path::new(&input), Path::new(&output)) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

pub fn transcode(data: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    if data.len() < 15 || !data.starts_with(b"wxgf") {
        return Err("不是 wxgf".into());
    }
    let voip_err = match try_voip(data) {
        Ok(img) => return Ok(img),
        Err(e) => e,
    };
    // 微信 DLL 拒识（常见 -224）时，抽出 HEVC 裸流交给 ffmpeg。
    match transcode_via_ffmpeg(data) {
        Ok(img) => Ok(img),
        Err(ff) => Err(format!("{voip_err}；{ff}")),
    }
}

fn try_voip(data: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    #[cfg(windows)]
    {
        // 单元测试直接进程内解码；正式运行走子进程隔离，避免 VoipEngine 崩主进程
        if cfg!(test) || std::env::var_os("WEBCLAW_WXGF_INPROCESS").is_some() {
            return voip::transcode_in_process(data);
        }
        return transcode_in_subprocess(data);
    }
    #[cfg(not(windows))]
    {
        let _ = data;
        Err("wxgf 转码仅支持 Windows（依赖本机微信 VoipEngine.dll）".into())
    }
}

fn decode_file(input: &Path, output: &Path) -> Result<(), String> {
    let data = std::fs::read(input).map_err(|e| format!("读取 wxgf 失败: {e}"))?;
    #[cfg(windows)]
    {
        let (bytes, _ext) = voip::transcode_in_process(&data)?;
        if let Some(parent) = output.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(output, &bytes).map_err(|e| format!("写入解码结果失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (data, output);
        Err("wxgf 转码仅支持 Windows".into())
    }
}

#[cfg(windows)]
fn transcode_in_subprocess(data: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    let id = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join("webclaw-wxgf");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let in_path = dir.join(format!("{id}.wxgf"));
    let out_path = dir.join(format!("{id}.img"));
    std::fs::write(&in_path, data).map_err(|e| format!("写入临时 wxgf 失败: {e}"))?;

    let exe = std::env::current_exe().map_err(|e| format!("定位可执行文件失败: {e}"))?;
    let mut cmd = Command::new(&exe);
    cmd.arg("--wxgf-decode").arg(&in_path).arg(&out_path);
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("启动 wxgf 解码子进程失败: {e}"))?;

    let bytes = std::fs::read(&out_path);
    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        // STATUS_ACCESS_VIOLATION = 0xC0000005
        if code == -1073741819 {
            return Err("wxgf 解码子进程异常退出（已与主程序隔离）".into());
        }
        if !err.is_empty() {
            return Err(err.chars().take(400).collect());
        }
        return Err(format!("wxgf 解码失败（exit={code}）"));
    }

    let bytes = bytes.map_err(|e| format!("读取解码结果失败: {e}"))?;
    let ext = image_ext_of(&bytes).ok_or_else(|| "解码结果不是 jpg/png".to_string())?;
    Ok((bytes, ext))
}

fn image_ext_of(data: &[u8]) -> Option<&'static str> {
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return Some("jpg");
    }
    if data.len() >= 8 && data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    None
}

/// 最大的 HEVC Annex-B 分片。长度字段在起始码前 4 字节、大端（与 chatlog 一致）。
fn extract_primary_hevc(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 8 || !data.starts_with(b"wxgf") {
        return None;
    }
    let header_len = data[4] as usize;
    if header_len >= data.len() {
        return None;
    }
    let patterns: &[&[u8]] = &[b"\x00\x00\x00\x01", b"\x00\x00\x01"];
    for pattern in patterns {
        let mut parts: Vec<(usize, usize)> = Vec::new();
        let mut offset = 0usize;
        while header_len + offset < data.len() {
            let window = &data[header_len + offset..];
            let Some(index) = find_bytes(window, pattern) else {
                break;
            };
            let abs = header_len + offset + index;
            if abs < 4 {
                offset += index + 1;
                continue;
            }
            let length = u32::from_be_bytes(data[abs - 4..abs].try_into().unwrap()) as usize;
            if length == 0 || abs + length > data.len() {
                offset += index + 1;
                continue;
            }
            parts.push((abs, length));
            offset += index + length;
        }
        if let Some(&(off, len)) = parts.iter().max_by_key(|(_, len)| *len) {
            if len > 32 {
                return Some(data[off..off + len].to_vec());
            }
        }
    }
    // 分片头对不上时，从第一处起始码截到末尾再试
    let start = find_bytes(&data[header_len..], b"\x00\x00\x00\x01")
        .map(|i| header_len + i)
        .or_else(|| find_bytes(&data[header_len..], b"\x00\x00\x01").map(|i| header_len + i))?;
    if data.len() - start > 32 {
        Some(data[start..].to_vec())
    } else {
        None
    }
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn find_ffmpeg() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("FFMPEG_PATH") {
        let p = PathBuf::from(raw);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("ffmpeg.exe");
            if beside.is_file() {
                return Some(beside);
            }
            let beside = dir.join("ffmpeg");
            if beside.is_file() {
                return Some(beside);
            }
        }
    }
    let probe = Command::new("ffmpeg").arg("-version").output();
    if probe.ok().is_some_and(|o| o.status.success()) {
        return Some(PathBuf::from("ffmpeg"));
    }
    None
}

fn transcode_via_ffmpeg(wxgf: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    let annex = extract_primary_hevc(wxgf).ok_or_else(|| "wxgf 中未找到 HEVC 裸流".to_string())?;
    let ffmpeg = find_ffmpeg().ok_or_else(|| {
        "未找到 ffmpeg（安装并加入 PATH，或设置 FFMPEG_PATH，或把 ffmpeg.exe 放在程序同目录）"
            .to_string()
    })?;
    let jpg = ffmpeg_hevc_to_jpeg(&ffmpeg, &annex)?;
    let ext = image_ext_of(&jpg).ok_or_else(|| "ffmpeg 输出不是 jpg".to_string())?;
    Ok((jpg, ext))
}

fn ffmpeg_hevc_to_jpeg(ffmpeg: &Path, annex: &[u8]) -> Result<Vec<u8>, String> {
    // 先按裸 HEVC 喂；探测失败再让 ffmpeg 自己认格式
    let forced = run_ffmpeg(ffmpeg, annex, true)?;
    if image_ext_of(&forced).is_some() {
        return Ok(forced);
    }
    let probed = run_ffmpeg(ffmpeg, annex, false)?;
    if image_ext_of(&probed).is_some() {
        return Ok(probed);
    }
    Err("ffmpeg 无法把这段 HEVC 转成 jpg".into())
}

fn run_ffmpeg(ffmpeg: &Path, annex: &[u8], force_hevc: bool) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner").arg("-loglevel").arg("error");
    if force_hevc {
        cmd.arg("-f").arg("hevc");
    }
    cmd.args([
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "-q:v",
        "3",
        "pipe:1",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动 ffmpeg 失败: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(annex);
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("等待 ffmpeg 失败: {e}"))?;
    if output.stdout.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        if err.is_empty() {
            return Err("ffmpeg 没有输出".into());
        }
        return Err(err.chars().take(180).collect());
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::extract_primary_hevc;

    #[test]
    fn extracts_largest_hevc_partition() {
        let mut data = b"wxgf".to_vec();
        data.push(5); // header length
        data.extend_from_slice(&[0, 0, 0, 0]);
        let nal = b"\x00\x00\x00\x01\x40\x01hevc-payload-bytes-long-enough!!";
        data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        data.extend_from_slice(nal);
        let got = extract_primary_hevc(&data).expect("partition");
        assert_eq!(got, nal);
    }
}

#[cfg(windows)]
mod voip {
    use super::image_ext_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows::core::{PCSTR, PCWSTR};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, SetDllDirectoryW, LOAD_WITH_ALTERED_SEARCH_PATH,
    };

    /// config 至少含 mode；额外填充避免 DLL 读越界。
    #[repr(C)]
    struct WxAmConfig {
        mode: i32,
        pad: [i32; 15],
    }

    // DLL 实际返回 32 位码；声明为 i64 时高位常为 0，会出现 4294967072（实为 -224）这类数。
    type WxamDec =
        unsafe extern "system" fn(*const u8, i32, *mut u8, *mut i32, *const WxAmConfig) -> i64;

    fn normalize_voip_ret(ret: i64) -> i32 {
        ret as i32
    }

    fn format_voip_err(ret: i64) -> String {
        let code = normalize_voip_ret(ret);
        // -224 / 0xFFFFFF20：常见于损坏、未下完、或当前 VoipEngine 不支持的 wxgf
        if code == -224 {
            return "微信图转码失败（VoipEngine 拒识，多半损坏/未下完/格式过新）".into();
        }
        if code == 0 {
            return "VoipEngine 返回异常 0".into();
        }
        format!("微信图转码失败（VoipEngine 码 {code}）")
    }

    /// 仅在 `--wxgf-decode` 子进程内调用。
    pub fn transcode_in_process(data: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
        if data.len() > i32::MAX as usize {
            return Err("wxgf 过大".into());
        }
        let (decode, _module) = load_decoder()?;
        const OUT_CAP: usize = 52 * 1024 * 1024;
        let mut out_buf = vec![0u8; OUT_CAP];
        // mode=1 实测出 PNG；个别文件 mode=1 失败时再试 mode=0
        let mut last_ret = 0i64;
        for mode in [1i32, 0] {
            // 输入拷贝：部分版本会改写输入缓冲
            let input = data.to_vec();
            let config = WxAmConfig {
                mode,
                pad: [0; 15],
            };
            let mut out_len = OUT_CAP as i32;
            let ret = unsafe {
                decode(
                    input.as_ptr(),
                    input.len() as i32,
                    out_buf.as_mut_ptr(),
                    &mut out_len,
                    &config,
                )
            };
            last_ret = ret;
            if normalize_voip_ret(ret) != 0 {
                continue;
            }
            if out_len <= 32 || out_len as usize > OUT_CAP {
                continue;
            }
            let bytes = out_buf[..out_len as usize].to_vec();
            if let Some(ext) = image_ext_of(&bytes) {
                return Ok((bytes, ext));
            }
        }
        Err(format_voip_err(last_ret))
    }

    fn load_decoder() -> Result<(WxamDec, HMODULE), String> {
        let dll = find_voip_engine()?;
        let dir = dll
            .parent()
            .ok_or_else(|| "VoipEngine.dll 路径无效".to_string())?;
        let wide_dir: Vec<u16> = dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // 临时设置 DLL 搜索目录，加载后立刻恢复，避免污染主进程其它动态库（如 OCR）
        unsafe { SetDllDirectoryW(PCWSTR(wide_dir.as_ptr())) }
            .map_err(|e| format!("设置 DLL 目录失败: {e}"))?;
        let wide: Vec<u16> = dll
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let load_result = unsafe {
            LoadLibraryExW(
                PCWSTR(wide.as_ptr()),
                None,
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        };
        let _ = unsafe { SetDllDirectoryW(PCWSTR::null()) };
        let module = load_result.map_err(|e| format!("加载 {} 失败: {e}", dll.display()))?;
        let proc_name = PCSTR(b"wxam_dec_wxam2pic_5\0".as_ptr());
        let fp = unsafe { GetProcAddress(module, proc_name) }.ok_or_else(|| {
            format!("{} 未导出 wxam_dec_wxam2pic_5", dll.display())
        })?;
        let decode: WxamDec = unsafe { std::mem::transmute(fp) };
        Ok((decode, module))
    }

    fn find_voip_engine() -> Result<PathBuf, String> {
        let mut roots = Vec::new();
        push_registry_installs(&mut roots);
        if let Ok(base) = std::env::var("ProgramFiles(x86)") {
            roots.push(PathBuf::from(base).join("Weixin"));
        }
        if let Ok(base) = std::env::var("ProgramFiles") {
            roots.push(PathBuf::from(&base).join("Weixin"));
            let tencent = PathBuf::from(base).join("Tencent");
            roots.push(tencent.join("Weixin"));
            roots.push(tencent.join("WeChat"));
        }
        if let Some(local) = dirs::data_local_dir() {
            roots.push(local.join("Tencent").join("Weixin"));
        }

        let mut hits: Vec<PathBuf> = Vec::new();
        for root in roots {
            collect_voip(&root, &mut hits);
        }
        hits.sort_by(|a, b| version_key(b.parent()).cmp(&version_key(a.parent())));
        hits.dedup();
        hits.into_iter().next().ok_or_else(|| {
            "未找到本机微信 VoipEngine.dll（wxgf 转码需要已安装微信）".to_string()
        })
    }

    fn collect_voip(root: &Path, out: &mut Vec<PathBuf>) {
        if !root.is_dir() {
            return;
        }
        let direct = root.join("VoipEngine.dll");
        if direct.is_file() {
            out.push(direct);
        }
        let Ok(rd) = std::fs::read_dir(root) else {
            return;
        };
        for entry in rd.flatten() {
            let dll = entry.path().join("VoipEngine.dll");
            if dll.is_file() {
                out.push(dll);
            }
        }
    }

    fn push_registry_installs(roots: &mut Vec<PathBuf>) {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::RegKey;
        const PATHS: &[(winreg::HKEY, &str)] = &[
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_CURRENT_USER,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
        ];
        for (hive, path) in PATHS {
            let Ok(key) = RegKey::predef(*hive).open_subkey(path) else {
                continue;
            };
            for name in key.enum_keys().flatten() {
                let Ok(sub) = key.open_subkey(&name) else {
                    continue;
                };
                let display: String = sub.get_value("DisplayName").unwrap_or_default();
                if display.contains("输入法") {
                    continue;
                }
                let is_wechat = display.contains('微')
                    || display.eq_ignore_ascii_case("Weixin")
                    || display.eq_ignore_ascii_case("WeChat");
                if !is_wechat {
                    continue;
                }
                let loc: String = sub.get_value("InstallLocation").unwrap_or_default();
                let loc = loc.trim().trim_matches('"').trim();
                if !loc.is_empty() {
                    roots.push(PathBuf::from(loc));
                }
            }
        }
    }

    fn version_key(dir: Option<&Path>) -> Vec<u32> {
        let Some(dir) = dir else {
            return Vec::new();
        };
        let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        name.split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect()
    }
}
