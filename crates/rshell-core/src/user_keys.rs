//! Managed private keys use platform-protected encryption; only public properties reach the UI.
use anyhow::{Context, Result, bail};
use russh::keys::{Algorithm, HashAlg, PrivateKey, ssh_key};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"RSHELL-KEY-1\0";
const MAX_SIZE: u64 = 65536;

#[derive(Clone)]
pub struct KeyInfo {
    pub path: PathBuf,
    pub name: String,
    pub algorithm: String,
    pub bits: u32,
    pub fingerprint: String,
    pub public_key: String,
}

fn read_bounded(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    fs::File::open(path)?
        .take(MAX_SIZE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SIZE {
        bail!("密钥文件超过 64 KiB 上限");
    }
    Ok(bytes)
}

pub fn load(path: impl AsRef<Path>, password: Option<&str>) -> Result<PrivateKey> {
    let bytes = read_bounded(path.as_ref())?;
    if bytes.starts_with(MAGIC) {
        let rest = &bytes[MAGIC.len()..];
        if rest.len() < 16 {
            bail!("密钥文件损坏");
        }
        let id = Uuid::from_slice(&rest[..16])?;
        let plain = crate::credentials::crypt(id, &rest[16..], false)?;
        return Ok(PrivateKey::from_openssh(&*plain)?);
    }
    let text = std::str::from_utf8(&bytes).context("私钥必须是 OpenSSH 或 PEM 文本格式")?;
    Ok(russh::keys::decode_secret_key(text, password)?)
}

pub fn generate(name: &str, rsa_bits: Option<usize>) -> Result<PrivateKey> {
    let mut rng = rand::rng();
    let mut key = if let Some(bits) = rsa_bits {
        if ![2048, 3072, 4096].contains(&bits) {
            bail!("不支持的 RSA 长度");
        }
        PrivateKey::new(
            ssh_key::private::RsaKeypair::random(&mut rng, bits)?.into(),
            name,
        )?
    } else {
        PrivateKey::random(&mut rng, Algorithm::Ed25519)?
    };
    key.set_comment(name);
    Ok(key)
}

pub fn save(directory: &Path, key: &PrivateKey, name: &str) -> Result<PathBuf> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        bail!("请输入 1–256 字节的名称，且不能包含控制字符");
    }
    let mut key = key.clone();
    key.set_comment(name.trim());
    let plain = key.to_openssh(ssh_key::LineEnding::LF)?;
    let id = Uuid::new_v4();
    let encrypted = crate::credentials::crypt(id, plain.as_bytes(), true)?;
    fs::create_dir_all(directory)?;
    let path = directory.join(format!("{id}.rkey"));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(MAGIC)?;
        file.write_all(id.as_bytes())?;
        file.write_all(&encrypted)?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result?;
    Ok(path)
}

pub fn list(directory: &Path) -> Result<Vec<KeyInfo>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut keys = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "rkey") {
            continue;
        }
        if keys.len() >= 1000 {
            bail!("用户密钥数量超过 1000 上限");
        }
        let key = load(&path, None).context("读取管理的用户密钥失败")?;
        let bits = match key.public_key().key_data() {
            ssh_key::public::KeyData::Rsa(rsa) => rsa.key_size(),
            ssh_key::public::KeyData::Ed25519(_) => 256,
            _ => 0,
        };
        keys.push(KeyInfo {
            path,
            name: key.comment().to_string(),
            algorithm: key.algorithm().to_string(),
            bits,
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            public_key: key.public_key().to_openssh()?,
        });
    }
    keys.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(keys)
}

pub fn export_private(source: &Path, destination: &Path, password: &str) -> Result<()> {
    if password.is_empty() {
        bail!("导出私钥必须设置保护口令");
    }
    let key = load(source, None)?.encrypt(&mut rand::rng(), password)?;
    let pem = key.to_openssh(ssh_key::LineEnding::LF)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .context("导出目标必须是不存在的新文件")?;
    file.write_all(pem.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rsa_generation_and_input_limits() {
        let key = generate("rsa-test", Some(2048)).unwrap();
        assert_eq!(key.public_key().key_data().rsa().unwrap().key_size(), 2048);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/key-tests");
        fs::create_dir_all(&root).unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        assert!(save(dir.path(), &key, "").is_err());
        assert!(save(dir.path(), &key, "bad\nname").is_err());
        let path = save(dir.path(), &key, "rsa-test").unwrap();
        assert_eq!(list(dir.path()).unwrap()[0].bits, 2048);
        assert_eq!(load(&path, None).unwrap().public_key(), key.public_key());
        fs::write(&path, vec![0; MAX_SIZE as usize + 1]).unwrap();
        assert!(load(&path, None).is_err());
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "requires unlocked macOS Keychain")]
    fn managed_keys_roundtrip() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/key-tests");
        fs::create_dir_all(&root).unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        let key = generate("test key", None).unwrap();
        let path = save(dir.path(), &key, "test key").unwrap();
        assert!(!fs::read(&path).unwrap().windows(7).any(|w| w == b"PRIVATE"));
        assert_eq!(load(&path, None).unwrap().public_key(), key.public_key());
        assert_eq!(list(dir.path()).unwrap()[0].bits, 256);
        let export = dir.path().join("export.key");
        assert!(export_private(&path, &export, "").is_err());
        export_private(&path, &export, "test-only").unwrap();
        assert!(load(&export, Some("wrong")).is_err());
        assert_eq!(
            load(&export, Some("test-only")).unwrap().public_key(),
            key.public_key()
        );
        assert!(export_private(&path, &export, "test-only").is_err());
        fs::write(&path, MAGIC).unwrap();
        assert!(load(&path, None).is_err());
        assert!(generate("x", Some(1024)).is_err());
    }
}
