//! Bounded, in-memory import. Archive members are never extracted or executed.
use crate::config::{Config, Folder, Profile, Protocol};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;
use zeroize::Zeroizing;
use zip::read::HasZipMetadata;

const MAX_ENTRY: u64 = 1024 * 1024;
const MAX_TOTAL: u64 = 32 * 1024 * 1024;

/// Export one SSH/Telnet session using the Xshell XTS archive layout.
pub fn export(path: &Path, profile: &Profile) -> Result<()> {
    ensure!(
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("xts")),
        "请选择 .xts 文件"
    );
    ensure!(
        matches!(profile.protocol, Protocol::Ssh | Protocol::Telnet),
        "串口会话暂不支持导出为 XTS"
    );
    profile.validate()?;
    ensure!(
        !profile
            .name
            .contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*'])
            && !profile.name.chars().any(char::is_control)
            && profile.name != "."
            && profile.name != "..",
        "会话名含 XTS 路径不支持的字符"
    );
    ensure!(
        !profile.username.contains(['\r', '\n']),
        "用户名含不支持的换行符"
    );
    let protocol = if profile.protocol == Protocol::Ssh {
        "SSH"
    } else {
        "TELNET"
    };
    let content = format!(
        "[CONNECTION]\r\nProtocol={protocol}\r\nHost={}\r\nPort={}\r\n[CONNECTION:AUTHENTICATION]\r\nUserName={}\r\n",
        profile.host, profile.port, profile.username
    );
    let mut utf16 = vec![0xff, 0xfe];
    for unit in content.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("无法创建 .xts 文件；已有同名文件时请另选名称")?;
    let result = (|| -> Result<()> {
        let mut writer = zip::ZipWriter::new(file);
        writer.start_file(
            format!("Xshell/Sessions/{}.xsh", profile.name),
            zip::write::SimpleFileOptions::default(),
        )?;
        writer.write_all(&utf16)?;
        writer.finish()?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

#[derive(Default, Debug)]
pub struct Report {
    pub ssh: usize,
    pub telnet: usize,
    pub serial_skipped: usize,
    pub unsupported: usize,
    pub invalid: usize,
    pub missing_username: usize,
    pub folders: usize,
}

impl Report {
    pub fn summary(&self) -> String {
        format!(
            "导入完成：SSH {}，Telnet {}，文件夹 {}。跳过：串口参数暂不兼容 {}，其他协议 {}，无效会话 {}。SSH 用户名待补 {}。密码需重新输入；私钥需重新选择。",
            self.ssh,
            self.telnet,
            self.folders,
            self.serial_skipped,
            self.unsupported,
            self.invalid,
            self.missing_username
        )
    }
}

/// Returns a new configuration so parsing failures never modify the caller's state.
pub fn import(path: &Path, current: &Config) -> Result<(Config, Report)> {
    import_with_names(path, current, false)
}

fn import_with_names(
    path: &Path,
    current: &Config,
    legacy_names: bool,
) -> Result<(Config, Report)> {
    let file = File::open(path).context("无法打开 XTS 文件")?;
    ensure!(
        file.metadata()?.len() <= 64 * 1024 * 1024,
        "XTS 文件超过 64 MiB"
    );
    let mut archive = zip::ZipArchive::new(file).context("XTS 不是支持的 ZIP 导出包")?;
    ensure!(archive.len() <= 4096, "XTS 条目超过 4096 个");
    let mut config = current.clone();
    let mut report = Report::default();
    let mut name = "Xshell 导入".to_owned();
    let mut suffix = 2;
    while config
        .folders
        .iter()
        .any(|f| f.parent_id.is_none() && f.name == name)
    {
        name = format!("Xshell 导入 ({suffix})");
        suffix += 1;
    }
    let root = Uuid::new_v4();
    config.folders.push(Folder {
        id: root,
        name,
        parent_id: None,
    });
    let mut folders = HashMap::new();
    folders.insert(String::new(), root);
    let mut total = 0;
    let mut sessions = 0;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("无法读取 XTS 条目（可能加密或损坏）")?;
        let name = if legacy_names {
            entry.name().to_owned()
        } else {
            decode_name(entry.name_raw(), entry.get_metadata().is_utf8)?
        }
        .replace('\\', "/");
        ensure!(name.len() <= 2048, "XTS 条目路径过长");
        let parts: Vec<_> = name.trim_end_matches('/').split('/').collect();
        if !parts
            .first()
            .is_some_and(|p| p.eq_ignore_ascii_case("Xshell"))
        {
            continue;
        }
        ensure!(
            parts.iter().all(|p| !p.is_empty()
                && *p != "."
                && *p != ".."
                && !p.contains(':')
                && !p.chars().any(char::is_control)),
            "XTS 包含不安全路径"
        );
        let parts = &parts[1..];
        let parts = if parts
            .first()
            .is_some_and(|p| p.eq_ignore_ascii_case("Sessions"))
        {
            &parts[1..]
        } else {
            parts
        };
        if parts.is_empty() {
            continue;
        }
        let is_session =
            !entry.is_dir() && parts.last().unwrap().to_ascii_lowercase().ends_with(".xsh");
        if !entry.is_dir() && !is_session {
            continue;
        }
        let directories = if entry.is_dir() {
            parts
        } else {
            &parts[..parts.len() - 1]
        };
        ensure!(directories.len() < 32, "XTS 文件夹超过 32 层");
        let mut parent = root;
        let mut key = String::new();
        for directory in directories {
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(directory);
            parent = *folders.entry(key.clone()).or_insert_with(|| {
                let id = Uuid::new_v4();
                config.folders.push(Folder {
                    id,
                    name: (*directory).into(),
                    parent_id: Some(parent),
                });
                id
            });
            ensure!(folders.len() <= 4096, "XTS 文件夹超过 4096 个");
        }
        if !is_session {
            continue;
        }
        sessions += 1;
        ensure!(entry.size() <= MAX_ENTRY, "XSH 会话超过 1 MiB");
        let mut bytes = Zeroizing::new(Vec::new());
        (&mut entry)
            .take(MAX_ENTRY + 1)
            .read_to_end(&mut bytes)
            .context("读取 XSH 失败")?;
        total += bytes.len() as u64;
        ensure!(
            bytes.len() as u64 <= MAX_ENTRY && total <= MAX_TOTAL,
            "XTS 解压数据超出容量限制"
        );
        let text = match decode(&bytes) {
            Ok(t) => t,
            Err(_) => {
                report.invalid += 1;
                continue;
            }
        };
        let values = fields(&text);
        let get = |section: &str, key: &str| values.get(&(section, key)).copied().unwrap_or("");
        let protocol = match get("CONNECTION", "Protocol").to_ascii_uppercase().as_str() {
            "SSH" => Protocol::Ssh,
            "TELNET" => Protocol::Telnet,
            "SERIAL" => {
                report.serial_skipped += 1;
                continue;
            }
            _ => {
                report.unsupported += 1;
                continue;
            }
        };
        let host = get("CONNECTION", "Host");
        let port = get("CONNECTION", "Port").parse::<u16>();
        if host.is_empty()
            || host.chars().any(|c| c.is_whitespace() || c.is_control())
            || !matches!(port, Ok(1..))
        {
            report.invalid += 1;
            continue;
        }
        let filename = parts.last().unwrap();
        let username = get("CONNECTION:AUTHENTICATION", "UserName");
        let profile = Profile {
            protocol,
            name: filename[..filename.len() - 4].into(),
            folder_id: Some(parent),
            host: host.into(),
            port: port.unwrap(),
            username: username.into(),
            ..Profile::default()
        };
        if profile.name.trim().is_empty() {
            report.invalid += 1;
            continue;
        }
        if protocol == Protocol::Ssh {
            report.ssh += 1;
            if username.is_empty() {
                report.missing_username += 1;
            }
        } else {
            report.telnet += 1;
        }
        config.profiles.push(profile);
    }
    ensure!(
        sessions > 0 || folders.len() > 1,
        "未找到 Xshell 会话或文件夹"
    );
    report.folders = config.folders.len() - current.folders.len();
    config.validate_folders()?;
    Ok((config, report))
}

// Xshell on Chinese Windows writes ANSI/GBK paths without ZIP's UTF-8 bit.
// The ZIP library's CP437 fallback is unsuitable for those exports. A validated
// Unicode extra field is already exposed by zip as UTF-8 name_raw + is_utf8.
fn decode_name(raw: &[u8], utf8: bool) -> Result<String> {
    if utf8 {
        return Ok(std::str::from_utf8(raw)
            .context("XTS 的 UTF-8 文件名无效")?
            .into());
    }
    if let Ok(name) = std::str::from_utf8(raw) {
        return Ok(name.into());
    }
    let (name, errors) = encoding_rs::GBK.decode_without_bom_handling(raw);
    ensure!(!errors, "XTS 文件名不是有效的 UTF-8 或 GBK，未导入");
    Ok(name.into_owned())
}

#[derive(Default)]
pub struct RepairReport {
    pub folders: usize,
    pub profiles: usize,
}
impl RepairReport {
    pub fn summary(&self) -> String {
        format!(
            "名称修复完成：文件夹 {}，会话 {}。未新增会话，连接配置和凭据保持不变。仅匹配原导入目录及原名称；已改名、移动或修改连接地址的会话可能需要手工检查。",
            self.folders, self.profiles
        )
    }
}

fn relative_import_path(config: &Config, mut id: Option<Uuid>) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    for _ in 0..32 {
        let folder = config.folders.iter().find(|f| Some(f.id) == id)?;
        if folder.parent_id.is_none() {
            let imported = folder.name == "Xshell 导入"
                || folder
                    .name
                    .strip_prefix("Xshell 导入 (")
                    .and_then(|s| s.strip_suffix(')'))
                    .is_some_and(|s| s.parse::<usize>().is_ok());
            if !imported {
                return None;
            }
            parts.reverse();
            return Some(parts);
        }
        parts.push(folder.name.clone());
        id = folder.parent_id;
    }
    None
}

/// Repair only exact legacy name/path + endpoint matches under import roots.
/// Work from the unchanged snapshot so parent renames cannot invalidate child matches.
pub fn repair_names(path: &Path, current: &Config) -> Result<(Config, RepairReport)> {
    let (old, _) = import_with_names(path, &Config::default(), true)?;
    let (correct, _) = import(path, &Config::default())?;
    ensure!(
        old.folders.len() == correct.folders.len() && old.profiles.len() == correct.profiles.len(),
        "文件名映射存在歧义，未修复"
    );
    let mut folder_names = HashMap::new();
    for (before, after) in old.folders.iter().zip(&correct.folders) {
        if before.name != after.name {
            folder_names.insert(
                relative_import_path(&old, Some(before.id)).context("原始目录无效")?,
                after.name.clone(),
            );
        }
    }
    let mut profile_names = HashMap::new();
    for (before, after) in old.profiles.iter().zip(&correct.profiles) {
        if before.name != after.name {
            let mut path =
                relative_import_path(&old, before.folder_id).context("原始会话目录无效")?;
            path.push(before.name.clone());
            // Protocol is intentionally compared too; usernames/credentials may have
            // been filled in since import and must not be replaced.
            let key = (
                path,
                before.host.clone(),
                before.port,
                before.protocol == Protocol::Telnet,
            );
            if let Some(previous) = profile_names.insert(key, after.name.clone()) {
                ensure!(previous == after.name, "会话名映射存在歧义，未修复");
            }
        }
    }
    let mut repaired = current.clone();
    let mut report = RepairReport::default();
    for folder in &mut repaired.folders {
        if let Some(path) = relative_import_path(current, Some(folder.id))
            && let Some(name) = folder_names.get(&path)
        {
            folder.name.clone_from(name);
            report.folders += 1;
        }
    }
    for profile in &mut repaired.profiles {
        if !matches!(profile.protocol, Protocol::Ssh | Protocol::Telnet) {
            continue;
        }
        if let Some(mut path) = relative_import_path(current, profile.folder_id) {
            path.push(profile.name.clone());
            let key = (
                path,
                profile.host.clone(),
                profile.port,
                profile.protocol == Protocol::Telnet,
            );
            if let Some(name) = profile_names.get(&key) {
                profile.name.clone_from(name);
                report.profiles += 1;
            }
        }
    }
    repaired.validate_folders()?;
    Ok((repaired, report))
}

fn decode(bytes: &[u8]) -> Result<Zeroizing<String>> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        ensure!(bytes.len().is_multiple_of(2), "无效 UTF-16");
        let little = bytes[0] == 0xff;
        let units = Zeroizing::new(
            bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| {
                    if little {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    }
                })
                .collect::<Vec<_>>(),
        );
        return Ok(Zeroizing::new(String::from_utf16(&units)?));
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(Zeroizing::new(text.trim_start_matches('\u{feff}').into())),
        Err(_) => bail!("仅支持 UTF-8 / UTF-16 会话"),
    }
}

// Borrow only allowlisted configuration fields. Secrets/scripts are never copied into profiles.
fn fields(text: &str) -> HashMap<(&str, &str), &str> {
    let mut result = HashMap::new();
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if let Some(s) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = s;
        } else if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if (section == "CONNECTION" && matches!(key, "Protocol" | "Host" | "Port"))
                || (section == "CONNECTION:AUTHENTICATION" && key == "UserName")
            {
                result.insert((section, key), value.trim());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(path: &Path, entries: &[(&str, Vec<u8>)]) {
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            let options = zip::write::SimpleFileOptions::default();
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap();
    }
    fn session(protocol: &str) -> Vec<u8> {
        format!("[CONNECTION]\r\nProtocol={protocol}\r\nHost=127.0.0.1\r\nPort=2222\r\n[CONNECTION:AUTHENTICATION]\r\nUserName=demo\r\nPassword=TEST_SECRET_NOT_IMPORTED\r\nPassphrase=TEST_PHRASE_NOT_IMPORTED\r\nScriptPath=never-run.cmd\r\n").into_bytes()
    }

    // Construct a real legacy ZIP: GBK raw filenames, UTF-8 bit unset, no Unicode
    // extra field. Writing Unicode names with ZipWriter alone cannot reproduce this bug.
    fn gbk_archive(path: &Path) {
        let entries = [
            "Xshell/线上服务器/",
            "Xshell/线上服务器/中文会话.xsh",
            "Xshell/空文件夹/",
        ];
        let mut names = HashMap::new();
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        for (index, name) in entries.iter().enumerate() {
            let (raw, _, errors) = encoding_rs::GBK.encode(name);
            assert!(!errors);
            let mut ascii = format!("{index}{}", "a".repeat(raw.len() - 1));
            if name.ends_with('/') {
                ascii.pop();
                ascii.push('/');
            }
            names.insert(ascii.clone().into_bytes(), raw.into_owned());
            let options = zip::write::SimpleFileOptions::default();
            if name.ends_with('/') {
                writer.add_directory(ascii, options).unwrap();
            } else {
                writer.start_file(ascii, options).unwrap();
                writer.write_all(&session("SSH")).unwrap();
            }
        }
        writer.finish().unwrap();
        let mut bytes = std::fs::read(path).unwrap();
        for i in 0..bytes.len() - 46 {
            let (offset, size_offset) = match &bytes[i..i + 4] {
                b"PK\x03\x04" => (30, 26),
                b"PK\x01\x02" => (46, 28),
                _ => continue,
            };
            let size =
                u16::from_le_bytes([bytes[i + size_offset], bytes[i + size_offset + 1]]) as usize;
            let filename = &mut bytes[i + offset..i + offset + size];
            let replacement = names.get(filename).unwrap();
            filename.copy_from_slice(replacement);
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn gbk_filenames_and_legacy_name_repair_preserve_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gbk.xts");
        gbk_archive(&path);
        let (correct, _) = import(&path, &Config::default()).unwrap();
        assert_eq!(correct.profiles[0].name, "中文会话");
        assert!(correct.folders.iter().any(|f| f.name == "空文件夹"));
        assert_eq!(
            correct.folder_path(correct.profiles[0].folder_id),
            "Xshell 导入 / 线上服务器"
        );
        let (mut legacy, _) = import_with_names(&path, &Config::default(), true).unwrap();
        assert_ne!(legacy.profiles[0].name, "中文会话");
        legacy.profiles[0].username = "user-filled-after-import".into();
        legacy.profiles[0].remember_secret = true;
        legacy.recent.push(legacy.profiles[0].id);
        let (mut repaired, report) = repair_names(&path, &legacy).unwrap();
        assert_eq!((report.folders, report.profiles), (2, 1));
        assert_eq!(repaired.profiles[0].name, "中文会话");
        let (_, again) = repair_names(&path, &repaired).unwrap();
        assert_eq!((again.folders, again.profiles), (0, 0));
        for (p, original) in repaired.profiles.iter_mut().zip(&legacy.profiles) {
            p.name.clone_from(&original.name);
        }
        for (f, original) in repaired.folders.iter_mut().zip(&legacy.folders) {
            f.name.clone_from(&original.name);
        }
        assert_eq!(
            serde_json::to_value(&repaired).unwrap(),
            serde_json::to_value(&legacy).unwrap()
        );
        legacy.profiles[0].name = "用户改名".into();
        let (repaired, report) = repair_names(&path, &legacy).unwrap();
        assert_eq!(report.profiles, 0);
        assert_eq!(repaired.profiles[0].name, "用户改名");
        legacy.folders.push(Folder {
            id: Uuid::new_v4(),
            parent_id: Some(legacy.folders[0].id),
            name: "线上服务器".into(),
        });
        assert!(repair_names(&path, &legacy).is_err());
    }

    #[test]
    fn filename_decoding_honors_utf8_and_rejects_invalid_gbk() {
        assert_eq!(
            decode_name("中文.xsh".as_bytes(), true).unwrap(),
            "中文.xsh"
        );
        assert_eq!(
            decode_name("中文.xsh".as_bytes(), false).unwrap(),
            "中文.xsh"
        );
        assert!(decode_name(&[0x81], false).is_err());
        assert!(decode_name(&[0xd6, 0xd0], true).is_err());
    }

    #[test]
    fn repair_worker_rolls_back_on_save_failure_and_never_touches_credentials() {
        use crate::store::{Action, Credentials, Outcome, Store};
        struct NoCredentials;
        impl Credentials for NoCredentials {
            fn read(&mut self, _: Uuid) -> Result<Option<Zeroizing<String>>> {
                panic!("repair must not read credentials")
            }
            fn write(&mut self, _: Uuid, _: &str) -> Result<()> {
                panic!("repair must not write credentials")
            }
            fn delete(&mut self, _: Uuid) -> Result<()> {
                panic!("repair must not delete credentials")
            }
        }
        fn receive(store: &Store) -> crate::store::Completion {
            let started = std::time::Instant::now();
            loop {
                if let Ok(result) = store.try_recv() {
                    return result;
                }
                assert!(started.elapsed().as_secs() < 5);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gbk.xts");
        gbk_archive(&path);
        let (legacy, _) = import_with_names(&path, &Config::default(), true).unwrap();
        let config_path = temp.path().join("config.json");
        std::fs::create_dir(&config_path).unwrap();
        let store =
            Store::start(config_path.clone(), legacy.clone(), NoCredentials, || {}).unwrap();
        store.submit(Action::RepairXtsNames(path.clone())).unwrap();
        assert!(receive(&store).result.is_err());
        std::fs::remove_dir(&config_path).unwrap();
        store.submit(Action::RepairXtsNames(path)).unwrap();
        let Outcome::RepairedNames(config, report) = receive(&store).result.unwrap() else {
            panic!("wrong outcome")
        };
        assert_eq!((report.folders, report.profiles), (2, 1));
        assert_eq!(config.profiles[0].id, legacy.profiles[0].id);
        assert_eq!(
            Config::load(&config_path).unwrap().profiles[0].name,
            "中文会话"
        );
    }

    #[test]
    fn imports_hierarchy_utf16_telnet_and_excludes_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.xts");
        let text = String::from_utf8(session("SSH")).unwrap();
        let utf16 = [
            vec![255, 254],
            text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ]
        .concat();
        archive(
            &path,
            &[
                ("Xshell/父/子/中文.xsh", utf16),
                ("Xshell/空目录/", vec![]),
                ("Xshell/telnet.xsh", session("TELNET")),
                ("Xshell/serial.xsh", session("SERIAL")),
                ("Xshell/rlogin.xsh", session("RLOGIN")),
                ("Xshell/script.py", b"never execute".to_vec()),
            ],
        );
        let (config, report) = import(&path, &Config::default()).unwrap();
        assert_eq!(
            (
                report.ssh,
                report.telnet,
                report.serial_skipped,
                report.unsupported,
                report.folders
            ),
            (1, 1, 1, 1, 4)
        );
        assert!(
            config
                .folder_path(config.profiles[0].folder_id)
                .ends_with("父 / 子")
        );
        assert!(config.known_hosts.is_empty());
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            !json.contains("TEST_SECRET")
                && !json.contains("TEST_PHRASE")
                && !json.contains("never-run")
        );
        assert!(
            config
                .profiles
                .iter()
                .all(|p| !p.remember_secret && p.key_path.is_empty())
        );
        let (again, _) = import(&path, &config).unwrap();
        assert_eq!(again.profiles.len(), 4);
        assert_eq!(again.profiles[0].id, config.profiles[0].id);
        assert!(again.folders.iter().any(|f| f.name == "Xshell 导入 (2)"));
    }

    #[test]
    fn rejects_unsafe_paths_oversize_and_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.xts");
        archive(&path, &[("Xshell/../bad.xsh", session("SSH"))]);
        assert!(import(&path, &Config::default()).is_err());
        archive(
            &path,
            &[("Xshell/huge.xsh", vec![0; MAX_ENTRY as usize + 1])],
        );
        assert!(import(&path, &Config::default()).is_err());
        std::fs::write(&path, b"invalid zip").unwrap();
        assert!(import(&path, &Config::default()).is_err());
    }

    #[test]
    fn missing_user_is_preserved_and_invalid_host_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.xts");
        archive(
            &path,
            &[
                (
                    "Xshell/empty-user.xsh",
                    b"[CONNECTION]\nProtocol=SSH\nHost=localhost\nPort=22".to_vec(),
                ),
                (
                    "Xshell/empty-host.xsh",
                    b"[CONNECTION]\nProtocol=SSH\nHost=\nPort=22".to_vec(),
                ),
            ],
        );
        let (config, report) = import(&path, &Config::default()).unwrap();
        assert_eq!(
            (report.ssh, report.missing_username, report.invalid),
            (1, 1, 1)
        );
        assert!(config.profiles[0].username.is_empty());
    }

    /// Explicit opt-in, local file only. No connections and no secrets printed.
    #[test]
    #[ignore = "Set RSHELL_TEST_XTS to an actual export file"]
    fn actual_export_roundtrip() {
        let path = std::env::var_os("RSHELL_TEST_XTS").expect("RSHELL_TEST_XTS");
        let (config, report) = import(Path::new(&path), &Config::default()).unwrap();
        let (legacy, _) = import_with_names(Path::new(&path), &Config::default(), true).unwrap();
        let (repaired, repair_report) = repair_names(Path::new(&path), &legacy).unwrap();
        for (a, b) in repaired.profiles.iter().zip(&config.profiles) {
            assert_eq!(a.name, b.name);
        }
        for (a, b) in repaired.folders.iter().zip(&config.folders) {
            assert_eq!(a.name, b.name);
        }
        for expected in [
            "C端",
            "交换机",
            "内网",
            "开发环境",
            "测试环境",
            "线上服务器",
            "领课",
            "腾讯",
        ] {
            assert!(
                config.folders.iter().any(|f| f.name == expected),
                "expected Chinese folder missing"
            );
        }
        assert!(
            config
                .profiles
                .iter()
                .any(|p| p.name == "lili阿里云 - 副本")
        );
        println!("{}", repair_report.summary());
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/xts-validation");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("config.json");
        config.save(&target).unwrap();
        let loaded = Config::load(&target).unwrap();
        assert_eq!(loaded.profiles.len(), report.ssh + report.telnet);
        assert_eq!(loaded.folders.len(), report.folders);
        assert!(
            loaded
                .profiles
                .iter()
                .all(|p| !p.remember_secret && p.key_path.is_empty())
        );
        assert!(loaded.known_hosts.is_empty());
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&config).unwrap()
        );
        println!("{}", report.summary());
    }
}
#[test]
fn exported_ssh_and_telnet_import_back_without_secrets() {
    let temp = tempfile::tempdir().unwrap();
    for protocol in [Protocol::Ssh, Protocol::Telnet] {
        let profile = Profile {
            protocol,
            name: "中文会话".into(),
            host: "example.test".into(),
            username: "demo".into(),
            port: if protocol == Protocol::Ssh { 22 } else { 23 },
            ..Profile::default()
        };
        let path = temp.path().join(format!("{:?}.xts", protocol));
        export(&path, &profile).unwrap();
        let (config, report) = import(&path, &Config::default()).unwrap();
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.profiles[0].name, profile.name);
        assert_eq!(config.profiles[0].host, profile.host);
        assert_eq!(config.profiles[0].protocol, protocol);
        assert_eq!(report.ssh + report.telnet, 1);
        assert!(export(&path, &profile).is_err());
    }
}
