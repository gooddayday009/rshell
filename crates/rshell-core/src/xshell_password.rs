//! Xshell compatibility decoder. Independent Rust implementation of the documented
//! RC4 + SHA-256 format (HyperSine/how-does-Xmanager-encrypt-password and
//! JDArmy/SharpXDecrypt). RC4 is ONLY for reading legacy Xshell credentials.
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Default)]
pub struct Options {
    pub master: Zeroizing<String>,
    pub username: String,
    pub sid: String,
    pub config_only: bool,
    pub allow_partial: bool,
}

pub fn supported(version: &str) -> bool {
    matches!(
        version,
        "5.1" | "5.2" | "5.3" | "6.0" | "7.0" | "7.1" | "8.0" | "8.1"
    )
}

pub fn decrypt(version: &str, encoded: &str, options: &Options) -> Result<Zeroizing<String>> {
    ensure!(
        options.master.len() <= 16384
            && options.username.len() <= 1024
            && options.sid.len() <= 1024,
        "密码或来源用户信息过长"
    );
    ensure!(supported(version), "不支持的 Xshell 密码版本");
    ensure!(encoded.len() <= 32768, "Xshell 密码字段过长");
    let data = Zeroizing::new(
        STANDARD
            .decode(encoded)
            .map_err(|_| anyhow::anyhow!("Xshell 密码编码无效"))?,
    );
    ensure!(
        data.len() > 32 && data.len() <= 16416,
        "Xshell 密码长度无效"
    );
    let material = Zeroizing::new(if !options.master.is_empty() {
        options.master.to_string()
    } else {
        ensure!(
            !options.sid.is_empty(),
            "需要来源 Windows 用户信息或 Xshell 主密码"
        );
        match version {
            "5.1" | "5.2" => options.sid.clone(),
            "5.3" | "6.0" | "7.0" => format!("{}{}", options.username, options.sid),
            _ => format!(
                "{}{}",
                options.sid.chars().rev().collect::<String>(),
                options.username
            ),
        }
    });
    // Xshell Windows versions use the system ANSI code page. Try UTF-8 too,
    // accepting a candidate ONLY when the stored SHA-256 digest matches.
    let utf8 = Zeroizing::new(material.as_bytes().to_vec());
    let mut candidates = vec![utf8];
    if let Some(ansi) = ansi_encode(&material) {
        candidates.push(ansi);
    }
    for material in candidates {
        let key = Zeroizing::new(Sha256::digest(&material).to_vec());
        let plain = rc4(&key, &data[..data.len() - 32]);
        if Sha256::digest(&plain)[..] == data[data.len() - 32..] {
            if let Ok(text) = std::str::from_utf8(&plain) {
                return Ok(Zeroizing::new(text.to_owned()));
            }
            if let Some(text) = ansi_decode(&plain) {
                return Ok(text);
            }
        }
    }
    anyhow::bail!("密码校验失败：主密码、来源用户信息不匹配或文件损坏")
}

fn rc4(key: &[u8], input: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut state = Zeroizing::new([0u8; 256]);
    for (i, value) in state.iter_mut().enumerate() {
        *value = i as u8;
    }
    let mut j = 0;
    for i in 0..256 {
        j = (j + state[i] as usize + key[i % key.len()] as usize) % 256;
        state.swap(i, j);
    }
    let (mut i, mut j) = (0, 0);
    let mut output = Zeroizing::new(Vec::with_capacity(input.len()));
    for byte in input {
        i = (i + 1) % 256;
        j = (j + state[i] as usize) % 256;
        state.swap(i, j);
        output.push(byte ^ state[(state[i] as usize + state[j] as usize) % 256]);
    }
    output
}

#[cfg(windows)]
fn ansi_encode(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    use windows_sys::Win32::Globalization::{CP_ACP, WideCharToMultiByte};
    let units = Zeroizing::new(text.encode_utf16().collect::<Vec<_>>());
    let mut used_default = 0;
    let mut bytes = Zeroizing::new(vec![0; units.len() * 4 + 4]);
    let n = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            0,
            units.as_ptr(),
            units.len() as i32,
            bytes.as_mut_ptr(),
            bytes.len() as i32,
            std::ptr::null(),
            &mut used_default,
        )
    };
    if n <= 0 || used_default != 0 {
        return None;
    }
    bytes.truncate(n as usize);
    Some(bytes)
}
#[cfg(windows)]
fn ansi_decode(bytes: &[u8]) -> Option<Zeroizing<String>> {
    use windows_sys::Win32::Globalization::{CP_ACP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    let mut units = Zeroizing::new(vec![0; bytes.len() + 1]);
    let n = unsafe {
        MultiByteToWideChar(
            CP_ACP,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            bytes.len() as i32,
            units.as_mut_ptr(),
            units.len() as i32,
        )
    };
    if n <= 0 {
        return None;
    }
    String::from_utf16(&units[..n as usize])
        .ok()
        .map(Zeroizing::new)
}
#[cfg(not(windows))]
fn ansi_encode(_: &str) -> Option<Zeroizing<Vec<u8>>> {
    None
}
#[cfg(not(windows))]
fn ansi_decode(_: &[u8]) -> Option<Zeroizing<String>> {
    None
}

/// Resolve only the current Windows account; never enumerate other accounts.
#[cfg(windows)]
pub fn current_identity() -> Option<(String, String)> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{Authorization::ConvertSidToStringSidW, LookupAccountNameW},
        System::WindowsProgramming::GetUserNameW,
    };
    let mut name = [0u16; 257];
    let mut len = name.len() as u32;
    if unsafe { GetUserNameW(name.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let username = String::from_utf16(&name[..len as usize - 1]).ok()?;
    let mut sid = [0u64; 32];
    let mut sid_len = std::mem::size_of_val(&sid) as u32;
    let mut domain = [0u16; 257];
    let mut domain_len = domain.len() as u32;
    let mut kind = 0;
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    } == 0
    {
        return None;
    }
    let mut ptr = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut ptr) } == 0 {
        return None;
    }
    let result = unsafe {
        let mut n = 0;
        while *ptr.add(n) != 0 {
            n += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(ptr, n)).ok();
        LocalFree(ptr.cast());
        result
    }?;
    Some((username, result))
}
#[cfg(not(windows))]
pub fn current_identity() -> Option<(String, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn published_master_vector_and_wrong_password() {
        let mut opts = Options {
            master: Zeroizing::new("123123".into()),
            ..Default::default()
        };
        let vector = "Rrm3P3AL0iDV7nBbS2bHvh7ZAvuN1NSJl8ZFL11+UJ+82+KAixa89O3OTAfRTg==";
        assert!(decrypt("6.0", vector, &opts).unwrap().as_str() == "This is a test");
        opts.master = Zeroizing::new("wrong".into());
        assert!(decrypt("6.0", vector, &opts).is_err());
        assert!(decrypt("9.0", vector, &opts).is_err());
        assert!(decrypt("7.1", "AA==", &opts).is_err());
    }
    #[test]
    fn modern_versions_unicode_and_tampering() {
        let options = Options {
            username: "user".into(),
            sid: "S-1-5-test".into(),
            ..Default::default()
        };
        let key = Sha256::digest(b"tset-5-1-Suser");
        let secret = "测试 password";
        let mut data = rc4(&key, secret.as_bytes());
        data.extend_from_slice(&Sha256::digest(secret.as_bytes()));
        for version in ["7.1", "8.1"] {
            assert!(
                decrypt(version, &STANDARD.encode(&data), &options)
                    .unwrap()
                    .as_str()
                    == secret
            );
        }
        data[0] ^= 1;
        assert!(decrypt("8.1", &STANDARD.encode(&data), &options).is_err());
    }
}
