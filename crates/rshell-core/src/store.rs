//! A single blocking worker owns configuration and credential transactions.
//! GUI callers only submit bounded messages and poll completed results.
use crate::config::{Config, Folder, KnownHost, Profile, Trust};
use anyhow::{Context, Result, bail};
use std::{path::PathBuf, sync::mpsc, thread};
use uuid::Uuid;
use zeroize::Zeroizing;

pub enum Action {
    ScrollbackLines(usize),
    ChromeVisibility(crate::config::ChromeVisibility),
    SaveJumpServer(crate::jumpserver::Saved),
    ReadJumpServerToken(Uuid),
    JumpServerPassword {
        id: Uuid,
        password: Option<Zeroizing<String>>,
    },
    JumpServerToken {
        id: Uuid,
        token: Option<Zeroizing<String>>,
    },
    ImportRsh(PathBuf),
    ExportRsh {
        path: PathBuf,
    },
    ExportXts {
        path: PathBuf,
    },
    ImportXts(PathBuf),
    RepairXtsNames(PathBuf),
    SaveAi {
        profile: crate::ai::AiProfile,
        key: Zeroizing<String>,
    },
    DeleteAi(Uuid),
    DefaultAi(Uuid),
    AiBlockedCommands(Vec<String>),
    ClaudeAgent(crate::claude_agent::Settings),
    AiTypography {
        size: f32,
        monospace: bool,
    },
    SaveProfile {
        profile: Profile,
        secret: Zeroizing<String>,
    },
    SaveProfileMetadata(Profile),
    KeepAlive {
        id: Uuid,
        settings: crate::config::KeepAlive,
    },
    DeleteProfile(Uuid),
    SaveFolder(Folder),
    DeleteFolder(Uuid),
    MoveProfile {
        id: Uuid,
        folder_id: Option<Uuid>,
    },
    ReadSecret(Uuid),
    ReadAiKey(Uuid),
    ReadAiHistory,
    SaveAiHistory(crate::ai::ChatHistory),
    SidebarZoom(f32),
    UiAppearance {
        font: Option<crate::config::UiFont>,
        font_size: Option<f32>,
        zoom: Option<f32>,
        palette: Option<crate::config::UiPalette>,
        custom_colors: Option<crate::config::CustomUiColors>,
    },
    Shortcuts(crate::config::Shortcuts),
    SessionOption(SessionOption, bool),
    Preferences {
        font_size: Option<f32>,
        terminal_font: Option<crate::config::TerminalFont>,
        terminal_theme: Option<crate::config::TerminalTheme>,
        light_theme: Option<bool>,
        copy_on_select: Option<bool>,
        trim_paste: Option<bool>,
    },
    Recent(Vec<Uuid>),
    SetShortcut {
        id: Uuid,
        enabled: bool,
    },
    TrustHost {
        host: KnownHost,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    ReplaceHost {
        expected: KnownHost,
        replacement: KnownHost,
    },
    RemoveHost(KnownHost),
}

pub enum Outcome {
    ImportedRsh(Config, String),
    ExportedRsh(PathBuf),
    ExportedXts(PathBuf),
    RepairedNames(Config, crate::xts::RepairReport),
    Imported(Config, crate::xts::Report),
    Committed(Config),
    Secret(Option<Zeroizing<String>>),
    AiHistory(crate::ai::ChatHistory),
    AiHistorySaved,
    JumpServerTokenSaved,
}

#[derive(Clone, Copy)]
pub enum SessionOption {
    ConfirmExit,
    ConfirmDisconnect,
    ClearScrollbackOnClose,
    AutoSaveSession,
    RememberSecretByDefault,
}

pub struct Completion {
    pub id: Uuid,
    pub result: Result<Outcome, String>,
}

struct Request {
    id: Uuid,
    action: Action,
}

pub struct Store {
    sender: Option<mpsc::SyncSender<Request>>,
    receiver: Option<mpsc::Receiver<Completion>>,
    worker: Option<thread::JoinHandle<()>>,
}

pub trait Credentials: Send + 'static {
    fn read(&mut self, id: Uuid) -> Result<Option<Zeroizing<String>>>;
    fn write(&mut self, id: Uuid, value: &str) -> Result<()>;
    fn delete(&mut self, id: Uuid) -> Result<()>;
}

impl Store {
    pub fn start(
        path: PathBuf,
        config: Config,
        credentials: impl Credentials,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let (sender, requests) = mpsc::sync_channel::<Request>(32);
        let (results, receiver) = mpsc::sync_channel(64);
        let worker = thread::Builder::new()
            .name("rshell-config".into())
            .spawn(move || {
                let mut state = State {
                    path,
                    config,
                    credentials,
                };
                while let Ok(request) = requests.recv() {
                    let result = state
                        .apply(request.action)
                        .map_err(|error| format!("{error:#}"));
                    if results
                        .send(Completion {
                            id: request.id,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    notify();
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            receiver: Some(receiver),
            worker: Some(worker),
        })
    }

    pub fn submit(&self, action: Action) -> Result<Uuid> {
        let id = Uuid::new_v4();
        self.sender
            .as_ref()
            .context("配置服务已关闭")?
            .try_send(Request { id, action })
            .map_err(|_| anyhow::anyhow!("配置服务繁忙或已关闭，请稍后重试"))?;
        Ok(id)
    }

    pub fn try_recv(&self) -> Result<Completion, mpsc::TryRecvError> {
        self.receiver.as_ref().unwrap().try_recv()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.sender.take();
        self.receiver.take(); // Unblock a worker whose completion queue is full.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct State<C> {
    path: PathBuf,
    config: Config,
    credentials: C,
}
impl<C: Credentials> State<C> {
    fn apply(&mut self, action: Action) -> Result<Outcome> {
        let mut candidate = self.config.clone();
        match action {
            Action::SaveJumpServer(saved) => {
                saved.settings.validate()?;
                candidate.jumpserver = Some(saved);
            }
            Action::JumpServerPassword { id, password } => {
                if let Some(password) = password {
                    crate::jumpserver::credentials::write_password(id, &password)?;
                } else {
                    crate::jumpserver::credentials::delete_password(id)?;
                }
                return Ok(Outcome::JumpServerTokenSaved);
            }
            Action::ReadJumpServerToken(id) => {
                return Ok(Outcome::Secret(crate::jumpserver::credentials::read(id)?));
            }
            Action::JumpServerToken { id, token } => {
                if let Some(token) = token {
                    crate::jumpserver::credentials::write(id, &token)?;
                } else {
                    crate::jumpserver::credentials::delete(id)?;
                }
                return Ok(Outcome::JumpServerTokenSaved);
            }
            Action::ImportRsh(path) => {
                let profiles = crate::rsh::import_all(&path)?;
                let name = format!("{} 个会话", profiles.len());
                candidate.profiles.extend(profiles);
                candidate.save(&self.path)?;
                self.config = candidate;
                return Ok(Outcome::ImportedRsh(self.config.clone(), name));
            }
            Action::ExportRsh { path } => {
                crate::rsh::export_all(&path, &self.config.profiles)?;
                return Ok(Outcome::ExportedRsh(path));
            }
            Action::ExportXts { path } => {
                crate::xts::export_all(&path, &self.config.profiles)?;
                return Ok(Outcome::ExportedXts(path));
            }
            Action::ImportXts(path) => {
                let (imported, report) = crate::xts::import(&path, &self.config)?;
                imported.save(&self.path)?;
                self.config = imported;
                return Ok(Outcome::Imported(self.config.clone(), report));
            }
            Action::RepairXtsNames(path) => {
                let (repaired, report) = crate::xts::repair_names(&path, &self.config)?;
                if report.folders + report.profiles > 0 {
                    repaired.save(&self.path)?;
                    self.config = repaired;
                }
                return Ok(Outcome::RepairedNames(self.config.clone(), report));
            }
            Action::SaveAi { profile, key } => {
                profile.validate()?;
                let id = profile.id;
                if candidate.profiles.iter().any(|p| p.id == id) {
                    bail!("AI 配置 ID 与 SSH 会话冲突");
                }
                let previous = candidate.ai.profiles.iter().find(|p| p.id == id);
                let retained = if profile.remember_key
                    && key.is_empty()
                    && previous.is_some_and(|p| p.remember_key)
                {
                    self.credentials.read(id)?
                } else {
                    None
                };
                let secret = if !key.is_empty() {
                    Some(key.as_str())
                } else {
                    retained.as_deref().map(String::as_str)
                };
                if profile.remember_key && secret.is_none() {
                    bail!("请输入需要保存的 API Key");
                }
                let remember = profile.remember_key;
                if let Some(existing) = candidate.ai.profiles.iter_mut().find(|p| p.id == id) {
                    *existing = profile;
                } else {
                    candidate.ai.profiles.push(profile);
                }
                if candidate.ai.default_id.is_none() {
                    candidate.ai.default_id = Some(id);
                }
                candidate.ai.validate()?;
                self.commit_credential(candidate, id, if remember { secret } else { None })?;
                return Ok(Outcome::Committed(self.config.clone()));
            }
            Action::DeleteAi(id) => {
                if !candidate.ai.profiles.iter().any(|p| p.id == id) {
                    bail!("AI 配置不存在");
                }
                candidate.ai.profiles.retain(|p| p.id != id);
                if candidate.ai.default_id == Some(id) {
                    candidate.ai.default_id = candidate.ai.profiles.first().map(|p| p.id);
                }
                self.commit_credential(candidate, id, None)?;
                return Ok(Outcome::Committed(self.config.clone()));
            }
            Action::DefaultAi(id) => {
                if !candidate.ai.profiles.iter().any(|p| p.id == id) {
                    bail!("AI 配置不存在");
                }
                candidate.ai.default_id = Some(id);
            }
            Action::AiBlockedCommands(commands) => {
                candidate.ai.blocked_commands = commands;
                candidate.ai.validate()?;
            }
            Action::ClaudeAgent(settings) => {
                settings.validate()?;
                candidate.ai.claude = settings;
            }
            Action::AiTypography { size, monospace } => {
                candidate.ai.chat_font_size = size;
                candidate.ai.chat_monospace = monospace;
                candidate.ai.validate()?;
            }
            Action::ReadSecret(id) => {
                let remembered = self
                    .config
                    .profiles
                    .iter()
                    .any(|p| p.id == id && p.remember_secret);
                return Ok(Outcome::Secret(if remembered {
                    self.credentials.read(id)?
                } else {
                    None
                }));
            }
            Action::ReadAiKey(id) => {
                let remembered = self
                    .config
                    .ai
                    .profiles
                    .iter()
                    .any(|p| p.id == id && p.remember_key);
                return Ok(Outcome::Secret(if remembered {
                    self.credentials.read(id)?
                } else {
                    None
                }));
            }
            Action::ReadAiHistory => {
                return Ok(Outcome::AiHistory(crate::ai::load_history(
                    &self.path.with_file_name("ai-history.bin"),
                )?));
            }
            Action::SaveAiHistory(history) => {
                crate::ai::save_history(&self.path.with_file_name("ai-history.bin"), &history)?;
                return Ok(Outcome::AiHistorySaved);
            }
            Action::SaveProfile { profile, secret } => {
                profile.validate()?;
                let id = profile.id;
                let remember = profile.remember_secret;
                if let Some(existing) = candidate.profiles.iter_mut().find(|p| p.id == id) {
                    *existing = profile;
                } else {
                    candidate.profiles.push(profile);
                }
                candidate.validate_folders()?;
                self.commit_credential(
                    candidate,
                    id,
                    if remember {
                        Some(secret.as_str())
                    } else {
                        None
                    },
                )?;
                return Ok(Outcome::Committed(self.config.clone()));
            }
            Action::SaveProfileMetadata(profile) => {
                profile.validate()?;
                let existing = candidate
                    .profiles
                    .iter_mut()
                    .find(|p| p.id == profile.id)
                    .context("会话已不存在")?;
                if existing.remember_secret != profile.remember_secret {
                    bail!("保存凭据的设置需要使用保存并连接");
                }
                if existing.remember_secret
                    && (existing.host != profile.host
                        || existing.port != profile.port
                        || existing.username != profile.username
                        || existing.auth != profile.auth
                        || existing.key_path != profile.key_path)
                {
                    bail!("连接信息变更后，请使用保存并连接重新保存凭据");
                }
                *existing = profile;
            }
            Action::SaveFolder(mut folder) => {
                folder.name = folder.name.trim().to_owned();
                if let Some(existing) = candidate.folders.iter_mut().find(|f| f.id == folder.id) {
                    *existing = folder;
                } else {
                    candidate.folders.push(folder);
                }
            }
            Action::DeleteFolder(id) => {
                if candidate.folders.iter().any(|f| f.parent_id == Some(id))
                    || candidate.profiles.iter().any(|p| p.folder_id == Some(id))
                {
                    bail!("只能删除空文件夹，请先移动会话和子文件夹");
                }
                candidate.folders.retain(|f| f.id != id);
            }
            Action::KeepAlive { id, settings } => {
                settings.validate()?;
                let profile = candidate
                    .profiles
                    .iter_mut()
                    .find(|p| p.id == id)
                    .context("会话已删除，请重新选择")?;
                if profile.protocol != crate::config::Protocol::Ssh {
                    bail!("仅 SSH 会话支持保活设置");
                }
                profile.keep_alive = settings;
            }
            Action::MoveProfile { id, folder_id } => {
                candidate
                    .profiles
                    .iter_mut()
                    .find(|p| p.id == id)
                    .context("会话已不存在")?
                    .folder_id = folder_id;
            }
            Action::DeleteProfile(id) => {
                candidate.profiles.retain(|profile| profile.id != id);
                candidate.recent.retain(|recent| *recent != id);
                candidate
                    .shortcut_profiles
                    .retain(|shortcut| *shortcut != id);
                self.commit_credential(candidate, id, None)?;
                return Ok(Outcome::Committed(self.config.clone()));
            }
            Action::ScrollbackLines(lines) => {
                if lines > crate::config::MAX_SCROLLBACK_LINES {
                    bail!("终端回看上限必须在 0–100000 行之间");
                }
                candidate.scrollback_lines = lines;
            }
            Action::ChromeVisibility(chrome) => candidate.chrome = chrome,
            Action::Preferences {
                font_size,
                terminal_font,
                terminal_theme,
                light_theme,
                copy_on_select,
                trim_paste,
            } => {
                if let Some(size) = font_size {
                    if !size.is_finite() || !(10.0..=32.0).contains(&size) {
                        bail!("字号必须在 10–32 之间");
                    }
                    candidate.font_size = size;
                }
                if let Some(font) = terminal_font {
                    candidate.terminal_font = font;
                }
                if let Some(theme) = terminal_theme {
                    candidate.terminal_theme = theme;
                }
                if let Some(value) = copy_on_select {
                    candidate.copy_on_select = value;
                }
                if let Some(value) = trim_paste {
                    candidate.trim_paste = value;
                }
                if let Some(light) = light_theme {
                    candidate.light_theme = light;
                }
            }
            Action::SidebarZoom(zoom) => {
                if !zoom.is_finite() || !(0.75..=1.75).contains(&zoom) {
                    bail!("会话管理器缩放比例必须在 75%–175% 之间");
                }
                candidate.sidebar_zoom = zoom;
            }
            Action::UiAppearance {
                font,
                font_size,
                zoom,
                palette,
                custom_colors,
            } => {
                if let Some(font) = font {
                    candidate.ui_font = font;
                }
                if let Some(font_size) = font_size {
                    if !font_size.is_finite() || !(10.0..=22.0).contains(&font_size) {
                        bail!("界面字号必须在 10–22 之间");
                    }
                    candidate.ui_font_size = font_size;
                }
                if let Some(zoom) = zoom {
                    if !zoom.is_finite() || !(0.75..=1.5).contains(&zoom) {
                        bail!("界面缩放比例必须在 75%–150% 之间");
                    }
                    candidate.ui_zoom = zoom;
                }
                if let Some(palette) = palette {
                    candidate.ui_palette = palette;
                }
                if let Some(colors) = custom_colors {
                    candidate.custom_ui_colors = colors;
                }
            }
            Action::Shortcuts(shortcuts) => {
                if !shortcuts.valid() {
                    bail!("快捷键不能重复，且必须包含 Ctrl 或 Alt；Enter、Tab、Esc 不可用");
                }
                candidate.shortcuts = shortcuts;
            }
            Action::SessionOption(option, enabled) => match option {
                SessionOption::ConfirmExit => candidate.session_options.confirm_exit = enabled,
                SessionOption::ConfirmDisconnect => {
                    candidate.session_options.confirm_disconnect = enabled
                }
                SessionOption::ClearScrollbackOnClose => {
                    candidate.session_options.clear_scrollback_on_close = enabled
                }
                SessionOption::AutoSaveSession => {
                    candidate.session_options.auto_save_session = enabled
                }
                SessionOption::RememberSecretByDefault => {
                    candidate.session_options.remember_secret_by_default = enabled
                }
            },
            Action::Recent(ids) => {
                for id in ids {
                    if candidate.profiles.iter().any(|profile| profile.id == id) {
                        candidate.recent.retain(|recent| *recent != id);
                        candidate.recent.insert(0, id);
                    }
                }
                candidate.recent.truncate(10);
            }
            Action::SetShortcut { id, enabled } => {
                if !candidate.profiles.iter().any(|profile| profile.id == id) {
                    bail!("会话已不存在");
                }
                candidate
                    .shortcut_profiles
                    .retain(|shortcut| *shortcut != id);
                if enabled {
                    candidate.shortcut_profiles.push(id);
                }
            }
            Action::TrustHost { host, reply } => {
                if reply.is_closed() {
                    bail!("连接已取消或确认已超时，未保存主机信任");
                }
                let host = KnownHost::verified(
                    host.host.clone(),
                    host.port,
                    &host.public_key,
                    &host.fingerprint,
                )?;
                match candidate.trust(&host.host, host.port, &host.fingerprint) {
                    Trust::Unknown => candidate.known_hosts.push(host),
                    Trust::Trusted => {}
                    Trust::Changed => bail!("该主机已保存了不同指纹，本次连接被拒绝"),
                }
                candidate.save(&self.path)?;
                if reply.send(true).is_err() {
                    self.config
                        .save(&self.path)
                        .context("连接已结束，撤销刚保存的主机信任失败，请在设置中检查主机记录")?;
                    bail!("连接已取消或确认已超时，主机信任已撤销");
                }
                self.config = candidate;
                return Ok(Outcome::Committed(self.config.clone()));
            }
            Action::ReplaceHost {
                expected,
                replacement,
            } => {
                if !expected.host.eq_ignore_ascii_case(&replacement.host)
                    || expected.port != replacement.port
                {
                    bail!("更新主机密钥不能改变主机和端口");
                }
                let verified = KnownHost::verified(
                    replacement.host.clone(),
                    replacement.port,
                    &replacement.public_key,
                    &replacement.fingerprint,
                )?;
                let position = candidate
                    .known_hosts
                    .iter()
                    .position(|entry| entry == &expected)
                    .context("主机记录已变化，请重新打开后核验")?;
                // Remove all historical keys for this endpoint; only the verified replacement remains trusted.
                candidate.known_hosts.retain(|entry| {
                    !(entry.host.eq_ignore_ascii_case(&expected.host)
                        && entry.port == expected.port)
                });
                candidate
                    .known_hosts
                    .insert(position.min(candidate.known_hosts.len()), verified);
            }
            Action::RemoveHost(expected) => {
                if !candidate.known_hosts.contains(&expected) {
                    bail!("主机记录已变化，请重新打开后操作");
                }
                candidate.known_hosts.retain(|entry| {
                    !(entry.host.eq_ignore_ascii_case(&expected.host)
                        && entry.port == expected.port)
                });
            }
        }
        candidate.save(&self.path)?;
        self.config = candidate;
        Ok(Outcome::Committed(self.config.clone()))
    }

    fn commit_credential(
        &mut self,
        candidate: Config,
        id: Uuid,
        secret: Option<&str>,
    ) -> Result<()> {
        let previous = self.credentials.read(id)?;
        match secret {
            Some(value) => self.credentials.write(id, value)?,
            None => self.credentials.delete(id)?,
        }
        if let Err(error) = candidate.save(&self.path) {
            let restored = match previous {
                Some(value) => self.credentials.write(id, &value),
                None => self.credentials.delete(id),
            };
            if let Err(restore_error) = restored {
                bail!("配置保存失败：{error:#}；凭据恢复失败：{restore_error:#}");
            }
            return Err(error).context("配置保存失败，凭据已恢复");
        }
        self.config = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::Duration,
    };
    #[derive(Default, Clone)]
    struct FakeCredentials(Arc<Mutex<HashMap<Uuid, String>>>);
    impl Credentials for FakeCredentials {
        fn read(&mut self, id: Uuid) -> Result<Option<Zeroizing<String>>> {
            Ok(self.0.lock().unwrap().get(&id).cloned().map(Zeroizing::new))
        }
        fn write(&mut self, id: Uuid, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(id, value.into());
            Ok(())
        }
        fn delete(&mut self, id: Uuid) -> Result<()> {
            self.0.lock().unwrap().remove(&id);
            Ok(())
        }
    }
    fn valid_profile() -> Profile {
        Profile {
            host: "localhost".into(),
            remember_secret: true,
            ..Default::default()
        }
    }

    #[test]
    fn shortcuts_are_explicit_persistent_and_removed_with_profile() {
        let temp = tempfile::tempdir().unwrap();
        let profile = valid_profile();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        assert!(
            state
                .apply(Action::SetShortcut {
                    id: profile.id,
                    enabled: true
                })
                .is_err()
        );
        state
            .apply(Action::SaveProfile {
                profile: profile.clone(),
                secret: Zeroizing::new(String::new()),
            })
            .unwrap();
        state
            .apply(Action::SetShortcut {
                id: profile.id,
                enabled: true,
            })
            .unwrap();
        state
            .apply(Action::SetShortcut {
                id: profile.id,
                enabled: true,
            })
            .unwrap();
        assert_eq!(
            Config::load(&state.path).unwrap().shortcut_profiles,
            vec![profile.id]
        );
        state.apply(Action::DeleteProfile(profile.id)).unwrap();
        assert!(
            Config::load(&state.path)
                .unwrap()
                .shortcut_profiles
                .is_empty()
        );
    }

    #[test]
    fn metadata_autosave_keeps_credentials_and_rejects_identity_change() {
        let temp = tempfile::tempdir().unwrap();
        let profile = valid_profile();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        state
            .apply(Action::SaveProfile {
                profile: profile.clone(),
                secret: Zeroizing::new("test-only-secret".into()),
            })
            .unwrap();
        let mut updated = profile.clone();
        updated.name = "renamed".into();
        state
            .apply(Action::SaveProfileMetadata(updated.clone()))
            .unwrap();
        assert_eq!(
            Config::load(&state.path).unwrap().profiles[0].name,
            "renamed"
        );
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "test-only-secret"
        );
        updated.host = "other.example".into();
        assert!(state.apply(Action::SaveProfileMetadata(updated)).is_err());
        assert_eq!(state.config.profiles[0].host, profile.host);
    }

    #[test]
    fn xts_import_commits_atomically_without_credentials() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("sessions.xts");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        zip.start_file("Xshell/test.xsh", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"[CONNECTION]\nProtocol=TELNET\nHost=localhost\nPort=23\n[CONNECTION:AUTHENTICATION]\nPassword=discard-me").unwrap();
        zip.finish().unwrap();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        let profile = valid_profile();
        state
            .apply(Action::SaveProfile {
                profile: profile.clone(),
                secret: Zeroizing::new("existing-secret".into()),
            })
            .unwrap();
        assert!(matches!(
            state
                .apply(Action::ImportXts(archive_path.clone()))
                .unwrap(),
            Outcome::Imported(_, _)
        ));
        assert_eq!(Config::load(&state.path).unwrap().profiles.len(), 2);
        assert_eq!(state.credentials.0.lock().unwrap().len(), 1);
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "existing-secret"
        );
        let before = serde_json::to_value(&state.config).unwrap();
        state.path = temp.path().join("blocked");
        std::fs::create_dir(&state.path).unwrap();
        assert!(state.apply(Action::ImportXts(archive_path)).is_err());
        assert_eq!(serde_json::to_value(&state.config).unwrap(), before);
        assert_eq!(state.credentials.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn keep_alive_update_preserves_profile_and_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        let mut profile = valid_profile();
        state
            .apply(Action::SaveProfile {
                profile: profile.clone(),
                secret: Zeroizing::new("test-only-secret".into()),
            })
            .unwrap();
        let settings = crate::config::KeepAlive {
            ssh_interval_secs: 120,
            tcp_enabled: true,
            ..Default::default()
        };
        state
            .apply(Action::KeepAlive {
                id: profile.id,
                settings: settings.clone(),
            })
            .unwrap();
        profile.keep_alive = settings.clone();
        assert_eq!(
            serde_json::to_value(&state.config.profiles).unwrap(),
            serde_json::to_value(vec![profile.clone()]).unwrap()
        );
        assert_eq!(
            serde_json::to_value(Config::load(&state.path).unwrap().profiles).unwrap(),
            serde_json::to_value(vec![profile.clone()]).unwrap()
        );
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "test-only-secret"
        );
        let invalid = crate::config::KeepAlive {
            ssh_interval_secs: 0,
            ..settings.clone()
        };
        assert!(
            state
                .apply(Action::KeepAlive {
                    id: profile.id,
                    settings: invalid
                })
                .is_err()
        );
        assert!(
            state
                .apply(Action::KeepAlive {
                    id: Uuid::new_v4(),
                    settings: settings.clone()
                })
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(&state.config.profiles).unwrap(),
            serde_json::to_value(vec![profile.clone()]).unwrap()
        );
        state.config.profiles[0].protocol = crate::config::Protocol::Serial;
        assert!(
            state
                .apply(Action::KeepAlive {
                    id: profile.id,
                    settings
                })
                .is_err()
        );
    }

    #[test]
    fn ai_configuration_lifecycle_keeps_keys_out_of_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        let mut state = State {
            path: path.clone(),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        let claude = crate::claude_agent::Settings {
            sdk_executable: String::new(),
            model: "sonnet".into(),
            max_turns: 8,
        };
        state.apply(Action::ClaudeAgent(claude.clone())).unwrap();
        assert_eq!(Config::load(&path).unwrap().ai.claude, claude);
        let invalid = crate::claude_agent::Settings {
            max_turns: 0,
            ..claude.clone()
        };
        assert!(state.apply(Action::ClaudeAgent(invalid)).is_err());
        assert_eq!(Config::load(&path).unwrap().ai.claude, claude);
        state
            .apply(Action::AiTypography {
                size: 11.0,
                monospace: true,
            })
            .unwrap();
        let restored = Config::load(&path).unwrap();
        assert_eq!(restored.ai.chat_font_size, 11.0);
        assert!(restored.ai.chat_monospace);
        state
            .apply(Action::AiBlockedCommands(vec!["curl".into()]))
            .unwrap();
        assert_eq!(Config::load(&path).unwrap().ai.blocked_commands, ["curl"]);
        assert!(
            state
                .apply(Action::AiBlockedCommands(vec!["curl | sh".into()]))
                .is_err()
        );
        assert_eq!(Config::load(&path).unwrap().ai.blocked_commands, ["curl"]);
        assert!(
            state
                .apply(Action::AiTypography {
                    size: 30.0,
                    monospace: false
                })
                .is_err()
        );
        assert_eq!(Config::load(&path).unwrap().ai.chat_font_size, 11.0);
        let mut profile = crate::ai::AiProfile {
            model: "test-model".into(),
            remember_key: true,
            ..Default::default()
        };
        let id = profile.id;
        state
            .apply(Action::SaveAi {
                profile: profile.clone(),
                key: Zeroizing::new("unique-ai-secret".into()),
            })
            .unwrap();
        assert_eq!(state.config.ai.default_id, Some(id));
        profile.name = "edited".into();
        state
            .apply(Action::SaveAi {
                profile: profile.clone(),
                key: Zeroizing::new(String::new()),
            })
            .unwrap();
        assert_eq!(
            state.credentials.read(id).unwrap().unwrap().as_str(),
            "unique-ai-secret"
        );
        assert_eq!(Config::load(&path).unwrap().ai.profiles[0].name, "edited");
        let second = crate::ai::AiProfile {
            model: "another-model".into(),
            ..Default::default()
        };
        state
            .apply(Action::SaveAi {
                profile: second.clone(),
                key: Zeroizing::new(String::new()),
            })
            .unwrap();
        state.apply(Action::DefaultAi(second.id)).unwrap();
        assert_eq!(state.config.ai.default_id, Some(second.id));
        assert!(state.apply(Action::DefaultAi(Uuid::new_v4())).is_err());
        state.apply(Action::DeleteAi(second.id)).unwrap();
        assert_eq!(state.config.ai.default_id, Some(id));
        profile.remember_key = false;
        state
            .apply(Action::SaveAi {
                profile,
                key: Zeroizing::new(String::new()),
            })
            .unwrap();
        assert!(state.credentials.read(id).unwrap().is_none());
        state.apply(Action::DeleteAi(id)).unwrap();
        assert!(state.config.ai.profiles.is_empty());
        assert!(state.config.ai.default_id.is_none());
        for file in [path.clone(), path.with_extension("json.bak")] {
            assert!(
                !std::fs::read_to_string(file)
                    .unwrap()
                    .contains("unique-ai-secret")
            );
        }
    }

    #[test]
    fn ai_failed_save_and_delete_restore_configuration_and_key() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        let profile = crate::ai::AiProfile {
            model: "model".into(),
            remember_key: true,
            ..Default::default()
        };
        state
            .apply(Action::SaveAi {
                profile: profile.clone(),
                key: Zeroizing::new("original".into()),
            })
            .unwrap();
        let blocker = temp.path().join("blocked");
        std::fs::write(&blocker, b"block").unwrap();
        state.path = blocker.join("config.json");
        assert!(
            state
                .apply(Action::SaveAi {
                    profile: profile.clone(),
                    key: Zeroizing::new("replacement".into())
                })
                .is_err()
        );
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "original"
        );
        assert!(state.apply(Action::DeleteAi(profile.id)).is_err());
        assert_eq!(state.config.ai.profiles, vec![profile.clone()]);
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "original"
        );
    }

    fn host() -> KnownHost {
        let key =
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
                .unwrap();
        KnownHost::verified(
            "server".into(),
            22,
            &key.public_key().to_openssh().unwrap(),
            &key.public_key()
                .fingerprint(russh::keys::HashAlg::Sha256)
                .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn folder_operations_persist_and_preserve_session_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        let parent = Folder {
            id: Uuid::new_v4(),
            name: "生产".into(),
            parent_id: None,
        };
        let mut child = Folder {
            id: Uuid::new_v4(),
            name: "数据库".into(),
            parent_id: Some(parent.id),
        };
        state.apply(Action::SaveFolder(parent.clone())).unwrap();
        state.apply(Action::SaveFolder(child.clone())).unwrap();
        assert_eq!(Config::load(&state.path).unwrap().folders.len(), 2);
        assert!(
            state
                .apply(Action::SaveFolder(Folder {
                    id: Uuid::new_v4(),
                    ..parent.clone()
                }))
                .is_err()
        );
        assert!(
            state
                .apply(Action::SaveFolder(Folder {
                    parent_id: Some(child.id),
                    ..parent.clone()
                }))
                .is_err()
        );
        let profile = valid_profile();
        state
            .apply(Action::SaveProfile {
                profile: profile.clone(),
                secret: Zeroizing::new("keep-secret".into()),
            })
            .unwrap();
        state
            .apply(Action::MoveProfile {
                id: profile.id,
                folder_id: Some(child.id),
            })
            .unwrap();
        assert!(state.apply(Action::DeleteFolder(parent.id)).is_err());
        assert!(state.apply(Action::DeleteFolder(child.id)).is_err());
        child.name = "DB".into();
        state.apply(Action::SaveFolder(child.clone())).unwrap();
        assert_eq!(state.config.folder_path(Some(child.id)), "生产 / DB");
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "keep-secret"
        );
        state
            .apply(Action::MoveProfile {
                id: profile.id,
                folder_id: None,
            })
            .unwrap();
        state.apply(Action::DeleteFolder(child.id)).unwrap();
        let disk = Config::load(&state.path).unwrap();
        assert_eq!(disk.folders, vec![parent]);
        assert_eq!(disk.profiles[0].folder_id, None);
        assert_eq!(disk.profiles[0].id, profile.id);
    }

    #[test]
    fn failed_folder_save_keeps_memory_and_disk_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        state.config.save(&state.path).unwrap();
        std::fs::create_dir(state.path.with_extension("json.bak")).unwrap();
        assert!(
            state
                .apply(Action::SaveFolder(Folder {
                    id: Uuid::new_v4(),
                    name: "empty".into(),
                    parent_id: None
                }))
                .is_err()
        );
        assert!(state.config.folders.is_empty());
        assert!(Config::load(&state.path).unwrap().folders.is_empty());
    }

    #[test]
    fn verified_rotation_revokes_old_key_and_rejects_stale_changes() {
        let temp = tempfile::tempdir().unwrap();
        let old = host();
        let new = host();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        let (reply, mut receiver) = tokio::sync::oneshot::channel();
        state
            .apply(Action::TrustHost {
                host: old.clone(),
                reply,
            })
            .unwrap();
        assert!(receiver.try_recv().unwrap());
        state
            .apply(Action::ReplaceHost {
                expected: old.clone(),
                replacement: new.clone(),
            })
            .unwrap();
        let disk = Config::load(&state.path).unwrap();
        assert_eq!(disk.trust("server", 22, &old.fingerprint), Trust::Changed);
        assert_eq!(disk.trust("server", 22, &new.fingerprint), Trust::Trusted);
        assert!(
            state
                .apply(Action::ReplaceHost {
                    expected: old.clone(),
                    replacement: host()
                })
                .is_err()
        );
        assert!(state.apply(Action::RemoveHost(old)).is_err());
        state
            .apply(Action::RemoveHost(state.config.known_hosts[0].clone()))
            .unwrap();
        assert_eq!(
            Config::load(&state.path)
                .unwrap()
                .trust("server", 22, &new.fingerprint),
            Trust::Unknown
        );
    }

    #[test]
    fn cancelled_confirmation_never_persists_trust() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        let (reply, receiver) = tokio::sync::oneshot::channel();
        drop(receiver);
        assert!(
            state
                .apply(Action::TrustHost {
                    host: host(),
                    reply
                })
                .is_err()
        );
        assert!(state.config.known_hosts.is_empty());
        assert!(!state.path.exists());
    }

    #[test]
    fn failed_rotation_keeps_original_trust_on_disk_and_in_memory() {
        let temp = tempfile::tempdir().unwrap();
        let old = host();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        state.config.known_hosts.push(old.clone());
        state.config.save(&state.path).unwrap();
        // A directory at the backup filename makes the transactional save fail.
        std::fs::create_dir(state.path.with_extension("json.bak")).unwrap();
        assert!(
            state
                .apply(Action::ReplaceHost {
                    expected: old.clone(),
                    replacement: host()
                })
                .is_err()
        );
        assert_eq!(state.config.known_hosts, vec![old.clone()]);
        assert_eq!(Config::load(&state.path).unwrap().known_hosts, vec![old]);
    }

    #[test]
    fn failed_config_commit_restores_credentials_and_memory() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("file-not-directory");
        std::fs::write(&blocker, b"x").unwrap();
        let profile = valid_profile();
        let mut credentials = FakeCredentials::default();
        credentials.write(profile.id, "old-secret").unwrap();
        let mut state = State {
            path: blocker.join("config.json"),
            config: Config::default(),
            credentials,
        };
        assert!(
            state
                .apply(Action::SaveProfile {
                    profile: profile.clone(),
                    secret: Zeroizing::new("new-secret".into())
                })
                .is_err()
        );
        assert!(state.config.profiles.is_empty());
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "old-secret"
        );
    }

    #[test]
    fn serial_updates_merge_instead_of_overwriting_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        let store = Store::start(
            path.clone(),
            Config::default(),
            FakeCredentials::default(),
            || {},
        )
        .unwrap();
        let profile = valid_profile();
        let first = store
            .submit(Action::SaveProfile {
                profile: profile.clone(),
                secret: Zeroizing::new("unique-plaintext-test-value".into()),
            })
            .unwrap();
        let second = store
            .submit(Action::Preferences {
                font_size: Some(24.0),
                terminal_font: None,
                terminal_theme: None,
                light_theme: None,
                copy_on_select: Some(false),
                trim_paste: Some(false),
            })
            .unwrap();
        let third = store.submit(Action::Recent(vec![profile.id])).unwrap();
        let fourth = store.submit(Action::SidebarZoom(1.3)).unwrap();
        for id in [first, second, third, fourth] {
            let completion = store
                .receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            assert_eq!(completion.id, id);
            assert!(completion.result.is_ok());
        }
        let config = Config::load(&path).unwrap();
        assert_eq!(config.profiles[0].id, profile.id);
        assert_eq!(config.recent, vec![profile.id]);
        assert_eq!(config.font_size, 24.0);
        assert_eq!(config.sidebar_zoom, 1.3);
        assert!(!config.copy_on_select);
        assert!(!config.trim_paste);
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("unique-plaintext-test-value")
        );
    }

    #[test]
    fn credential_api_runs_off_calling_thread() {
        struct ThreadChecking {
            caller: thread::ThreadId,
        }
        impl Credentials for ThreadChecking {
            fn read(&mut self, _: Uuid) -> Result<Option<Zeroizing<String>>> {
                assert_ne!(thread::current().id(), self.caller);
                Ok(None)
            }
            fn write(&mut self, _: Uuid, _: &str) -> Result<()> {
                assert_ne!(thread::current().id(), self.caller);
                Ok(())
            }
            fn delete(&mut self, _: Uuid) -> Result<()> {
                assert_ne!(thread::current().id(), self.caller);
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let store = Store::start(
            temp.path().join("config.json"),
            Config::default(),
            ThreadChecking {
                caller: thread::current().id(),
            },
            || {},
        )
        .unwrap();
        store
            .submit(Action::SaveProfile {
                profile: valid_profile(),
                secret: Zeroizing::new("test".into()),
            })
            .unwrap();
        assert!(
            store
                .receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .result
                .is_ok()
        );
    }

    #[test]
    fn failed_delete_restores_secret_and_profile() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("blocked");
        std::fs::write(&blocker, b"x").unwrap();
        let profile = valid_profile();
        let mut config = Config::default();
        config.profiles.push(profile.clone());
        let mut credentials = FakeCredentials::default();
        credentials.write(profile.id, "keep").unwrap();
        let mut state = State {
            path: blocker.join("config.json"),
            config,
            credentials,
        };
        assert!(state.apply(Action::DeleteProfile(profile.id)).is_err());
        assert_eq!(state.config.profiles.len(), 1);
        assert_eq!(
            state
                .credentials
                .read(profile.id)
                .unwrap()
                .unwrap()
                .as_str(),
            "keep"
        );
    }
    #[test]
    fn history_and_chrome_settings_persist_and_reject_oversized_history() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/settings-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let mut state = State {
            path: temp.path().join("config.json"),
            config: Config::default(),
            credentials: FakeCredentials::default(),
        };
        state.apply(Action::ScrollbackLines(20000)).unwrap();
        let hidden = crate::config::ChromeVisibility {
            menu: false,
            toolbar: false,
            address: false,
            shortcuts: false,
        };
        state.apply(Action::ChromeVisibility(hidden)).unwrap();
        let saved = Config::load(&state.path).unwrap();
        assert_eq!(saved.scrollback_lines, 20000);
        assert_eq!(saved.chrome, hidden);
        assert!(!saved.chrome.any());
        assert!(
            state
                .apply(Action::ScrollbackLines(
                    crate::config::MAX_SCROLLBACK_LINES + 1
                ))
                .is_err()
        );
        assert_eq!(Config::load(&state.path).unwrap().scrollback_lines, 20000);
        state.apply(Action::ScrollbackLines(0)).unwrap();
        state
            .apply(Action::ChromeVisibility(Default::default()))
            .unwrap();
        assert_eq!(Config::load(&state.path).unwrap().scrollback_lines, 0);
        assert!(Config::load(&state.path).unwrap().chrome.any());
        let mut invalid = state.config.clone();
        invalid.scrollback_lines = usize::MAX;
        invalid.save(&state.path).unwrap();
        assert!(Config::load(&state.path).is_err());
    }
}
