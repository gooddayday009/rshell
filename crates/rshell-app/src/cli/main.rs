mod options;

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use options::{Action, Options};
use rshell_core::{
    config::{AuthMethod, Config, Profile, Protocol},
    headless::Session,
    transfer::{Outcome, Request},
};
use std::{io::IsTerminal, path::PathBuf, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

fn main() {
    let options = match Options::parse(std::env::args_os().skip(1).collect()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("RShell CLI: {error:#}\n使用 rshell-cli --help 查看用法。");
            std::process::exit(2);
        }
    };
    match options.action {
        Action::Help => {
            println!("{}", options::HELP);
            return;
        }
        Action::Version => {
            println!("rshell-cli {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("RShell CLI: {error}");
            std::process::exit(255);
        }
    };
    let code = runtime.block_on(async {
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(options.timeout), run(options)) => {
                match result {
                    Ok(Ok(code)) => code,
                    Ok(Err(error)) => { eprintln!("RShell CLI: {error:#}"); 255 }
                    Err(_) => { eprintln!("RShell CLI: 操作超时；远端命令可能仍在运行，传输临时文件可能保留"); 124 }
                }
            }
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result { eprintln!("RShell CLI: 无法监听 Ctrl+C：{error}"); return 255; }
                eprintln!("RShell CLI: 已取消；远端命令可能仍在运行，传输临时文件可能保留");
                130
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(3));
    std::process::exit(code as i32);
}

async fn run(options: Options) -> Result<u32> {
    let directory = options
        .config_dir
        .clone()
        .or_else(|| std::env::var_os("RSHELL_CONFIG_DIR").map(PathBuf::from))
        .or_else(|| {
            ProjectDirs::from("com", "RShell", "RShell").map(|dirs| dirs.config_dir().to_path_buf())
        })
        .context("找不到配置目录")?;
    let path = directory.join("config.json");
    let load_path = path.clone();
    let config = tokio::task::spawn_blocking(move || Config::load(&load_path)).await??;
    if !config.cli_enabled {
        bail!(
            "CLI 默认关闭。请在 RShell 设置 → 会话 中启用“启用命令行（CLI）”；未读取凭据、未连接服务器"
        );
    }
    let profile = options.profile(&config)?;
    let secret = if options.password_stdin {
        if std::io::stdin().is_terminal() {
            bail!("--password-stdin 需要重定向或管道输入，不能在可回显的终端输入密码");
        }
        read_secret(tokio::io::stdin()).await?
    } else {
        saved_secret(directory, profile.clone()).await?
    };
    let session = Session::connect(profile, secret, path).await?;
    let result = async {
        match options.action {
            Action::Exec(command) => {
                session
                    .exec(&command, &mut tokio::io::stdout(), &mut tokio::io::stderr())
                    .await
            }
            Action::List(remote) => {
                let Outcome::Listed { path, entries } = session.sftp(Request::List(remote)).await?
                else {
                    unreachable!()
                };
                let text = if options.json {
                    let entries: Vec<_> = entries.into_iter().map(|entry| serde_json::json!({
                        "name": entry.name, "directory": entry.directory, "size": entry.size,
                    })).collect();
                    serde_json::to_string(&serde_json::json!({"path": path, "entries": entries}))?
                        + "\n"
                } else {
                    let mut text = String::from("TYPE\tSIZE\tNAME\n");
                    for entry in entries {
                        // Escape untrusted names in human-readable output, including ANSI controls.
                        let name: String = entry
                            .name
                            .chars()
                            .flat_map(|c| {
                                if c.is_control() {
                                    c.escape_default().collect::<Vec<_>>()
                                } else {
                                    vec![c]
                                }
                            })
                            .collect();
                        text.push_str(&format!(
                            "{}\t{}\t{name}\n",
                            if entry.directory { "dir" } else { "file" },
                            entry.size
                        ));
                    }
                    text
                };
                let mut stdout = tokio::io::stdout();
                stdout.write_all(text.as_bytes()).await?;
                stdout.flush().await?;
                Ok(0)
            }
            Action::Upload { local, remote } => {
                session.sftp(Request::Upload { local, remote }).await?;
                Ok(0)
            }
            Action::Download { remote, local } => {
                session.sftp(Request::Download { remote, local }).await?;
                Ok(0)
            }
            Action::Sync {
                local,
                remote,
                upload,
            } => {
                let report = session
                    .sync_directory(rshell_core::sftp_sync::Request {
                        local,
                        remote,
                        upload,
                        dry_run: options.dry_run,
                        delete: options.delete,
                    })
                    .await?;
                let text = if options.json {
                    serde_json::to_string(&report)? + "\n"
                } else {
                    let mut text = format!(
                        "{}\t{}\tdelete={}\nACTION\tBYTES\tRELATIVE_PATH\n",
                        if report.dry_run { "PLAN" } else { "DONE" },
                        if report.upload { "upload" } else { "download" },
                        report.delete
                    );
                    for op in report.operations {
                        text.push_str(&format!(
                            "{:?}\t{}\t{:?}\n",
                            op.kind,
                            op.bytes,
                            if op.path.is_empty() { "." } else { &op.path }
                        ));
                    }
                    text
                };
                let mut stdout = tokio::io::stdout();
                stdout.write_all(text.as_bytes()).await?;
                stdout.flush().await?;
                Ok(0)
            }
            Action::Help | Action::Version => unreachable!(),
        }
    }
    .await;
    session.disconnect().await;
    result
}

async fn saved_secret(directory: PathBuf, profile: Profile) -> Result<Zeroizing<String>> {
    #[cfg(windows)]
    if profile.remember_secret {
        use rshell_core::{credentials::EncryptedCredentials, store::Credentials};
        let id = profile.id;
        if let Some(secret) = tokio::task::spawn_blocking(move || {
            EncryptedCredentials(directory.join("credentials")).read(id)
        })
        .await??
        {
            return Ok(secret);
        }
    }
    #[cfg(not(windows))]
    let _ = directory;
    if profile.auth == AuthMethod::PrivateKey {
        return Ok(Zeroizing::new(String::new()));
    }
    bail!("没有可用的已保存密码，请通过 --password-stdin 提供密码")
}

async fn read_secret(input: impl tokio::io::AsyncRead + Unpin) -> Result<Zeroizing<String>> {
    let mut bytes = Zeroizing::new(Vec::new());
    // A bounded first line, preserving spaces and supporting Windows CRLF.
    let count = tokio::io::BufReader::new(input)
        .take(16387)
        .read_until(b'\n', &mut bytes)
        .await?;
    if count == 0 {
        bail!("标准输入未提供密码或私钥口令");
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > 16384 || bytes.contains(&0) {
        bail!("密码或口令过长或包含 NUL");
    }
    Ok(Zeroizing::new(
        std::str::from_utf8(&bytes)
            .context("密码或口令必须为 UTF-8")?
            .to_owned(),
    ))
}

impl Options {
    fn profile(&self, config: &Config) -> Result<Profile> {
        let profile = if let Some(selector) = &self.profile {
            let by_id = uuid::Uuid::parse_str(selector)
                .ok()
                .and_then(|id| config.profiles.iter().find(|profile| profile.id == id));
            if let Some(profile) = by_id {
                profile.clone()
            } else {
                let matches: Vec<_> = config
                    .profiles
                    .iter()
                    .filter(|profile| profile.name == *selector)
                    .collect();
                match matches.as_slice() {
                    [profile] => (*profile).clone(),
                    [] => bail!("找不到已保存会话；--profile 使用完整名称或 UUID"),
                    _ => bail!("会话名称重复，请使用 UUID 指定连接"),
                }
            }
        } else {
            Profile {
                name: "CLI".into(),
                host: self.host.clone().context("缺少 --host")?,
                username: self.user.clone().context("缺少 --user")?,
                port: self.port.unwrap_or(22),
                auth: if self.identity.is_some() {
                    AuthMethod::PrivateKey
                } else {
                    AuthMethod::Password
                },
                key_path: self.identity.clone().unwrap_or_default(),
                ..Default::default()
            }
        };
        if profile.protocol != Protocol::Ssh {
            bail!("CLI 仅支持 SSH 会话");
        }
        profile.validate()?;
        Ok(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn secret_is_bounded_and_preserves_spaces_without_echoing() {
        assert_eq!(
            &*read_secret(&b"  test only  \r\nignored"[..]).await.unwrap(),
            "  test only  "
        );
        assert!(read_secret(&b""[..]).await.is_err());
        assert!(read_secret(&vec![b'x'; 16385][..]).await.is_err());
        assert!(read_secret(&b"bad\0secret"[..]).await.is_err());
    }
    #[test]
    fn ambiguous_profile_names_require_uuid_and_non_ssh_is_rejected() {
        let first = Profile {
            name: "demo".into(),
            host: "127.0.0.1".into(),
            ..Default::default()
        };
        let second = Profile {
            id: uuid::Uuid::new_v4(),
            ..first.clone()
        };
        let mut config = Config {
            profiles: vec![first.clone(), second],
            ..Default::default()
        };
        let mut options = Options::parse(
            ["exec", "--profile", "demo", "--", "whoami"]
                .map(Into::into)
                .to_vec(),
        )
        .unwrap();
        assert!(options.profile(&config).is_err());
        options.profile = Some(first.id.to_string());
        assert_eq!(options.profile(&config).unwrap().id, first.id);
        config.profiles[0].protocol = Protocol::Telnet;
        assert!(options.profile(&config).is_err());
    }
}
