//! Local-only control channel for the bundled Rust shell. Terminal output is never control data.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshTarget {
    pub host: String,
    pub username: String,
    pub port: u16,
}

impl SshTarget {
    pub fn parse(args: &[String]) -> Result<Self> {
        let mut target = None;
        let mut username = String::new();
        let mut port = 22;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-p" => port = args.next().context("ssh -p 缺少端口")?.parse()?,
                "-l" => username = args.next().context("ssh -l 缺少用户名")?.clone(),
                value if value.starts_with('-') => {
                    bail!("暂不支持 ssh 选项 {value}；支持 ssh [-p port] [-l user] [user@]host")
                }
                value => {
                    if target.replace(value.to_owned()).is_some() {
                        bail!("内置 ssh 目前只支持交互连接，不接受远端命令");
                    }
                }
            }
        }
        let target = target.context("用法：ssh [-p port] [-l user] [user@]host")?;
        let host = if let Some((user, host)) = target.rsplit_once('@') {
            if !username.is_empty() && username != user {
                bail!("ssh 用户名冲突");
            }
            username = user.to_owned();
            host.to_owned()
        } else {
            target
        };
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&host)
            .to_owned();
        let value = Self {
            host,
            username,
            port,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<()> {
        if self.port == 0
            || self.host.is_empty()
            || self.host.len() > 255
            || self.username.len() > 255
            || self
                .host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '@'))
            || self
                .username
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            bail!("无效的 SSH 主机、用户名或端口");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    target: SshTarget,
}

pub struct Bridge {
    listener: TcpListener,
    token: String,
}
impl Bridge {
    pub fn new() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            token: uuid::Uuid::new_v4().to_string(),
        })
    }
    pub fn configure(&self, command: &mut portable_pty::CommandBuilder) -> Result<()> {
        command.env(
            "RSHELL_SSH_BRIDGE_ADDR",
            self.listener.local_addr()?.to_string(),
        );
        command.env("RSHELL_SSH_BRIDGE_TOKEN", &self.token);
        Ok(())
    }
    pub fn poll(&self) -> Option<SshTarget> {
        let (mut stream, peer) = self.listener.accept().ok()?;
        if !peer.ip().is_loopback() {
            return None;
        }
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok()?;
        let mut length = [0; 4];
        stream.read_exact(&mut length).ok()?;
        let length = u32::from_le_bytes(length) as usize;
        if length > 2048 {
            return None;
        }
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).ok()?;
        let request: Request = serde_json::from_slice(&bytes).ok()?;
        if request.token != self.token || request.target.validate().is_err() {
            return None;
        }
        Some(request.target)
    }
}

pub fn request_ssh(target: SshTarget) -> Result<()> {
    target.validate()?;
    let address: std::net::SocketAddr = std::env::var("RSHELL_SSH_BRIDGE_ADDR")
        .context("ssh 交互连接需要从 RShell 标签启动内置 Shell")?
        .parse()?;
    if !address.ip().is_loopback() {
        bail!("SSH 控制通道必须位于本机");
    }
    let token = std::env::var("RSHELL_SSH_BRIDGE_TOKEN")?;
    let bytes = serde_json::to_vec(&Request { token, target })?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssh_arguments_are_structured_and_never_accept_passwords_or_remote_commands() {
        let parse = |s: &str| {
            SshTarget::parse(&s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
        };
        assert_eq!(
            parse("-p 2222 user@host").unwrap(),
            SshTarget {
                host: "host".into(),
                username: "user".into(),
                port: 2222
            }
        );
        assert_eq!(parse("-l root [::1]").unwrap().host, "::1");
        for bad in [
            "-p 0 host",
            "-p 99999 host",
            "host rm",
            "-password secret host",
            "-l a b@host",
            "",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn control_bridge_rejects_wrong_token_and_accepts_authenticated_request() {
        let bridge = Bridge::new().unwrap();
        for valid in [false, true] {
            let bytes = serde_json::to_vec(&Request {
                token: if valid {
                    bridge.token.clone()
                } else {
                    "wrong".into()
                },
                target: SshTarget {
                    host: "localhost".into(),
                    username: "test".into(),
                    port: 2222,
                },
            })
            .unwrap();
            let mut client = TcpStream::connect(bridge.listener.local_addr().unwrap()).unwrap();
            client
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .unwrap();
            client.write_all(&bytes).unwrap();
            assert_eq!(bridge.poll().is_some(), valid);
        }
    }
}
