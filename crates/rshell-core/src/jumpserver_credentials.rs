//! Windows Credential Manager; call only from a blocking worker.
use anyhow::{Context, Result, ensure};
use uuid::Uuid;
use windows_sys::Win32::{Foundation::ERROR_NOT_FOUND, Security::Credentials::*};
use zeroize::Zeroizing;

fn target(id: Uuid) -> Vec<u16> {
    format!("RShell/JumpServer/{id}")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

pub fn read(id: Uuid) -> Result<Option<Zeroizing<String>>> {
    let name = target(id);
    let mut credential = std::ptr::null_mut();
    if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(None);
        }
        return Err(error).context("读取 JumpServer Windows 凭据失败");
    }
    unsafe {
        let c = &*credential;
        let bytes = if c.CredentialBlobSize > 0 && !c.CredentialBlob.is_null() {
            std::slice::from_raw_parts_mut(c.CredentialBlob, c.CredentialBlobSize as usize)
        } else {
            &mut []
        };
        let value = std::str::from_utf8(bytes)
            .map(|v| Some(Zeroizing::new(v.to_owned())))
            .context("JumpServer 凭据格式无效");
        bytes.fill(0);
        CredFree(credential.cast());
        value
    }
}

pub fn write(id: Uuid, token: &str) -> Result<()> {
    ensure!(
        !token.is_empty() && token.len() <= 2560,
        "登录令牌超过 Windows 凭据容量限制"
    );
    let mut name = target(id);
    let mut bytes = Zeroizing::new(token.as_bytes().to_vec());
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: name.as_mut_ptr(),
        CredentialBlobSize: bytes.len() as u32,
        CredentialBlob: bytes.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    ensure!(
        unsafe { CredWriteW(&credential, 0) } != 0,
        "保存 JumpServer Windows 凭据失败"
    );
    Ok(())
}

pub fn delete(id: Uuid) -> Result<()> {
    let name = target(id);
    if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
            return Err(error).context("删除 JumpServer Windows 凭据失败");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "explicit Windows Credential Manager validation"]
    fn jumpserver_credentials_roundtrip() {
        let id = Uuid::new_v4();
        assert!(read(id).unwrap().is_none());
        write(id, "rshell-test-token").unwrap();
        let result = read(id);
        delete(id).unwrap();
        assert_eq!(result.unwrap().unwrap().as_str(), "rshell-test-token");
        assert!(read(id).unwrap().is_none());
    }
}
