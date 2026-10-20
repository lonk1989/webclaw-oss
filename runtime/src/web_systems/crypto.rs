//! 网站账号密码落盘加密。
//! Windows：DPAPI（绑定当前 Windows 登录用户，无法在其他机器/账号直接解密）
//! 其他平台：SHA256 密钥流混淆（弱于 DPAPI，仍避免明文）

use crate::paths;
use base64::Engine;
use sha2::{Digest, Sha256};

const ENC_PREFIX: &str = "wcdpapi:v1:";
const XOR_PREFIX: &str = "wcxor:v1:";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

#[cfg(windows)]
fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut plain_buf = plain.to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: plain_buf.len() as u32,
        pbData: plain_buf.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        CryptProtectData(
            &in_blob,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
        .map_err(|e| format!("DPAPI 加密失败: {e}"))?;
        if out_blob.pbData.is_null() || out_blob.cbData == 0 {
            return Err("DPAPI 加密返回空数据".into());
        }
        let sealed =
            std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out_blob.pbData as *mut _));
        Ok(sealed)
    }
}

#[cfg(windows)]
fn dpapi_unprotect(sealed: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut sealed_buf = sealed.to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: sealed_buf.len() as u32,
        pbData: sealed_buf.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        CryptUnprotectData(
            &in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
        .map_err(|e| format!("DPAPI 解密失败: {e}"))?;
        if out_blob.pbData.is_null() || out_blob.cbData == 0 {
            return Err("DPAPI 解密返回空数据".into());
        }
        let plain =
            std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out_blob.pbData as *mut _));
        Ok(plain)
    }
}

fn xor_keystream(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut counter: u64 = 0;
    while out.len() < len {
        let mut hasher = Sha256::new();
        hasher.update(b"webclaw.web-systems.xor.v1");
        hasher.update(paths::current_user_key().as_bytes());
        if let Ok(name) = std::env::var("COMPUTERNAME") {
            hasher.update(name.as_bytes());
        } else if let Ok(name) = std::env::var("HOSTNAME") {
            hasher.update(name.as_bytes());
        }
        hasher.update(counter.to_le_bytes());
        out.extend_from_slice(&hasher.finalize());
        counter = counter.wrapping_add(1);
    }
    out.truncate(len);
    out
}

fn xor_obfuscate(plain: &[u8]) -> Vec<u8> {
    let ks = xor_keystream(plain.len());
    plain.iter().zip(ks.iter()).map(|(a, b)| a ^ b).collect()
}

/// 判断磁盘字段是否已是加密格式
pub fn is_encrypted_password(stored: &str) -> bool {
    stored.starts_with(ENC_PREFIX) || stored.starts_with(XOR_PREFIX)
}

/// 明文 → 落盘密文（空串保持空串）
pub fn encrypt_password(plain: &str) -> Result<String, String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    if is_encrypted_password(plain) {
        return Ok(plain.to_string());
    }
    #[cfg(windows)]
    {
        let sealed = dpapi_protect(plain.as_bytes())?;
        return Ok(format!("{ENC_PREFIX}{}", b64().encode(sealed)));
    }
    #[cfg(not(windows))]
    {
        let sealed = xor_obfuscate(plain.as_bytes());
        Ok(format!("{XOR_PREFIX}{}", b64().encode(sealed)))
    }
}

/// 落盘字段 → 明文（兼容旧版明文）
pub fn decrypt_password(stored: &str) -> Result<String, String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    if let Some(b64_part) = stored.strip_prefix(ENC_PREFIX) {
        let packed = b64()
            .decode(b64_part)
            .map_err(|e| format!("密码密文解码失败: {e}"))?;
        #[cfg(windows)]
        {
            let plain = dpapi_unprotect(&packed)?;
            return String::from_utf8(plain).map_err(|e| format!("密码明文非 UTF-8: {e}"));
        }
        #[cfg(not(windows))]
        {
            return Err("当前平台无法解密 Windows DPAPI 密文".into());
        }
    }
    if let Some(b64_part) = stored.strip_prefix(XOR_PREFIX) {
        let packed = b64()
            .decode(b64_part)
            .map_err(|e| format!("密码密文解码失败: {e}"))?;
        let plain = xor_obfuscate(&packed);
        return String::from_utf8(plain).map_err(|e| format!("密码明文非 UTF-8: {e}"));
    }
    // 旧版明文
    Ok(stored.to_string())
}
