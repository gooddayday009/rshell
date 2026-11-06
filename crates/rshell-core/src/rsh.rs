//! RShell's portable session file. Only the listed public settings cross the boundary.
use crate::config::{AuthMethod, KeepAlive, Profile, Protocol, SerialSettings};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};
use uuid::Uuid;

const MAX_FILE: u64 = 32 * 1024 * 1024;

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tunnels: Vec<crate::tunnel::Rule>,
    #[serde(default, skip_serializing_if = "is_false")]
    tunnel_only: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

fn check_extension(path: &Path) -> Result<()> {
    ensure!(
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rsh")),
        "请选择 .rsh 文件"
    );
    Ok(())
}

fn session_file(profile: &Profile) -> Result<SessionFile> {
    profile.validate_portable()?;
    // A private-key session must select its key again after import.
    let file = SessionFile {
        format: "RShell session".into(),
        version: if profile.tunnels.is_empty() && !profile.tunnel_only {
            1
        } else {
            3
        },
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
        tunnels: profile.tunnels.clone(),
        tunnel_only: profile.tunnel_only,
    };
    Ok(file)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionCollection {
    format: String,
    version: u32,
    sessions: Vec<SessionFile>,
}

pub fn export(path: &Path, profile: &Profile) -> Result<()> {
    write_file(path, &session_file(profile)?)
}

pub fn export_all(path: &Path, profiles: &[Profile]) -> Result<()> {
    ensure!(!profiles.is_empty(), "暂无可导出的会话");
    ensure!(profiles.len() <= 4096, "会话超过 4096 个");
    let file = SessionCollection {
        format: "RShell sessions".into(),
        version: 2,
        sessions: profiles.iter().map(session_file).collect::<Result<_>>()?,
    };
    write_file(path, &file)
}

fn write_file(path: &Path, file: &impl Serialize) -> Result<()> {
    check_extension(path)?;
    let data = serde_json::to_vec_pretty(file)?;
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

pub fn import_all(path: &Path) -> Result<Vec<Profile>> {
    check_extension(path)?;
    ensure!(fs::metadata(path)?.len() <= MAX_FILE, ".rsh 文件过大");
    let data = fs::read(path).context("无法读取 .rsh 文件")?;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Portable {
        Single(SessionFile),
        Collection(SessionCollection),
    }
    let files = match serde_json::from_slice(&data).context("无效的 .rsh 会话文件")? {
        Portable::Single(file) => vec![file],
        Portable::Collection(file) => {
            ensure!(
                file.format == "RShell sessions" && file.version == 2,
                "不支持的 .rsh 会话格式或版本"
            );
            ensure!(
                !file.sessions.is_empty() && file.sessions.len() <= 4096,
                "会话数量须为 1 至 4096 个"
            );
            file.sessions
        }
    };
    files.into_iter().map(into_profile).collect()
}

pub fn import(path: &Path) -> Result<Profile> {
    let mut profiles = import_all(path)?;
    ensure!(profiles.len() == 1, "此文件包含多个会话");
    Ok(profiles.remove(0))
}

fn into_profile(file: SessionFile) -> Result<Profile> {
    if file.format != "RShell session" || !matches!(file.version, 1 | 3) {
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
        tunnels: file.tunnels,
        tunnel_only: file.tunnel_only,
        key_path: String::new(),
        remember_secret: false,
        folder_id: None,
        group: String::new(),
    };
    profile.validate_portable()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_export_retains_rules_and_keeps_ordinary_files_compatible() {
        let profile = Profile {
            host: "server.test".into(),
            ..Default::default()
        };
        let legacy = serde_json::to_value(session_file(&profile).unwrap()).unwrap();
        assert_eq!(legacy["version"], 1);
        assert!(legacy.get("tunnels").is_none());
        assert!(legacy.get("tunnel_only").is_none());
        let tunnel = Profile {
            tunnels: vec![crate::tunnel::Rule::default()],
            tunnel_only: true,
            ..profile
        };
        let file = session_file(&tunnel).unwrap();
        assert_eq!(file.version, 3);
        let restored = into_profile(file).unwrap();
        assert_eq!(restored.tunnels, tunnel.tunnels);
        assert!(restored.tunnel_only);
    }

    #[test]
    fn all_sessions_roundtrip_including_serial_without_key_material() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("all.rsh");
        let ssh = Profile {
            name: "同名会话".into(),
            host: "example.test".into(),
            auth: AuthMethod::PrivateKey,
            key_path: "C:/secret/private.key".into(),
            remember_secret: true,
            ..Profile::default()
        };
        let serial = Profile {
            protocol: Protocol::Serial,
            name: ssh.name.clone(),
            ..Profile::default()
        };
        let telnet = Profile {
            protocol: Protocol::Telnet,
            remember_secret: false,
            ..ssh.clone()
        };
        let profiles = vec![ssh, serial, telnet];
        export_all(&path, &profiles).unwrap();
        let imported = import_all(&path).unwrap();
        assert_eq!(imported.len(), profiles.len());
        for (before, after) in profiles.iter().zip(&imported) {
            assert_eq!(before.name, after.name);
            assert_eq!(before.protocol, after.protocol);
            assert_ne!(before.id, after.id);
            assert!(after.key_path.is_empty());
            assert!(!after.remember_secret);
        }
        assert!(!fs::read_to_string(&path).unwrap().contains("C:/secret"));
        assert!(export_all(&path, &profiles).is_err());
        assert!(export_all(&temp.path().join("empty.rsh"), &[]).is_err());
    }

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
