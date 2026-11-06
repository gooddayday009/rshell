//! Keychain-protected AES-GCM files. Called only by blocking workers.
use anyhow::{Context, Result, bail};
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use security_framework::os::macos::keychain::SecKeychain;
use uuid::Uuid;
use zeroize::Zeroizing;

const SERVICE: &str = "com.RShell.RShell.file-encryption";
const ACCOUNT: &str = "master-key-v1";
const MAGIC: &[u8] = b"RSHELL-MAC-1\0";

fn master_key(create: bool) -> Result<Zeroizing<Vec<u8>>> {
    let keychain = SecKeychain::default().context("打开 macOS 钥匙串失败")?;
    match keychain.find_generic_password(SERVICE, ACCOUNT) {
        Ok((value, _)) => return Ok(Zeroizing::new(value.to_vec())),
        Err(error) if error.code() == -25300 && create => {}
        Err(error) => return Err(error).context("读取 macOS 加密密钥失败"),
    }
    let mut key = Zeroizing::new(vec![0; 32]);
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow::anyhow!("生成加密密钥失败"))?;
    // Add-only: a concurrent CLI/GUI must never overwrite an existing master key.
    match keychain.add_generic_password(SERVICE, ACCOUNT, &key) {
        Ok(()) => Ok(key),
        Err(error) if error.code() == -25299 => {
            let (value, _) = keychain.find_generic_password(SERVICE, ACCOUNT)?;
            Ok(Zeroizing::new(value.to_vec()))
        }
        Err(error) => Err(error).context("保存 macOS 加密密钥失败"),
    }
}

pub(crate) fn crypt(id: Uuid, bytes: &[u8], protect: bool) -> Result<Zeroizing<Vec<u8>>> {
    if !protect && (!bytes.starts_with(MAGIC) || bytes.len() < MAGIC.len() + 12 + 16) {
        bail!("不是有效的 macOS 加密文件；Windows DPAPI 文件不能直接迁移");
    }
    let secret = master_key(protect)?;
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, &secret)
            .map_err(|_| anyhow::anyhow!("macOS 加密密钥格式无效"))?,
    );
    let aad = aead::Aad::from(id.as_bytes());
    if protect {
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("生成加密随机数失败"))?;
        let mut data = Zeroizing::new(bytes.to_vec());
        key.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), aad, &mut *data)
            .map_err(|_| anyhow::anyhow!("凭据加密失败"))?;
        let mut result = MAGIC.to_vec();
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&data);
        Ok(Zeroizing::new(result))
    } else {
        let nonce = bytes[MAGIC.len()..MAGIC.len() + 12].try_into()?;
        let mut data = Zeroizing::new(bytes[MAGIC.len() + 12..].to_vec());
        let size = key
            .open_in_place(aead::Nonce::assume_unique_for_key(nonce), aad, &mut data)
            .map_err(|_| anyhow::anyhow!("凭据解密失败：密钥不匹配或文件已损坏"))?
            .len();
        data.truncate(size);
        Ok(data)
    }
}
