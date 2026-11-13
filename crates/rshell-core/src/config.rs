use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum AuthMethod {
    #[default]
    Password,
    PrivateKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct KeepAlive {
    pub ssh_enabled: bool,
    pub ssh_interval_secs: u64,
    pub idle_enabled: bool,
    pub idle_interval_secs: u64,
    pub idle_text: String,
    pub tcp_enabled: bool,
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self {
            ssh_enabled: true,
            ssh_interval_secs: 60,
            idle_enabled: false,
            idle_interval_secs: 60,
            idle_text: String::new(),
            tcp_enabled: false,
        }
    }
}

impl KeepAlive {
    pub fn validate(&self) -> Result<()> {
        if (self.ssh_enabled && !(1..=86400).contains(&self.ssh_interval_secs))
            || (self.idle_enabled && !(1..=86400).contains(&self.idle_interval_secs))
        {
            bail!("保持活动间隔必须在 1–86400 秒之间");
        }
        if self.idle_text.len() > 1024 || (self.idle_enabled && self.idle_text.is_empty()) {
            bail!("空闲发送字符串不能为空，且不得超过 1024 UTF-8 字节");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Protocol {
    #[default]
    Ssh,
    Telnet,
    Serial,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SerialSettings {
    pub port_name: String,
    pub baud_rate: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: SerialParity,
    pub flow_control: SerialFlowControl,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SerialParity {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SerialFlowControl {
    #[default]
    None,
    Software,
    Hardware,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            port_name: if cfg!(target_os = "macos") {
                "/dev/cu.usbserial"
            } else {
                "COM1"
            }
            .into(),
            baud_rate: 115200,
            data_bits: 8,
            stop_bits: 1,
            parity: SerialParity::None,
            flow_control: SerialFlowControl::None,
        }
    }
}

impl SerialSettings {
    pub fn validate(&self) -> Result<()> {
        let name = self.port_name.to_ascii_uppercase();
        let number = name.strip_prefix("COM").unwrap_or("");
        let windows_port = !number.is_empty()
            && number.bytes().all(|c| c.is_ascii_digit())
            && !number.starts_with('0')
            && number.parse::<u16>().is_ok();
        let macos_port = cfg!(target_os = "macos")
            && self.port_name.strip_prefix("/dev/").is_some_and(|name| {
                (name.starts_with("cu.") || name.starts_with("tty."))
                    && name.len() > 4
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            });
        if !windows_port && !macos_port {
            bail!("请输入有效的串口名称：Windows 使用 COM1，macOS 使用 /dev/cu.* 或 /dev/tty.*");
        }
        if self.baud_rate == 0
            || !(5..=8).contains(&self.data_bits)
            || !(1..=2).contains(&self.stop_bits)
        {
            bail!("波特率必须大于 0，数据位为 5–8，停止位为 1 或 2");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub serial: SerialSettings,
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing)]
    pub group: String,
    #[serde(default)]
    pub folder_id: Option<Uuid>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub key_path: String,
    #[serde(default)]
    pub remember_secret: bool,
    #[serde(default)]
    pub keep_alive: KeepAlive,
    #[serde(default)]
    pub tunnels: Vec<crate::tunnel::Rule>,
    #[serde(default)]
    pub tunnel_only: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            protocol: Protocol::Ssh,
            serial: SerialSettings::default(),
            id: Uuid::new_v4(),
            name: "新会话".into(),
            group: String::new(),
            folder_id: None,
            host: String::new(),
            port: 22,
            username: "root".into(),
            auth: AuthMethod::Password,
            key_path: String::new(),
            remember_secret: false,
            keep_alive: KeepAlive::default(),
            tunnels: Vec::new(),
            tunnel_only: false,
        }
    }
}

impl Profile {
    pub fn endpoint(&self) -> String {
        match self.protocol {
            Protocol::Ssh => format!("{}@{}:{}", self.username, self.host, self.port),
            Protocol::Telnet => format!("telnet://{}:{}", self.host, self.port),
            Protocol::Serial => {
                format!("{} · {} baud", self.serial.port_name, self.serial.baud_rate)
            }
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_portable()?;
        if self.protocol == Protocol::Ssh {
            if self.username.trim().is_empty() {
                bail!("名称、用户名不能为空，端口必须在 1–65535 之间");
            }
            if self.auth == AuthMethod::PrivateKey && self.key_path.trim().is_empty() {
                bail!("请选择私钥文件路径");
            }
        }
        Ok(())
    }

    /// Portable sessions may omit login details that must be supplied before connecting.
    pub(crate) fn validate_portable(&self) -> Result<()> {
        crate::tunnel::validate(&self.tunnels, self.tunnel_only)?;
        if self.protocol != Protocol::Ssh
            && (self.tunnel_only || self.tunnels.iter().any(|r| r.enabled))
        {
            bail!("隧道仅支持 SSH 会话，请先禁用隧道规则和仅隧道模式");
        }
        if self.name.trim().is_empty() {
            bail!("名称不能为空");
        }
        if self.protocol == Protocol::Serial {
            if self.remember_secret {
                bail!("串口会话不使用登录凭据");
            }
            return self.serial.validate();
        }
        self.keep_alive.validate()?;
        if self.host.trim().is_empty() || self.host.chars().any(char::is_whitespace) {
            bail!("请输入有效的主机名或 IP（不含空白）");
        }
        if self.protocol == Protocol::Telnet {
            if self.port == 0 || self.remember_secret {
                bail!("Telnet 端口必须在 1–65535 之间，登录凭据请在终端手动输入");
            }
            return Ok(());
        }
        if self.port == 0 {
            bail!("名称、用户名不能为空，端口必须在 1–65535 之间");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownHost {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    #[serde(default)]
    pub algorithm: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub confirmed_at: Option<u64>,
}

impl KnownHost {
    /// Construct a record from an independently verified OpenSSH public key.
    pub fn verified(host: String, port: u16, public_key: &str, expected: &str) -> Result<Self> {
        if host.trim().is_empty() || host.chars().any(char::is_whitespace) || port == 0 {
            bail!("主机或端口无效");
        }
        let key = russh::keys::ssh_key::PublicKey::from_openssh(public_key.trim())
            .context("请输入 OpenSSH 格式的主机公钥（不是私钥）")?;
        let fingerprint = key.fingerprint(russh::keys::HashAlg::Sha256).to_string();
        if fingerprint != expected.trim() {
            bail!("公钥与核验的 SHA-256 指纹不一致，未更改信任记录");
        }
        Ok(Self {
            host,
            port,
            fingerprint,
            algorithm: key.algorithm().to_string(),
            public_key: key.to_openssh()?,
            confirmed_at: Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
            ),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Trust {
    Unknown,
    Trusted,
    Changed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    /// Local CLI opt-in; not an authorization boundary against the same OS user.
    #[serde(default)]
    pub cli_enabled: bool,
    #[serde(default)]
    pub jumpserver: Option<crate::jumpserver::Saved>,
    #[serde(default)]
    pub ai: crate::ai::AiSettings,
    pub schema_version: u32,
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub folders: Vec<Folder>,
    /// Manual sibling order; absent entries keep the legacy folder/name ordering.
    #[serde(default)]
    pub session_order: Vec<crate::session_tree::Item>,
    pub known_hosts: Vec<KnownHost>,
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    #[serde(default)]
    pub chrome: ChromeVisibility,
    pub font_size: f32,
    #[serde(default = "default_ui_font_size")]
    pub ui_font_size: f32,
    #[serde(default = "default_ui_zoom")]
    pub ui_zoom: f32,
    #[serde(default)]
    pub ui_font: UiFont,
    #[serde(default)]
    pub ui_palette: UiPalette,
    #[serde(default)]
    pub custom_ui_colors: CustomUiColors,
    #[serde(default)]
    pub terminal_font: TerminalFont,
    #[serde(default)]
    pub terminal_theme: TerminalTheme,
    #[serde(default = "default_sidebar_zoom")]
    pub sidebar_zoom: f32,
    #[serde(default = "enabled")]
    pub copy_on_select: bool,
    #[serde(default = "enabled")]
    pub trim_paste: bool,
    #[serde(default)]
    pub light_theme: bool,
    #[serde(default)]
    pub recent: Vec<Uuid>,
    #[serde(default)]
    pub shortcut_profiles: Vec<Uuid>,
    #[serde(default)]
    pub shortcuts: Shortcuts,
    #[serde(default)]
    pub session_options: SessionOptions,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyBinding {
    pub fn new(key: &str, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self {
            key: key.into(),
            ctrl,
            alt,
            shift,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Shortcuts {
    pub new_local: KeyBinding,
    pub close_tab: KeyBinding,
    pub previous_tab: KeyBinding,
    pub next_tab: KeyBinding,
    pub previous_tab_alt: KeyBinding,
    pub next_tab_alt: KeyBinding,
    pub copy_selection: KeyBinding,
    pub search_terminal: KeyBinding,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            new_local: KeyBinding::new("N", false, true, true),
            close_tab: KeyBinding::new("F4", true, false, true),
            previous_tab: KeyBinding::new("PageUp", true, false, false),
            next_tab: KeyBinding::new("PageDown", true, false, false),
            previous_tab_alt: KeyBinding::new("ArrowLeft", true, true, false),
            next_tab_alt: KeyBinding::new("ArrowRight", true, true, false),
            copy_selection: KeyBinding::new("C", true, false, true),
            search_terminal: KeyBinding::new("F", true, false, true),
        }
    }
}

impl Shortcuts {
    fn migrate_shell_search(&mut self) {
        if self.search_terminal == KeyBinding::new("F", true, false, false) {
            let replacement = KeyBinding::new("F", true, false, true);
            // Preserve a user binding already using the new combination.
            if !self.bindings().contains(&&replacement) {
                self.search_terminal = replacement;
            }
        }
    }

    pub fn bindings(&self) -> [&KeyBinding; 8] {
        [
            &self.new_local,
            &self.close_tab,
            &self.previous_tab,
            &self.next_tab,
            &self.previous_tab_alt,
            &self.next_tab_alt,
            &self.copy_selection,
            &self.search_terminal,
        ]
    }

    pub fn binding_mut(&mut self, index: usize) -> &mut KeyBinding {
        match index {
            0 => &mut self.new_local,
            1 => &mut self.close_tab,
            2 => &mut self.previous_tab,
            3 => &mut self.next_tab,
            4 => &mut self.previous_tab_alt,
            5 => &mut self.next_tab_alt,
            6 => &mut self.copy_selection,
            7 => &mut self.search_terminal,
            _ => unreachable!(),
        }
    }

    pub fn valid(&self) -> bool {
        let bindings = self.bindings();
        bindings.iter().all(|binding| {
            (binding.ctrl || binding.alt)
                && !binding.key.is_empty()
                && !matches!(binding.key.as_str(), "Escape" | "Enter" | "Tab")
                && !(binding.ctrl
                    && !binding.shift
                    && matches!(binding.key.as_str(), "C" | "V" | "X"))
        }) && bindings
            .iter()
            .enumerate()
            .all(|(i, binding)| !bindings[..i].contains(binding))
    }
}

#[cfg(test)]
mod shortcut_tests {
    use super::{Config, KeyBinding, Shortcuts};

    #[test]
    fn search_migration_frees_ctrl_f_without_overwriting_other_bindings() {
        let mut shortcuts = Shortcuts {
            search_terminal: KeyBinding::new("F", true, false, false),
            ..Default::default()
        };
        shortcuts.migrate_shell_search();
        assert_eq!(shortcuts, Shortcuts::default());
        shortcuts.search_terminal = KeyBinding::new("R", true, false, true);
        shortcuts.migrate_shell_search();
        assert_eq!(shortcuts.search_terminal.key, "R");
        shortcuts.search_terminal = KeyBinding::new("F", true, false, false);
        shortcuts.new_local = KeyBinding::new("F", true, false, true);
        let previous = shortcuts.clone();
        shortcuts.migrate_shell_search();
        assert_eq!(shortcuts, previous);
    }

    #[test]
    fn old_config_uses_default_shortcuts_and_round_trips() {
        let mut value = serde_json::to_value(Config::default()).unwrap();
        value.as_object_mut().unwrap().remove("shortcuts");
        let restored: Config = serde_json::from_value(value).unwrap();
        assert_eq!(restored.shortcuts, Shortcuts::default());
        let saved: Config =
            serde_json::from_str(&serde_json::to_string(&restored).unwrap()).unwrap();
        assert_eq!(saved.shortcuts, restored.shortcuts);
    }

    #[test]
    fn shortcut_conflicts_are_rejected() {
        let mut shortcuts = Shortcuts::default();
        assert!(shortcuts.valid());
        shortcuts.next_tab = shortcuts.previous_tab.clone();
        assert!(!shortcuts.valid());
        shortcuts.next_tab = KeyBinding::new("Tab", true, false, false);
        assert!(!shortcuts.valid());
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UiFont {
    #[default]
    Geist,
    System,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UiPalette {
    Rshell,
    Pearl,
    Mist,
    Graphite,
    Cobalt,
    Sage,
    Amber,
    Blush,
    #[default]
    VsCode,
    Idea,
    Xcode,
    JetBrains,
    Cursor,
    Claude,
    Custom,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CustomUiColors {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub primary: [u8; 3],
    pub border: [u8; 3],
    pub sidebar: [u8; 3],
}

impl Default for CustomUiColors {
    fn default() -> Self {
        Self {
            background: [255, 255, 255],
            foreground: [30, 35, 42],
            primary: [44, 99, 183],
            border: [193, 210, 229],
            sidebar: [232, 240, 250],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalFont {
    #[default]
    GeistMono,
    FiraCode,
    JetBrainsMono,
    CascadiaCode,
    Consolas,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalTheme {
    #[default]
    Xterm,
    OneDark,
    VsDark,
    VsLight,
    Nord,
    Okaidia,
    Material,
    DuotoneLight,
    DuotoneDark,
    Xcode,
    XcodeDark,
    IdeaLight,
    IdeaDark,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DefaultShell {
    #[default]
    Builtin,
    GitBash,
    PowerShell,
}
impl DefaultShell {
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "RShell 内置 Shell（rubash）",
            Self::GitBash => "Git Bash",
            Self::PowerShell => "本地 PowerShell",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionOptions {
    /// Legacy two-choice setting, read only when default_shell is absent.
    pub use_powershell: bool,
    pub default_shell: Option<DefaultShell>,
    pub git_bash_path: String,
    pub powershell_path: String,
    pub confirm_exit: bool,
    pub confirm_disconnect: bool,
    pub clear_scrollback_on_close: bool,
    pub auto_save_session: bool,
    pub remember_secret_by_default: bool,
}

impl SessionOptions {
    pub fn selected_shell(&self) -> DefaultShell {
        self.default_shell.unwrap_or(if self.use_powershell {
            DefaultShell::PowerShell
        } else {
            DefaultShell::Builtin
        })
    }
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            use_powershell: false,
            default_shell: None,
            git_bash_path: String::new(),
            powershell_path: String::new(),
            confirm_exit: true,
            confirm_disconnect: true,
            clear_scrollback_on_close: false,
            auto_save_session: true,
            remember_secret_by_default: true,
        }
    }
}

fn enabled() -> bool {
    true
}

fn default_sidebar_zoom() -> f32 {
    1.0
}

pub const MAX_SCROLLBACK_LINES: usize = 100_000;

fn default_scrollback_lines() -> usize {
    5000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChromeVisibility {
    pub menu: bool,
    pub toolbar: bool,
    pub address: bool,
    pub shortcuts: bool,
}

impl Default for ChromeVisibility {
    fn default() -> Self {
        Self {
            menu: true,
            toolbar: true,
            address: true,
            shortcuts: true,
        }
    }
}

impl ChromeVisibility {
    pub fn any(self) -> bool {
        self.menu || self.toolbar || self.address || self.shortcuts
    }
}

fn default_ui_font_size() -> f32 {
    13.0
}

fn default_ui_zoom() -> f32 {
    1.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cli_enabled: false,
            schema_version: 4,
            jumpserver: None,
            ai: Default::default(),
            folders: vec![],
            session_order: vec![],
            profiles: vec![],
            known_hosts: vec![],
            scrollback_lines: default_scrollback_lines(),
            chrome: ChromeVisibility::default(),
            font_size: 16.0,
            ui_font_size: default_ui_font_size(),
            ui_zoom: default_ui_zoom(),
            ui_font: UiFont::default(),
            ui_palette: UiPalette::default(),
            custom_ui_colors: CustomUiColors::default(),
            terminal_font: TerminalFont::default(),
            terminal_theme: TerminalTheme::default(),
            sidebar_zoom: default_sidebar_zoom(),
            copy_on_select: true,
            trim_paste: true,
            light_theme: false,
            recent: vec![],
            shortcut_profiles: vec![],
            shortcuts: Shortcuts::default(),
            session_options: SessionOptions::default(),
        }
    }
}

impl Config {
    pub fn folder_path(&self, id: Option<Uuid>) -> String {
        let mut parts = Vec::new();
        let mut current = id;
        for _ in 0..32 {
            let Some(folder) = current.and_then(|id| self.folders.iter().find(|f| f.id == id))
            else {
                break;
            };
            parts.push(folder.name.as_str());
            current = folder.parent_id;
        }
        parts.reverse();
        if parts.is_empty() {
            "所有会话".into()
        } else {
            parts.join(" / ")
        }
    }

    pub fn validate_folders(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for folder in &self.folders {
            if folder.name.trim().is_empty() || !ids.insert(folder.id) {
                bail!("文件夹名称不能为空，ID 不能重复");
            }
            if self.folders.iter().any(|f| {
                f.id != folder.id && f.parent_id == folder.parent_id && f.name == folder.name
            }) {
                bail!("同一目录下已存在同名文件夹");
            }
            let mut seen = std::collections::HashSet::new();
            let mut current = Some(folder.id);
            while let Some(id) = current {
                if !seen.insert(id) || seen.len() > 32 {
                    bail!("文件夹不能循环嵌套，最多支持 32 层");
                }
                current = self
                    .folders
                    .iter()
                    .find(|f| f.id == id)
                    .context("父文件夹不存在")?
                    .parent_id;
            }
        }
        for profile in &self.profiles {
            if let Some(id) = profile.folder_id
                && !ids.contains(&id)
            {
                bail!("会话所属文件夹不存在");
            }
        }
        Ok(())
    }

    pub fn trust(&self, host: &str, port: u16, fingerprint: &str) -> Trust {
        let entries: Vec<_> = self
            .known_hosts
            .iter()
            .filter(|k| k.host.eq_ignore_ascii_case(host) && k.port == port)
            .collect();
        if entries.is_empty() {
            Trust::Unknown
        } else if entries.iter().any(|k| k.fingerprint == fingerprint) {
            Trust::Trusted
        } else {
            Trust::Changed
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(data) => {
                let mut config: Self = serde_json::from_slice(&data)
                    .context("配置损坏，请保留原文件并检查 .bak 备份")?;
                config.shortcuts.migrate_shell_search();
                if (1..=3).contains(&config.schema_version) {
                    for profile in &mut config.profiles {
                        if !profile.group.is_empty() {
                            let id = if let Some(folder) = config
                                .folders
                                .iter()
                                .find(|f| f.name == profile.group && f.parent_id.is_none())
                            {
                                folder.id
                            } else {
                                let id = Uuid::new_v4();
                                config.folders.push(Folder {
                                    id,
                                    name: profile.group.clone(),
                                    parent_id: None,
                                });
                                id
                            };
                            profile.folder_id = Some(id);
                        }
                        profile.group.clear();
                    }
                    config.schema_version = 4;
                }
                if config.schema_version != 4 {
                    bail!("不支持的配置版本 {}", config.schema_version);
                }
                for profile in &config.profiles {
                    if profile.protocol != Protocol::Ssh {
                        profile.validate()?;
                    } else {
                        profile.keep_alive.validate()?;
                        crate::tunnel::validate(&profile.tunnels, profile.tunnel_only)?;
                    }
                }
                config.validate_folders()?;
                config.ai.validate()?;
                if config.scrollback_lines > MAX_SCROLLBACK_LINES {
                    bail!("终端回看上限必须在 0–100000 行之间");
                }
                if !config.font_size.is_finite() || !(10.0..=32.0).contains(&config.font_size) {
                    bail!("配置中的字号无效");
                }
                if !config.ui_font_size.is_finite() || !(10.0..=22.0).contains(&config.ui_font_size)
                {
                    bail!("配置中的界面字号无效");
                }
                if !config.ui_zoom.is_finite() || !(0.75..=1.5).contains(&config.ui_zoom) {
                    bail!("配置中的界面缩放比例无效");
                }
                if !config.sidebar_zoom.is_finite() || !(0.75..=1.75).contains(&config.sidebar_zoom)
                {
                    bail!("配置中的会话管理器缩放比例无效");
                }
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("读取配置失败"),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        for profile in &self.profiles {
            if profile.protocol != Protocol::Ssh {
                profile.validate()?;
            } else {
                profile.keep_alive.validate()?;
                crate::tunnel::validate(&profile.tunnels, profile.tunnel_only)?;
            }
        }
        self.validate_folders()?;
        self.ai.validate()?;
        let parent = path.parent().context("配置目录无效")?;
        fs::create_dir_all(parent)?;
        let tmp: PathBuf = parent.join(format!("config-{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            drop(file);
            if path.exists() {
                fs::copy(path, path.with_extension("json.bak"))?;
            }
            fs::rename(&tmp, path).context("替换配置失败，原配置仍保留")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_is_opt_in_for_new_and_legacy_configs() {
        assert!(!super::Config::default().cli_enabled);
        let mut legacy = serde_json::to_value(super::Config::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("cli_enabled");
        assert!(
            !serde_json::from_value::<super::Config>(legacy)
                .unwrap()
                .cli_enabled
        );
    }
    #[test]
    fn sidebar_zoom_defaults_for_legacy_configs() {
        let mut value = serde_json::to_value(super::Config::default()).unwrap();
        value.as_object_mut().unwrap().remove("sidebar_zoom");
        let config: super::Config = serde_json::from_value(value).unwrap();
        assert_eq!(config.sidebar_zoom, 1.0);
    }

    #[test]
    fn clipboard_preferences_default_on_for_legacy_configs() {
        let mut value = serde_json::to_value(super::Config::default()).unwrap();
        value.as_object_mut().unwrap().remove("copy_on_select");
        value.as_object_mut().unwrap().remove("trim_paste");
        let config: super::Config = serde_json::from_value(value).unwrap();
        assert!(config.copy_on_select && config.trim_paste);
    }

    #[test]
    fn session_options_default_for_legacy_configs_and_roundtrip() {
        let mut value = serde_json::to_value(super::Config::default()).unwrap();
        value.as_object_mut().unwrap().remove("session_options");
        let config: super::Config = serde_json::from_value(value).unwrap();
        assert_eq!(config.session_options, super::SessionOptions::default());
        assert!(config.session_options.remember_secret_by_default);
        assert!(!config.session_options.use_powershell);
        let mut powershell = config.clone();
        powershell.session_options.use_powershell = true;
        let powershell: super::Config =
            serde_json::from_str(&serde_json::to_string(&powershell).unwrap()).unwrap();
        assert!(powershell.session_options.use_powershell);
        let restored: super::Config =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(restored.session_options, config.session_options);

        let mut legacy = serde_json::to_value(restored).unwrap();
        legacy["session_options"]
            .as_object_mut()
            .unwrap()
            .remove("remember_secret_by_default");
        let legacy: super::Config = serde_json::from_value(legacy).unwrap();
        assert!(legacy.session_options.remember_secret_by_default);
    }

    use super::*;

    #[test]
    fn keep_alive_defaults_roundtrip_and_validation() {
        let mut value = serde_json::to_value(Profile::default()).unwrap();
        value.as_object_mut().unwrap().remove("keep_alive");
        let legacy: Profile = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.keep_alive, KeepAlive::default());
        let mut settings = KeepAlive {
            ssh_enabled: false,
            idle_enabled: true,
            idle_interval_secs: 1,
            idle_text: "保活".into(),
            tcp_enabled: true,
            ..Default::default()
        };
        settings.validate().unwrap();
        let roundtrip: KeepAlive =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(roundtrip, settings);
        settings.idle_interval_secs = 0;
        assert!(settings.validate().is_err());
        settings.idle_interval_secs = 86401;
        assert!(settings.validate().is_err());
        settings.idle_interval_secs = 1;
        settings.idle_text.clear();
        assert!(settings.validate().is_err());
        settings.idle_text = "中".repeat(342);
        assert!(settings.validate().is_err());
        settings.idle_enabled = false;
        settings.idle_text.clear();
        settings.ssh_enabled = true;
        settings.ssh_interval_secs = 0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn legacy_groups_migrate_to_folders_without_changing_profile_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let profile = Profile::default();
        let mut value = serde_json::to_value(Config::default()).unwrap();
        value["schema_version"] = 3.into();
        value.as_object_mut().unwrap().remove("folders");
        let mut legacy = serde_json::to_value(&profile).unwrap();
        legacy["group"] = "生产/旧标签".into();
        legacy.as_object_mut().unwrap().remove("folder_id");
        value["profiles"] = serde_json::json!([legacy]);
        let original = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &original).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.profiles[0].id, profile.id);
        assert_eq!(config.folders.len(), 1);
        assert_eq!(config.folders[0].name, "生产/旧标签");
        assert_eq!(config.profiles[0].folder_id, Some(config.folders[0].id));
        assert_eq!(fs::read(&path).unwrap(), original);
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap().folders, config.folders);
    }

    #[test]
    fn invalid_folder_graphs_and_orphan_sessions_are_rejected() {
        let id = Uuid::new_v4();
        let mut config = Config::default();
        config.folders.push(Folder {
            id,
            name: "A".into(),
            parent_id: Some(id),
        });
        assert!(config.validate_folders().is_err());
        config.folders[0].parent_id = Some(Uuid::new_v4());
        assert!(config.validate_folders().is_err());
        config.folders[0].parent_id = None;
        config.profiles.push(Profile {
            folder_id: Some(Uuid::new_v4()),
            ..Default::default()
        });
        assert!(config.validate_folders().is_err());
    }

    #[test]
    fn trust_is_bound_to_host_port_and_fingerprint() {
        let mut c = Config::default();
        c.known_hosts.push(KnownHost {
            host: "server".into(),
            port: 22,
            fingerprint: "SHA256:one".into(),
            algorithm: String::new(),
            public_key: String::new(),
            confirmed_at: None,
        });
        assert_eq!(c.trust("SERVER", 22, "SHA256:one"), Trust::Trusted);
        assert_eq!(c.trust("server", 22, "SHA256:two"), Trust::Changed);
        assert_eq!(c.trust("server", 2222, "SHA256:one"), Trust::Unknown);
    }

    #[test]
    fn atomic_save_replaces_file_and_keeps_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut c = Config::default();
        c.save(&path).unwrap();
        c.profiles.push(Profile::default());
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap().profiles.len(), 1);
        assert!(
            Config::load(&path.with_extension("json.bak"))
                .unwrap()
                .profiles
                .is_empty()
        );
    }

    #[test]
    fn invalid_config_is_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, b"broken").unwrap();
        assert!(Config::load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
    }

    #[test]
    fn terminal_appearance_defaults_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let old = r#"{"schema_version":4,"profiles":[],"known_hosts":[],"font_size":16.0}"#;
        fs::write(&path, old).unwrap();
        let mut config = Config::load(&path).unwrap();
        assert_eq!(config.scrollback_lines, 5000);
        assert_eq!(config.chrome, ChromeVisibility::default());
        assert_eq!(config.terminal_font, TerminalFont::GeistMono);
        assert_eq!(config.terminal_theme, TerminalTheme::Xterm);
        assert_eq!(config.ui_font, UiFont::Geist);
        assert_eq!(config.ui_font_size, 13.0);
        assert_eq!(config.ui_zoom, 1.0);
        assert_eq!(config.ui_palette, UiPalette::VsCode);
        config.terminal_font = TerminalFont::CascadiaCode;
        config.terminal_theme = TerminalTheme::Nord;
        config.ui_font = UiFont::System;
        config.ui_font_size = 16.0;
        config.ui_zoom = 1.25;
        config.ui_palette = UiPalette::Custom;
        config.custom_ui_colors.background = [24, 32, 40];
        config.save(&path).unwrap();
        let restored = Config::load(&path).unwrap();
        assert_eq!(restored.terminal_font, TerminalFont::CascadiaCode);
        assert_eq!(restored.terminal_theme, TerminalTheme::Nord);
        assert_eq!(restored.ui_font, UiFont::System);
        assert_eq!(restored.ui_font_size, 16.0);
        assert_eq!(restored.ui_zoom, 1.25);
        assert_eq!(restored.ui_palette, UiPalette::Custom);
        assert_eq!(restored.custom_ui_colors.background, [24, 32, 40]);
    }

    #[test]
    fn version_one_migrates_without_touching_source_until_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let old = r#"{"schema_version":1,"profiles":[],"known_hosts":[],"font_size":16.0}"#;
        fs::write(&path, old).unwrap();
        let migrated = Config::load(&path).unwrap();
        assert!(migrated.ai.profiles.is_empty());
        assert!(migrated.ai.default_id.is_none());
        assert_eq!(migrated.schema_version, 4);
        assert_eq!(fs::read_to_string(&path).unwrap(), old);
        migrated.save(&path).unwrap();
        assert_eq!(
            fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            old
        );
    }

    #[test]
    fn version_two_host_records_keep_trust_without_inventing_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let old = r#"{"schema_version":2,"profiles":[],"known_hosts":[{"host":"server","port":22,"fingerprint":"SHA256:old"}],"font_size":16.0}"#;
        fs::write(&path, old).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.schema_version, 4);
        assert_eq!(config.trust("server", 22, "SHA256:old"), Trust::Trusted);
        assert!(config.known_hosts[0].public_key.is_empty());
        assert_eq!(config.known_hosts[0].confirmed_at, None);
        assert_eq!(fs::read_to_string(path).unwrap(), old);
    }

    #[test]
    fn verified_host_requires_matching_public_key_and_fingerprint() {
        let key =
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
                .unwrap();
        let public = key.public_key().to_openssh().unwrap();
        let fingerprint = key
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        let host = KnownHost::verified("server".into(), 22, &public, &fingerprint).unwrap();
        assert_eq!(host.algorithm, "ssh-ed25519");
        assert!(host.confirmed_at.unwrap() > 0);
        assert!(KnownHost::verified("server".into(), 22, &public, "SHA256:wrong").is_err());
        assert!(
            KnownHost::verified("server".into(), 22, "not a public key", &fingerprint).is_err()
        );
    }
}

#[cfg(test)]
mod serial_tests {
    use super::*;
    #[test]
    #[cfg(target_os = "macos")]
    fn macos_serial_paths_are_bounded_to_device_names() {
        let mut settings = SerialSettings::default();
        for path in ["/dev/cu.usbserial-123", "/dev/tty.usbmodem123"] {
            settings.port_name = path.into();
            settings.validate().unwrap();
        }
        for path in [
            "/dev/cu.",
            "/dev/tty.",
            "/dev/cu.a/../../file",
            "/tmp/cu.usbserial",
            "/dev/cu.a\n",
        ] {
            settings.port_name = path.into();
            assert!(settings.validate().is_err());
        }
    }
    #[test]
    fn old_profile_defaults_to_ssh_and_serial_roundtrips_without_ssh_fields() {
        let mut value = serde_json::to_value(Profile::default()).unwrap();
        value.as_object_mut().unwrap().remove("protocol");
        value.as_object_mut().unwrap().remove("serial");
        let old: Profile = serde_json::from_value(value).unwrap();
        assert_eq!(old.protocol, Protocol::Ssh);
        let mut profile = Profile {
            protocol: Protocol::Serial,
            ..Default::default()
        };
        profile.serial.port_name = "COM12".into();
        profile.serial.parity = SerialParity::Even;
        profile.serial.flow_control = SerialFlowControl::Hardware;
        profile.validate().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serial.json");
        let config = Config {
            profiles: vec![profile.clone()],
            ..Default::default()
        };
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.profiles[0].serial, profile.serial);
        assert_eq!(loaded.profiles[0].protocol, Protocol::Serial);
        assert!(!loaded.profiles[0].remember_secret);
    }
    #[test]
    fn serial_rejects_invalid_parameters_and_secrets() {
        let mut p = Profile {
            protocol: Protocol::Serial,
            ..Default::default()
        };
        for name in [
            "", "COM0", "COM01", "COM-1", "COM1 ", "COM65536", "C:\\file",
        ] {
            p.serial.port_name = name.into();
            assert!(p.validate().is_err(), "{name}");
        }
        p.serial.port_name = "com12".into();
        p.validate().unwrap();
        p.serial.baud_rate = 0;
        assert!(p.validate().is_err());
        p.serial.baud_rate = 115200;
        p.serial.data_bits = 9;
        assert!(p.validate().is_err());
        p.serial.data_bits = 8;
        p.serial.stop_bits = 3;
        assert!(p.validate().is_err());
        p.serial.stop_bits = 1;
        p.remember_secret = true;
        assert!(p.validate().is_err());
    }
}
