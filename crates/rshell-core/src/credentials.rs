//! RShell-owned encrypted files, protected by current-user Windows DPAPI.
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    ptr,
};
use uuid::Uuid;
use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};
use zeroize::Zeroizing;

pub struct EncryptedCredentials(pub PathBuf);
const MAX_FILE_SIZE: u64 = 65536;
const MAGIC: &[u8] = b"RSHELL-SECRET-1\0";

pub(crate) fn crypt(id: Uuid, bytes: &[u8], protect: bool) -> Result<Zeroizing<Vec<u8>>> {
    let mut input_bytes = Zeroizing::new(bytes.to_vec());
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_bytes.len().try_into()?,
        pbData: input_bytes.as_mut_ptr(),
    };
    let mut entropy_bytes = *id.as_bytes();
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: 16,
        pbData: entropy_bytes.as_mut_ptr(),
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };
    // DPAPI allocates output; copy it, wipe it, then release with LocalFree.
    let success = unsafe {
        if protect {
            CryptProtectData(
                &input,
                ptr::null(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error()).context("RShell 凭据加密或解密失败");
    }
    let result = unsafe {
        let result = if output.cbData == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        if !output.pbData.is_null() {
            ptr::write_bytes(output.pbData, 0, output.cbData as usize);
            LocalFree(output.pbData.cast());
        }
        Zeroizing::new(result)
    };
    Ok(result)
}

impl EncryptedCredentials {
    fn path(&self, id: Uuid) -> PathBuf {
        self.0.join(format!("{id}.bin"))
    }
}
impl crate::store::Credentials for EncryptedCredentials {
    fn read(&mut self, id: Uuid) -> Result<Option<Zeroizing<String>>> {
        let file = match fs::File::open(self.path(id)) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("读取 RShell 加密凭据失败"),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_SIZE + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_SIZE || !bytes.starts_with(MAGIC) {
            bail!("RShell 加密凭据文件格式无效");
        }
        let plaintext = crypt(id, &bytes[MAGIC.len()..], false)?;
        let text = std::str::from_utf8(&plaintext).context("RShell 凭据内容格式无效")?;
        Ok(Some(Zeroizing::new(text.to_owned())))
    }
    fn write(&mut self, id: Uuid, value: &str) -> Result<()> {
        if value.len() > 16384 {
            bail!("密码或口令过长");
        }
        let encrypted = crypt(id, value.as_bytes(), true)?;
        fs::create_dir_all(&self.0)?;
        let path = self.path(id);
        let temporary = self.0.join(format!("{}.tmp", Uuid::new_v4()));
        let result = write_file(&temporary, &path, &encrypted);
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    fn delete(&mut self, id: Uuid) -> Result<()> {
        match fs::remove_file(self.path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("删除 RShell 加密凭据失败"),
        }
    }
}
fn write_file(temporary: &Path, path: &Path, encrypted: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)?;
    file.write_all(MAGIC)?;
    file.write_all(encrypted)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path).context("替换 RShell 加密凭据失败")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Credentials;
    #[test]
    fn encrypted_credentials_roundtrip() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/credential-tests");
        fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let mut store = EncryptedCredentials(temp.path().join("credentials"));
        let id = Uuid::new_v4();
        assert!(store.read(id).unwrap().is_none());
        store.write(id, "仅测试-secret").unwrap();
        assert_eq!(store.read(id).unwrap().unwrap().as_str(), "仅测试-secret");
        let ciphertext = fs::read(store.path(id)).unwrap();
        assert!(!ciphertext.windows(b"secret".len()).any(|w| w == b"secret"));
        let other = Uuid::new_v4();
        fs::write(store.path(other), &ciphertext).unwrap();
        assert!(store.read(other).is_err());
        store.write(id, "replacement").unwrap();
        assert_eq!(store.read(id).unwrap().unwrap().as_str(), "replacement");
        store.write(id, "").unwrap();
        assert_eq!(store.read(id).unwrap().unwrap().as_str(), "");
        fs::write(store.path(id), b"damaged").unwrap();
        assert!(store.read(id).is_err());
        store.delete(id).unwrap();
        assert!(store.read(id).unwrap().is_none());
        store.delete(id).unwrap();
    }
}
