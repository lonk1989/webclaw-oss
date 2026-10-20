//! 微信 4.x 本地图片 `.dat` 解密（V0 / V1 / V2）。
//!
//! V2（主流）：`07 08 V2 08 07` + AES-128-ECB 头 + 16B 分隔 + XOR 尾。
//! 账号级密钥可由 MMKV kvcomm 文件名中的 code + 账号名离线派生：
//! `aes_key = md5("{code}{account}").hex()[:16]`（ASCII），`xor_key = code & 0xFF`。

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use md5::{Digest, Md5};
use rhai::{Dynamic, EvalAltResult, Map};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const V2_MAGIC: &[u8; 6] = b"\x07\x08V2\x08\x07";
const V1_MAGIC: &[u8; 6] = b"\x07\x08V1\x08\x07";
/// 社区已知的 V1 固定密钥
const V1_AES_KEY: &[u8; 16] = b"cfcd208495d565ef";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageKeys {
    pub aes_key: String,
    pub xor_key: u8,
    pub account: String,
    pub code: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatKind {
    V0,
    V1,
    V2,
    Plain,
}

fn err(msg: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(msg.into().into(), rhai::Position::NONE))
}

pub fn detect_kind(data: &[u8]) -> DatKind {
    if data.len() >= 6 && data.starts_with(V2_MAGIC) {
        DatKind::V2
    } else if data.len() >= 6 && data.starts_with(V1_MAGIC) {
        DatKind::V1
    } else if looks_like_image(data) {
        DatKind::Plain
    } else {
        DatKind::V0
    }
}

pub fn looks_like_image(data: &[u8]) -> bool {
    image_ext(data).is_some()
}

pub fn image_ext(data: &[u8]) -> Option<&'static str> {
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return Some("jpg");
    }
    if data.len() >= 4 && data.starts_with(b"\x89PNG") {
        return Some("png");
    }
    if data.len() >= 4 && (data.starts_with(b"GIF8")) {
        return Some("gif");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("webp");
    }
    if data.len() >= 4 && data.starts_with(b"wxgf") {
        return Some("wxgf");
    }
    if data.len() >= 2 && data[0] == 0x42 && data[1] == 0x4D {
        return Some("bmp");
    }
    None
}

pub fn clean_account_name(name: &str) -> String {
    let name = name.trim();
    if name.starts_with("wxid_") {
        let parts: Vec<&str> = name.split('_').collect();
        if parts.len() >= 3 {
            return format!("{}_{}", parts[0], parts[1]);
        }
    }
    // Lonk1989_ebe2 → 也尝试 Lonk1989（去掉末段短后缀）
    if let Some((head, tail)) = name.rsplit_once('_') {
        if !head.is_empty()
            && (2..=6).contains(&tail.len())
            && tail.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return head.to_string();
        }
    }
    name.to_string()
}

pub fn derive_keys(code: u64, account: &str) -> ImageKeys {
    let account = account.to_string();
    let material = format!("{code}{account}");
    let digest = Md5::digest(material.as_bytes());
    let hex = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let aes_key = hex.chars().take(16).collect::<String>();
    ImageKeys {
        aes_key,
        xor_key: (code & 0xFF) as u8,
        account,
        code,
    }
}

fn aes128_ecb_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    if key.len() != 16 {
        return Err(format!("AES 密钥须 16 字节，实际 {}", key.len()));
    }
    if data.len() % 16 != 0 {
        return Err(format!("AES 明文长度须为 16 倍数，实际 {}", data.len()));
    }
    let cipher = Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(16) {
        cipher.encrypt_block(chunk.into());
    }
    Ok(out)
}

fn aes128_ecb_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    if key.len() != 16 {
        return Err(format!("AES 密钥须 16 字节，实际 {}", key.len()));
    }
    if data.len() % 16 != 0 {
        return Err(format!("AES 密文长度须为 16 倍数，实际 {}", data.len()));
    }
    let cipher = Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(16) {
        cipher.decrypt_block(chunk.into());
    }
    Ok(out)
}

fn verify_aes_key(key_ascii: &[u8], first_ct_block: &[u8]) -> bool {
    if first_ct_block.len() < 16 || key_ascii.len() != 16 {
        return false;
    }
    match aes128_ecb_decrypt(key_ascii, &first_ct_block[..16]) {
        Ok(pt) => looks_like_image(&pt),
        Err(_) => false,
    }
}

pub fn decrypt_bytes(data: &[u8], keys: &ImageKeys) -> Result<Vec<u8>, String> {
    match detect_kind(data) {
        DatKind::Plain => Ok(data.to_vec()),
        DatKind::V0 => {
            if data.is_empty() {
                return Err("空文件".into());
            }
            let xor = data[0] ^ 0xFF;
            Ok(data.iter().map(|b| b ^ xor).collect())
        }
        DatKind::V1 => {
            // 与 V2 同布局，固定 AES key；xor 仍用账号派生或尾部推断
            decrypt_v2_like(data, V1_AES_KEY, keys.xor_key)
        }
        DatKind::V2 => {
            let key = keys.aes_key.as_bytes();
            if key.len() != 16 {
                return Err(format!("aes_key 须为 16 字符 ASCII，实际 {:?}", keys.aes_key));
            }
            decrypt_v2_like(data, key, keys.xor_key)
        }
    }
}

fn decrypt_v2_like(data: &[u8], aes_key: &[u8], xor_key: u8) -> Result<Vec<u8>, String> {
    if data.len() < 15 {
        return Err("文件过短".into());
    }
    let aes_size = u32::from_le_bytes(data[6..10].try_into().unwrap()) as usize;
    let xor_size = u32::from_le_bytes(data[10..14].try_into().unwrap()) as usize;
    let expected = 15 + aes_size + 16 + xor_size;
    if data.len() != expected {
        return Err(format!(
            "V2 长度不符：file={} expected={} (aes={} xor={})",
            data.len(),
            expected,
            aes_size,
            xor_size
        ));
    }
    if aes_size == 0 || aes_size % 16 != 0 {
        return Err(format!("非法 aes_size={aes_size}"));
    }
    let head = aes128_ecb_decrypt(aes_key, &data[15..15 + aes_size])?;
    let tail_start = 15 + aes_size + 16;
    let tail: Vec<u8> = data[tail_start..tail_start + xor_size]
        .iter()
        .map(|b| b ^ xor_key)
        .collect();
    let mut out = head;
    out.extend_from_slice(&tail);
    Ok(out)
}

/// JPEG/PNG/GIF 尾部是否完整。wxgf 不能靠魔数判断异或是否正确。
fn container_complete(plain: &[u8]) -> bool {
    if plain.len() >= 4 && plain[0] == 0xFF && plain[1] == 0xD8 && plain[2] == 0xFF {
        return plain.len() >= 4 && plain[plain.len() - 2] == 0xFF && plain[plain.len() - 1] == 0xD9;
    }
    if plain.starts_with(b"\x89PNG\r\n\x1a\n") {
        return plain.len() >= 8 && plain.ends_with(b"IEND\xaeB`\x82");
    }
    if plain.starts_with(b"GIF8") {
        return plain.last() == Some(&0x3B);
    }
    false
}

/// `code & 0xFF` 有时是 0，但图片尾部实际用了别的异或（这份样本是 0x80）。
/// 用一张已能看出 JPEG/PNG 头的 V2 文件，把能还原完整文件尾的异或找出来。
fn xor_that_completes(data: &[u8], aes_key: &[u8], hinted: u8) -> Option<u8> {
    let ok = |xor: u8| {
        decrypt_v2_like(data, aes_key, xor)
            .ok()
            .is_some_and(|plain| container_complete(&plain))
    };
    if ok(hinted) {
        return Some(hinted);
    }
    (0u16..=255).map(|x| x as u8).find(|&x| x != hinted && ok(x))
}

fn find_jpeg_or_png_v2(wx_root: &Path, aes_key: &[u8]) -> Option<Vec<u8>> {
    let attach = wx_root.join("msg").join("attach");
    if !attach.is_dir() {
        return None;
    }
    let mut files = Vec::new();
    let _ = collect_suffix_files(&attach, "_t.dat", &mut files, 80);
    if files.is_empty() {
        let _ = collect_dat_samples(&attach, &mut files, 40);
    }
    for path in files {
        let Ok(data) = fs::read(&path) else {
            continue;
        };
        if !data.starts_with(V2_MAGIC) {
            continue;
        }
        // 异或只作用在 AES 段之后，头部魔数与异或无关
        let Ok(head) = decrypt_v2_like(&data, aes_key, 0) else {
            continue;
        };
        let jpeg = head.len() >= 3 && head[0] == 0xFF && head[1] == 0xD8 && head[2] == 0xFF;
        let png = head.starts_with(b"\x89PNG");
        if jpeg || png {
            return Some(data);
        }
    }
    None
}

fn collect_suffix_files(
    dir: &Path,
    suffix: &str,
    out: &mut Vec<PathBuf>,
    limit: usize,
) -> std::io::Result<()> {
    if out.len() >= limit {
        return Ok(());
    }
    for ent in fs::read_dir(dir)? {
        let ent = ent?;
        let path = ent.path();
        if path.is_dir() {
            collect_suffix_files(&path, suffix, out, limit)?;
        } else if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().ends_with(suffix))
        {
            out.push(path);
            if out.len() >= limit {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn scan_kvcomm_codes() -> Vec<u64> {
    let mut codes = Vec::new();
    let mut seen = HashSet::new();
    let Some(roaming) = dirs::data_dir() else {
        return codes;
    };
    // Windows: %APPDATA%\Tencent\xwechat\net*\kvcomm
    let base = roaming.join("Tencent").join("xwechat");
    if !base.is_dir() {
        return codes;
    }
    let Ok(entries) = fs::read_dir(&base) else {
        return codes;
    };
    for ent in entries.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if !(name == "net" || name.starts_with("net_")) {
            continue;
        }
        let kv = ent.path().join("kvcomm");
        if !kv.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(&kv) else {
            continue;
        };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            // key_574066560_... 或 key_reportnow_574066560_...
            if let Some(code) = parse_code_from_kvcomm_name(&fname) {
                if seen.insert(code) {
                    codes.push(code);
                }
            }
        }
    }
    codes
}

fn parse_code_from_kvcomm_name(name: &str) -> Option<u64> {
    // key_<code>_ 或 key_reportnow_<code>_
    let re = regex::Regex::new(r"(?:^key_|_key_)(?:reportnow_)?(\d{6,})_").ok()?;
    re.captures(name)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn account_candidates(wx_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.iter().any(|x| x == &s) {
            out.push(s);
        }
    };
    if let Some(name) = wx_root.file_name().and_then(|s| s.to_str()) {
        push(name.to_string());
        push(clean_account_name(name));
    }
    if let Some(roaming) = dirs::data_dir() {
        let login = roaming.join("Tencent").join("xwechat").join("login");
        if let Ok(entries) = fs::read_dir(login) {
            for ent in entries.flatten() {
                if ent.path().is_dir() {
                    if let Some(n) = ent.file_name().to_str() {
                        push(n.to_string());
                        push(clean_account_name(n));
                    }
                }
            }
        }
    }
    out
}

fn find_sample_v2_ct_block(wx_root: &Path) -> Option<[u8; 16]> {
    let attach = wx_root.join("msg").join("attach");
    if !attach.is_dir() {
        return None;
    }
    let mut samples: Vec<PathBuf> = Vec::new();
    let _ = collect_dat_samples(&attach, &mut samples, 40);
    samples.sort_by_cached_key(|p| {
        std::cmp::Reverse(
            p.metadata()
                .and_then(|m| m.modified())
                .ok()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        )
    });
    for p in samples {
        if let Ok(data) = fs::read(&p) {
            if data.len() >= 31 && data.starts_with(V2_MAGIC) {
                let mut block = [0u8; 16];
                block.copy_from_slice(&data[15..31]);
                return Some(block);
            }
        }
    }
    None
}

fn collect_dat_samples(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) -> std::io::Result<()> {
    if out.len() >= limit {
        return Ok(());
    }
    for ent in fs::read_dir(dir)? {
        let ent = ent?;
        let path = ent.path();
        if path.is_dir() {
            collect_dat_samples(&path, out, limit)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("dat"))
        {
            out.push(path);
            if out.len() >= limit {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// 从本机 kvcomm + 账号名派生并校验密钥。
pub fn resolve_keys_for_root(wx_root: &Path) -> Result<ImageKeys, String> {
    if !wx_root.is_dir() {
        return Err(format!("微信目录不存在: {}", wx_root.display()));
    }
    let codes = scan_kvcomm_codes();
    if codes.is_empty() {
        return Err(
            "未在 %APPDATA%\\Tencent\\xwechat\\net*\\kvcomm 找到 key_<code>_*.statistic"
                .into(),
        );
    }
    let accounts = account_candidates(wx_root);
    if accounts.is_empty() {
        return Err("无法推断账号名（目录名 / login 文件夹）".into());
    }
    let sample = find_sample_v2_ct_block(wx_root)
        .ok_or_else(|| "未找到可用于校验的 V2 .dat 样例".to_string())?;

    for code in &codes {
        for acc in &accounts {
            let mut keys = derive_keys(*code, acc);
            if verify_aes_key(keys.aes_key.as_bytes(), &sample) {
                if let Some(data) = find_jpeg_or_png_v2(wx_root, keys.aes_key.as_bytes()) {
                    if let Some(xor) =
                        xor_that_completes(&data, keys.aes_key.as_bytes(), keys.xor_key)
                    {
                        keys.xor_key = xor;
                    }
                }
                return Ok(keys);
            }
        }
    }
    Err(format!(
        "密钥派生未命中（试了 {} 个 code × {} 个账号名）。请确认本机已登录对应微信账号",
        codes.len(),
        accounts.len()
    ))
}

pub fn keys_to_map(keys: &ImageKeys) -> Map {
    let mut m = Map::new();
    m.insert("aes_key".into(), Dynamic::from(keys.aes_key.clone()));
    m.insert("xor_key".into(), Dynamic::from(keys.xor_key as i64));
    m.insert("account".into(), Dynamic::from(keys.account.clone()));
    m.insert("code".into(), Dynamic::from(keys.code as i64));
    // 旧缓存只校验了 AES，异或可能是错的（code 低字节为 0 时常见）
    m.insert("xor_checked".into(), Dynamic::from(1_i64));
    m
}

pub fn keys_from_map(m: &Map) -> Result<ImageKeys, String> {
    let aes_key = m
        .get("aes_key")
        .map(|v| v.to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "缺少 aes_key".to_string())?;
    let xor_key = m
        .get("xor_key")
        .map(|v| {
            if let Ok(i) = v.as_int() {
                i as u8
            } else if let Ok(f) = v.as_float() {
                f as u8
            } else {
                v.to_string().parse::<u64>().unwrap_or(0) as u8
            }
        })
        .ok_or_else(|| "缺少 xor_key".to_string())?;
    let account = m
        .get("account")
        .map(|v| v.to_string())
        .unwrap_or_default();
    let code = m
        .get("code")
        .map(|v| {
            if let Ok(i) = v.as_int() {
                i as u64
            } else if let Ok(f) = v.as_float() {
                f as u64
            } else {
                v.to_string().parse().unwrap_or(0)
            }
        })
        .unwrap_or(0);
    if aes_key.len() != 16 {
        return Err(format!("aes_key 须 16 字符，实际 {}", aes_key.len()));
    }
    Ok(ImageKeys {
        aes_key,
        xor_key,
        account,
        code,
    })
}

pub fn load_keys_json(path: &Path) -> Result<ImageKeys, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("读取密钥文件失败: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("解析密钥 JSON 失败: {e}"))
}

pub fn save_keys_json(path: &Path, keys: &ImageKeys) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(keys).map_err(|e| e.to_string())?;
    fs::write(path, raw).map_err(|e| format!("写入密钥失败: {e}"))
}

/// 从 `.dat` 路径向上推断 `xwechat_files/<账号>` 根目录。
pub fn infer_wx_root(dat_path: &Path) -> Result<PathBuf, String> {
    let mut cur = dat_path.to_path_buf();
    if cur.is_file() {
        cur = cur
            .parent()
            .ok_or_else(|| "路径无父目录".to_string())?
            .to_path_buf();
    }
    for _ in 0..12 {
        let name = cur
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        // 账号目录通常含 msg/ attach 或 cache 子目录
        if cur.join("msg").is_dir() || cur.join("cache").is_dir() {
            return Ok(cur);
        }
        // 也可识别 xwechat_files 的直接子目录
        if let Some(parent) = cur.parent() {
            if parent
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("xwechat_files"))
                && !name.is_empty()
            {
                return Ok(cur);
            }
            cur = parent.to_path_buf();
        } else {
            break;
        }
    }
    Err(format!(
        "无法从路径推断微信数据根目录: {}",
        dat_path.display()
    ))
}

/// 解密单个 .dat，写入 `dest`（目录或完整文件路径）。
/// 返回实际写出路径。wxgf 优先用本机微信 VoipEngine 转成 jpg/png。
pub fn decrypt_dat_to_file(src: &Path, dest: &Path, keys: &ImageKeys) -> Result<PathBuf, String> {
    let data = fs::read(src).map_err(|e| format!("读取失败 {}: {e}", src.display()))?;
    let plain = decrypt_bytes(&data, keys)?;
    let ext = image_ext(&plain).ok_or_else(|| {
        format!(
            "解密后无法识别图像格式（head={}）",
            plain
                .iter()
                .take(8)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join("")
        )
    })?;
    let (bytes, ext) = if ext == "wxgf" {
        transcode_wxgf(&plain)?
    } else {
        (plain, ext)
    };

    let out_path = if dest_is_dir_hint(dest) {
        fs::create_dir_all(dest).map_err(|e| format!("创建目录失败: {e}"))?;
        let stem = src
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image");
        let stem = stem.trim_end_matches("_h").trim_end_matches("_t");
        dest.join(format!("{stem}.{ext}"))
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        dest.to_path_buf()
    };

    fs::write(&out_path, &bytes).map_err(|e| format!("写入失败 {}: {e}", out_path.display()))?;
    Ok(out_path)
}

/// 把解密后的 wxgf 转成 jpg/png（本机微信 VoipEngine）。
fn transcode_wxgf(data: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    super::wxgf_voip::transcode(data)
}

fn dest_is_dir_hint(dest: &Path) -> bool {
    if dest.exists() {
        return dest.is_dir();
    }
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    // 无扩展名 → 当作目录
    !name.contains('.')
}

/// Rhai: 解析密钥
pub fn rhai_resolve_image_key(wx_root: &str) -> Result<Map, Box<EvalAltResult>> {
    let root = PathBuf::from(wx_root);
    let keys = resolve_keys_for_root(&root).map_err(err)?;
    Ok(keys_to_map(&keys))
}

/// Rhai: 用显式密钥解密
pub fn rhai_dat_decrypt_with_keys(
    src: &str,
    dest: &str,
    keys_map: Map,
) -> Result<String, Box<EvalAltResult>> {
    let keys = keys_from_map(&keys_map).map_err(err)?;
    let out = decrypt_dat_to_file(Path::new(src), Path::new(dest), &keys).map_err(err)?;
    Ok(out.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_lonk1989_matches_known() {
        // 本机实测：code=574066560 account=Lonk1989 → a657a64124075eb3 / xor=0x80
        let k = derive_keys(574066560, "Lonk1989");
        assert_eq!(k.aes_key, "a657a64124075eb3");
        assert_eq!(k.xor_key, 0x80);
    }

    #[test]
    fn xor_zero_hint_recovers_real_tail_xor() {
        // code 低字节为 0 时派生异或是 0，但尾部实际异或可以是 0x80
        let key = b"5756411b46708b3a";
        let mut plain = vec![0xFF, 0xD8, 0xFF, 0xE0];
        plain.resize(16 + 48, 0x11);
        plain.extend_from_slice(&[0xFF, 0xD9]);
        let xor = 0x80u8;
        let ct_head = aes128_ecb_encrypt(key, &plain[..16]).unwrap();
        let ct_tail: Vec<u8> = plain[16..].iter().map(|b| b ^ xor).collect();
        let mut data = Vec::new();
        data.extend_from_slice(V2_MAGIC);
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&(ct_tail.len() as u32).to_le_bytes());
        data.push(1);
        data.extend_from_slice(&ct_head);
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&ct_tail);
        assert_eq!(xor_that_completes(&data, key, 0), Some(0x80));
        assert_eq!(xor_that_completes(&data, key, 0x80), Some(0x80));
    }

    #[test]
    fn clean_account_strips_short_suffix() {
        assert_eq!(clean_account_name("Lonk1989_ebe2"), "Lonk1989");
        assert_eq!(clean_account_name("wxid_abc_1234"), "wxid_abc");
    }

    #[test]
    fn parse_kvcomm_code() {
        assert_eq!(
            parse_code_from_kvcomm_name("key_574066560_4065599242_1_x.statistic"),
            Some(574066560)
        );
        assert_eq!(
            parse_code_from_kvcomm_name("key_reportnow_574066560_4065599242_1_x.statistic"),
            Some(574066560)
        );
    }

    #[test]
    fn decrypt_real_thumb_if_present() {
        let p = PathBuf::from(
            r"E:\Documents\xwechat_files\Lonk1989_ebe2\msg\attach\195805f617d44848bbd089d5e02fe180\2026-09\Img\a20c96210b49494e9a9da36ffb3a8858_t.dat",
        );
        if !p.is_file() {
            return;
        }
        let keys = derive_keys(574066560, "Lonk1989");
        let data = fs::read(&p).unwrap();
        let plain = decrypt_bytes(&data, &keys).unwrap();
        assert_eq!(image_ext(&plain), Some("jpg"));
        assert!(plain.ends_with(&[0xFF, 0xD9]));
    }

    #[test]
    fn transcode_wxgf_dat_if_present() {
        let root = PathBuf::from(r"E:\Documents\xwechat_files\Lonk1989_ebe2\msg\attach");
        if !root.is_dir() {
            return;
        }
        let keys = derive_keys(574066560, "Lonk1989");
        let mut scanned = 0usize;
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if !name.to_ascii_lowercase().ends_with(".dat") || name.contains("_t.dat") {
                    continue;
                }
                scanned += 1;
                if scanned > 250 {
                    return;
                }
                let Ok(data) = fs::read(&p) else { continue };
                let Ok(plain) = decrypt_bytes(&data, &keys) else { continue };
                if image_ext(&plain) != Some("wxgf") {
                    continue;
                }
                let (img, ext) = transcode_wxgf(&plain).expect("wxgf transcode");
                assert!(ext == "jpg" || ext == "png", "unexpected ext {ext}");
                assert!(image_ext(&img) == Some(ext));
                println!("wxgf -> {ext} {} bytes from {}", img.len(), p.display());
                return;
            }
        }
    }
}
