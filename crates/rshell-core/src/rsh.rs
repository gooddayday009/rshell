//! RShell's portable, single-session file. Only the listed public settings cross the boundary.
use crate::config::{AuthMethod, KeepAlive, Profile, Protocol, SerialSettings};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};
use uuid::Uuid;

const MAX_FILE: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionFile {
    format: String,
    version: u32,
    protocol: Protocol,
    serial: SerialSettings,
    name: String,
    host: String,
    port: u16,
    username: String,
    auth: AuthMethod,
    keep_alive: KeepAlive,
}

fn check_extension(path: &Path) -> Result<()> {
    ensure!(
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rsh")),
        "请选择 .rsh 文件"
    );
    Ok(())
}

pub fn export(path: &Path, profile: &Profile) -> Result<()> {
    check_extension(path)?;
    profile.validate()?;
    // A private-key session must select its key again after import.
    let file = SessionFile {
        format: "RShell session".into(),
        version: 1,
        protocol: profile.protocol,
        serial: profile.serial.clone(),
        name: profile.name.clone(),
        host: profile.host.clone(),
        port: profile.port,
        username: profile.username.clone(),
        auth: if profile.auth == AuthMethod::PrivateKey {
            AuthMethod::Password
        } else {
            profile.auth.clone()
        },
        keep_alive: profile.keep_alive.clone(),
    };
    let data = serde_json::to_vec_pretty(&file)?;
    ensure!(data.len() as u64 <= MAX_FILE, "会话文件过大");
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("无法创建 .rsh 文件；已有同名文件时请另选名称")?;
    let result = output.write_all(&data).and_then(|_| output.sync_all());
    if let Err(error) = result {
        drop(output);
        let _ = fs::remove_file(path);
        return Err(error).context("写入 .rsh 文件失败");
    }
    Ok(())
}

pub fn import(path: &Path) -> Result<Profile> {
    check_extension(path)?;
    ensure!(fs::metadata(path)?.len() <= MAX_FILE, ".rsh 文件过大");
    let data = fs::read(path).context("无法读取 .rsh 文件")?;
    let file: SessionFile = serde_json::from_slice(&data).context("无效的 .rsh 会话文件")?;
    if file.format != "RShell session" || file.version != 1 {
        bail!("不支持的 .rsh 会话格式或版本");
    }
    let profile = Profile {
        id: Uuid::new_v4(),
        protocol: file.protocol,
        serial: file.serial,
        name: file.name,
        host: file.host,
        port: file.port,
        username: file.username,
        auth: file.auth,
        keep_alive: file.keep_alive,
        key_path: String::new(),
        remember_secret: false,
        folder_id: None,
        group: String::new(),
    };
    profile.validate()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_excludes_local_identity_and_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.rsh");
        let profile = Profile {
            name: "生产 SSH".into(),
            host: "example.test".into(),
            key_path: "C:/secret/private.key".into(),
            auth: AuthMethod::PrivateKey,
            remember_secret: true,
            ..Profile::default()
        };
        export(&path, &profile).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        for excluded in [
            "private.key",
            "key_path",
            "remember_secret",
            "folder_id",
            "id",
        ] {
            assert!(!text.contains(&format!("\"{excluded}\"")));
        }
        assert!(!text.contains("C:/secret"));
        let imported = import(&path).unwrap();
        assert_eq!(imported.name, profile.name);
        assert_eq!(imported.host, profile.host);
        assert_ne!(imported.id, profile.id);
        assert_eq!(imported.auth, AuthMethod::Password);
        assert!(!imported.remember_secret);
        assert!(export(&path, &profile).is_err());
    }

    #[test]
    fn rejects_unsupported_or_injected_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.rsh");
        let profile = Profile {
            host: "example.test".into(),
            ..Profile::default()
        };
        export(&path, &profile).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replace("\"version\": 1", "\"version\": 2")).unwrap();
        assert!(import(&path).is_err());
        fs::write(
            &path,
            text.replace("\"version\": 1", "\"secret\": \"x\", \"version\": 1"),
        )
        .unwrap();
        assert!(import(&path).is_err());
    }
}
