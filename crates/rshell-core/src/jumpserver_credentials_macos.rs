//! JumpServer tokens and passwords in macOS Keychain; blocking workers only.
use anyhow::{Context, Result, ensure};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use uuid::Uuid;
use zeroize::Zeroizing;
const SERVICE: &str = "com.RShell.RShell.JumpServer";

fn read_target(account: String) -> Result<Option<Zeroizing<String>>> {
    match get_generic_password(SERVICE, &account) {
        Ok(bytes) => {
            let bytes = Zeroizing::new(bytes);
            Ok(Some(Zeroizing::new(
                std::str::from_utf8(&bytes)?.to_owned(),
            )))
        }
        Err(error) if error.code() == -25300 => Ok(None),
        Err(error) => Err(error).context("读取 macOS 钥匙串失败"),
    }
}
fn write_target(account: String, value: &str) -> Result<()> {
    ensure!(!value.is_empty() && value.len() <= 2560, "凭据长度无效");
    set_generic_password(SERVICE, &account, value.as_bytes()).context("保存 macOS 钥匙串失败")
}
fn delete_target(account: String) -> Result<()> {
    match delete_generic_password(SERVICE, &account) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(error) => Err(error).context("删除 macOS 钥匙串凭据失败"),
    }
}
pub fn read(id: Uuid) -> Result<Option<Zeroizing<String>>> {
    read_target(id.to_string())
}
pub fn write(id: Uuid, value: &str) -> Result<()> {
    write_target(id.to_string(), value)
}
pub fn delete(id: Uuid) -> Result<()> {
    delete_target(id.to_string())
}
pub fn read_password(id: Uuid) -> Result<Option<Zeroizing<String>>> {
    read_target(format!("{id}/Password"))
}
pub fn write_password(id: Uuid, value: &str) -> Result<()> {
    write_target(format!("{id}/Password"), value)
}
pub fn delete_password(id: Uuid) -> Result<()> {
    delete_target(format!("{id}/Password"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires unlocked macOS Keychain"]
    fn jumpserver_credentials_roundtrip() {
        let id = Uuid::new_v4();
        assert!(read(id).unwrap().is_none());
        write(id, "synthetic-token").unwrap();
        write_password(id, "synthetic-password").unwrap();
        assert_eq!(read(id).unwrap().unwrap().as_str(), "synthetic-token");
        write(id, "replacement").unwrap();
        assert_eq!(read(id).unwrap().unwrap().as_str(), "replacement");
        assert!(write_password(id, "").is_err());
        assert!(write(id, &"x".repeat(2561)).is_err());
        delete(id).unwrap();
        assert!(read(id).unwrap().is_none());
        assert_eq!(
            read_password(id).unwrap().unwrap().as_str(),
            "synthetic-password"
        );
        delete_password(id).unwrap();
        delete_password(id).unwrap();
        assert!(read_password(id).unwrap().is_none());
    }
}
