//! Portable password-protected backup. Plaintext secrets are binary fields, never JSON.
use super::*;
use crate::xts::Secrets;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{AeadInOut, KeyInit},
};
use argon2::{Algorithm, Argon2, Params, Version};
use std::{collections::HashMap, io::Read};
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"RSHELL-ENCRYPTED-1\0";
const LIMIT: usize = 64 * 1024 * 1024;
const HEADER: usize = MAGIC.len() + 12 + 16 + 12;
const PARAMS: [u32; 3] = [65536, 3, 1];

pub fn is_encrypted(path: &Path) -> Result<bool> {
    let mut file = fs::File::open(path).context("无法打开会话文件")?;
    let mut prefix = vec![0; MAGIC.len()];
    let n = file.read(&mut prefix)?;
    Ok(n == MAGIC.len() && prefix == MAGIC)
}
fn key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    ensure!(
        !password.is_empty() && password.len() <= 16384,
        "请输入有效的导出口令（最长 16384 字节）"
    );
    let mut key = Zeroizing::new([0; 32]);
    let params = Params::new(PARAMS[0], PARAMS[1], PARAMS[2], Some(32))
        .map_err(|_| anyhow::anyhow!("密钥参数无效"))?;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut *key)
        .map_err(|_| anyhow::anyhow!("密钥派生失败"))?;
    Ok(key)
}

pub fn export(path: &Path, profiles: &[Profile], secrets: &Secrets, password: &str) -> Result<()> {
    check_extension(path)?;
    ensure!(
        !profiles.is_empty() && profiles.len() <= 4096,
        "会话数量须为 1 至 4096 个"
    );
    let public = serde_json::to_vec(&SessionCollection {
        format: "RShell sessions".into(),
        version: 2,
        sessions: profiles.iter().map(session_file).collect::<Result<_>>()?,
    })?;
    ensure!(public.len() <= MAX_FILE as usize, "会话配置过大");
    let by_id: HashMap<_, _> = secrets.iter().map(|(id, s)| (*id, s)).collect();
    let size =
        4 + public.len() + profiles.len() * 4 + secrets.iter().map(|(_, s)| s.len()).sum::<usize>();
    ensure!(size + HEADER + 16 <= LIMIT, "加密备份超过容量限制");
    let mut payload = Zeroizing::new(Vec::with_capacity(size + 16));
    payload.extend_from_slice(&(public.len() as u32).to_le_bytes());
    payload.extend_from_slice(&public);
    for profile in profiles {
        if let Some(secret) = by_id.get(&profile.id) {
            ensure!(
                profile.protocol == Protocol::Ssh
                    && profile.auth == AuthMethod::Password
                    && secret.len() <= 16384,
                "不支持导出的凭据类型或长度"
            );
            payload.extend_from_slice(&(secret.len() as u32).to_le_bytes());
            payload.extend_from_slice(secret.as_bytes());
        } else {
            payload.extend_from_slice(&u32::MAX.to_le_bytes());
        }
    }
    let salt: [u8; 16] = rand::random();
    let nonce: [u8; 12] = rand::random();
    let mut header = MAGIC.to_vec();
    for value in PARAMS {
        header.extend_from_slice(&value.to_le_bytes());
    }
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce);
    let derived = key(password, &salt)?;
    let cipher =
        Aes256Gcm::new_from_slice(&*derived).map_err(|_| anyhow::anyhow!("加密初始化失败"))?;
    cipher
        .encrypt_in_place(&Nonce::from(nonce), &header, &mut *payload)
        .map_err(|_| anyhow::anyhow!("备份加密失败"))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).context("无法创建导出临时文件")?;
    file.write_all(&header)?;
    file.write_all(&payload)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|_| anyhow::anyhow!("无法保存备份；目标已存在或目录不可写"))?;
    Ok(())
}

pub fn import(path: &Path, password: &str) -> Result<(Vec<Profile>, Secrets)> {
    check_extension(path)?;
    let file = fs::File::open(path).context("无法打开加密备份")?;
    ensure!(
        file.metadata()?.len() <= LIMIT as u64,
        "加密备份超过容量限制"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= LIMIT && bytes.len() >= HEADER + 16 && bytes.starts_with(MAGIC),
        "加密备份格式无效或版本不支持"
    );
    let mut at = MAGIC.len();
    for expected in PARAMS {
        ensure!(
            u32::from_le_bytes(bytes[at..at + 4].try_into()?) == expected,
            "不支持的密钥派生参数"
        );
        at += 4;
    }
    let derived = key(password, &bytes[at..at + 16])?;
    at += 16;
    let nonce: [u8; 12] = bytes[at..at + 12].try_into()?;
    let header = bytes[..HEADER].to_vec();
    let mut payload = Zeroizing::new(bytes[HEADER..].to_vec());
    let cipher =
        Aes256Gcm::new_from_slice(&*derived).map_err(|_| anyhow::anyhow!("解密初始化失败"))?;
    cipher
        .decrypt_in_place(&Nonce::from(nonce), &header, &mut *payload)
        .map_err(|_| anyhow::anyhow!("口令错误或备份已损坏"))?;
    let mut reader = payload.as_slice();
    let public_len = number(&mut reader)? as usize;
    ensure!(
        public_len <= MAX_FILE as usize && public_len <= reader.len(),
        "备份配置长度无效"
    );
    let collection: SessionCollection = serde_json::from_slice(&reader[..public_len])
        .map_err(|_| anyhow::anyhow!("备份配置无效"))?;
    reader = &reader[public_len..];
    ensure!(
        collection.format == "RShell sessions"
            && collection.version == 2
            && !collection.sessions.is_empty()
            && collection.sessions.len() <= 4096,
        "备份配置版本或数量无效"
    );
    let mut profiles = Vec::new();
    let mut secrets = Vec::new();
    for session in collection.sessions {
        let mut profile = into_profile(session)?;
        let length = number(&mut reader)?;
        if length != u32::MAX {
            let length = length as usize;
            ensure!(
                length <= 16384
                    && length <= reader.len()
                    && profile.protocol == Protocol::Ssh
                    && profile.auth == AuthMethod::Password,
                "备份凭据字段无效"
            );
            let text = std::str::from_utf8(&reader[..length])
                .map_err(|_| anyhow::anyhow!("备份凭据编码无效"))?;
            secrets.push((profile.id, Zeroizing::new(text.to_owned())));
            profile.remember_secret = true;
            reader = &reader[length..];
        }
        profiles.push(profile);
    }
    ensure!(reader.is_empty(), "备份含多余数据");
    Ok((profiles, secrets))
}
fn number(reader: &mut &[u8]) -> Result<u32> {
    ensure!(reader.len() >= 4, "备份数据不完整");
    let n = u32::from_le_bytes(reader[..4].try_into()?);
    *reader = &reader[4..];
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encrypted_roundtrip_randomization_wrong_password_and_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rsh");
        let b = dir.path().join("b.rsh");
        let profile = Profile {
            host: "server.test".into(),
            remember_secret: true,
            ..Default::default()
        };
        let id = profile.id;
        let secrets = vec![(id, Zeroizing::new("中文 secret".into()))];
        export(
            &a,
            std::slice::from_ref(&profile),
            &secrets,
            "export password",
        )
        .unwrap();
        export(&b, &[profile], &secrets, "export password").unwrap();
        assert!(is_encrypted(&a).unwrap());
        assert_ne!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
        let (profiles, restored) = import(&a, "export password").unwrap();
        assert_ne!(profiles[0].id, id);
        assert_eq!(restored[0].0, profiles[0].id);
        assert!(restored[0].1.as_str() == "中文 secret");
        assert!(profiles[0].remember_secret);
        assert!(import(&a, "wrong").is_err());
        let mut data = fs::read(&a).unwrap();
        data[HEADER] ^= 1;
        fs::write(&b, &data).unwrap();
        assert!(import(&b, "export password").is_err());
        data[MAGIC.len()] ^= 1;
        fs::write(&b, &data).unwrap();
        assert!(import(&b, "export password").is_err());
        fs::write(&b, &data[..HEADER - 1]).unwrap();
        assert!(import(&b, "export password").is_err());
        assert!(export(&a, &profiles, &restored, "export password").is_err());
    }
}
