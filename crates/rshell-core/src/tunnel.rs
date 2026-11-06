//! TCP forwarding owned by one SSH connection; no shell commands or local subprocesses.
use crate::ssh::{Event, SharedSession};
use anyhow::{Context, Result, bail, ensure};
use russh::{Channel, client};
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
};

pub const MAX_RULES: usize = 16;
const MAX_CONNECTIONS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Local,
    Remote,
    Dynamic,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "本地转发",
            Self::Remote => "远程转发",
            Self::Dynamic => "动态 SOCKS5",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub enabled: bool,
    pub kind: Kind,
    pub bind_host: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            enabled: true,
            kind: Kind::Local,
            bind_host: "127.0.0.1".into(),
            bind_port: 8080,
            target_host: "127.0.0.1".into(),
            target_port: 80,
        }
    }
}

impl Rule {
    pub fn description(&self) -> String {
        let listen = format!("[{}]:{}", self.bind_host, self.bind_port);
        match self.kind {
            Kind::Local => format!(
                "本机 {listen} → SSH 服务端访问 {}:{}",
                self.target_host, self.target_port
            ),
            Kind::Remote => format!(
                "SSH 服务端 {listen} → 本机访问 {}:{}",
                self.target_host, self.target_port
            ),
            Kind::Dynamic => format!("本机 {listen} · SOCKS5 TCP / 远端 DNS"),
        }
    }
}

pub fn validate(rules: &[Rule], tunnel_only: bool) -> Result<()> {
    ensure!(
        rules.len() <= MAX_RULES,
        "每个会话最多保存 {MAX_RULES} 条隧道规则"
    );
    ensure!(
        !tunnel_only || rules.iter().any(|r| r.enabled),
        "仅隧道模式至少需要一条启用的规则"
    );
    for (index, rule) in rules.iter().enumerate().filter(|(_, r)| r.enabled) {
        let ip: IpAddr = rule
            .bind_host
            .parse()
            .context("隧道监听地址必须为 IPv4 或 IPv6 地址（例如 127.0.0.1 或 ::1）")?;
        ensure!(rule.bind_port != 0, "隧道监听端口必须在 1–65535 之间");
        if rule.kind == Kind::Dynamic {
            ensure!(ip.is_loopback(), "无认证 SOCKS5 代理只能监听回环地址");
        } else {
            ensure!(
                valid_host(&rule.target_host) && rule.target_port != 0,
                "隧道目标主机无效或目标端口为 0"
            );
        }
        for other in rules[..index].iter().filter(|r| r.enabled) {
            if (rule.kind == Kind::Remote) == (other.kind == Kind::Remote)
                && rule.bind_port == other.bind_port
            {
                let other_ip: IpAddr = other.bind_host.parse()?;
                ensure!(
                    ip != other_ip && !ip.is_unspecified() && !other_ip.is_unspecified(),
                    "隧道监听地址和端口存在冲突"
                );
            }
        }
    }
    Ok(())
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub(crate) struct Forwarded {
    pub channel: Channel<client::Msg>,
    pub address: String,
    pub port: u32,
    pub reply: client::ChannelOpenHandle,
}

pub(crate) struct Tunnels {
    session: Arc<SharedSession>,
    rules: Vec<Rule>,
    remote_requests: Vec<(String, u16)>,
    listeners: JoinSet<Result<()>>,
    streams: JoinSet<(usize, Result<()>)>,
    accepted: mpsc::Receiver<(usize, TcpStream)>,
    forwarded: mpsc::Receiver<Forwarded>,
    statuses: Vec<String>,
}

impl Drop for Tunnels {
    fn drop(&mut self) {
        // Abort listeners and active copies even if authentication/channel workers are cancelled.
        self.listeners.abort_all();
        self.streams.abort_all();
        let requests = std::mem::take(&mut self.remote_requests);
        if !requests.is_empty() {
            let session = self.session.clone();
            tokio::spawn(async move {
                for (host, port) in requests {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        session.0.cancel_tcpip_forward(host, u32::from(port)),
                    )
                    .await;
                }
            });
        }
    }
}

impl Tunnels {
    pub(crate) async fn start(
        session: Arc<SharedSession>,
        rules: Vec<Rule>,
        forwarded: mpsc::Receiver<Forwarded>,
        events: &mpsc::Sender<Event>,
    ) -> Result<Self> {
        let (tx, accepted) = mpsc::channel(MAX_CONNECTIONS);
        let mut tunnels = Self {
            session,
            rules,
            remote_requests: Vec::new(),
            listeners: JoinSet::new(),
            streams: JoinSet::new(),
            accepted,
            forwarded,
            statuses: Vec::new(),
        };
        for (index, rule) in tunnels.rules.iter().enumerate() {
            if !rule.enabled {
                tunnels.statuses.push("已禁用".into());
                continue;
            }
            let context = || format!("隧道 {} 启动失败：{}", index + 1, rule.description());
            if rule.kind == Kind::Remote {
                // Register before awaiting so a cancelled or timed-out request is also cleaned up.
                tunnels
                    .remote_requests
                    .push((rule.bind_host.clone(), rule.bind_port));
                tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    tunnels
                        .session
                        .0
                        .tcpip_forward(&rule.bind_host, u32::from(rule.bind_port)),
                )
                .await
                .context("远程转发请求超时")?
                .with_context(context)?;
            } else {
                let ip: IpAddr = rule.bind_host.parse()?;
                let listener = TcpListener::bind((ip, rule.bind_port))
                    .await
                    .with_context(context)?;
                let tx = tx.clone();
                tunnels.listeners.spawn(async move {
                    loop {
                        let (socket, _) = listener.accept().await?;
                        // Overload closes the new socket instead of growing an unbounded queue.
                        let _ = tx.try_send((index, socket));
                    }
                });
            }
            tunnels.statuses.push("监听中".into());
        }
        if !tunnels.rules.is_empty() {
            events
                .send(Event::Tunnels(tunnels.statuses.clone()))
                .await?;
        }
        Ok(tunnels)
    }

    pub(crate) async fn run(&mut self, events: &mpsc::Sender<Event>) -> Result<()> {
        let mut heartbeat = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = heartbeat.tick() => ensure!(!self.session.0.is_closed(), "SSH 隧道连接已中断"),
                Some(result) = self.listeners.join_next(), if !self.listeners.is_empty() => {
                    result??;
                    bail!("隧道监听任务意外结束");
                }
                Some(result) = self.streams.join_next(), if !self.streams.is_empty() => {
                    let (index, result) = result?;
                    if let Err(error) = result {
                        let error: String = format!("{error:#}").chars().take(240).collect();
                        self.statuses[index] = format!("监听中 · 最近连接失败：{error}");
                        events.send(Event::Tunnels(self.statuses.clone())).await?;
                    }
                }
                Some((index, socket)) = self.accepted.recv(), if !self.listeners.is_empty() => {
                    if self.streams.len() >= MAX_CONNECTIONS { continue; }
                    let session = self.session.clone();
                    let rule = self.rules[index].clone();
                    self.streams.spawn(async move { (index, forward_local(session, socket, rule).await) });
                }
                Some(incoming) = self.forwarded.recv() => {
                    let index = self.rules.iter().position(|r| r.enabled && r.kind == Kind::Remote
                        && r.bind_host == incoming.address && u32::from(r.bind_port) == incoming.port);
                    // Never let unsolicited remote channels choose a local target.
                    if let Some(index) = index.filter(|_| self.streams.len() < MAX_CONNECTIONS) {
                        let rule = self.rules[index].clone();
                        self.streams.spawn(async move { (index, forward_remote(incoming, rule).await) });
                    }
                }
            }
        }
    }
}

async fn forward_remote(incoming: Forwarded, rule: Rule) -> Result<()> {
    let mut socket = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((rule.target_host.as_str(), rule.target_port)),
    )
    .await
    .context("本机连接目标超时")?
    .context("本机无法连接转发目标")?;
    incoming.reply.accept().await;
    let mut stream = incoming.channel.into_stream();
    tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
    Ok(())
}

async fn forward_local(
    session: Arc<SharedSession>,
    mut socket: TcpStream,
    rule: Rule,
) -> Result<()> {
    let dynamic = rule.kind == Kind::Dynamic;
    let (host, port) = if dynamic {
        tokio::time::timeout(CONNECT_TIMEOUT, socks_target(&mut socket))
            .await
            .context("SOCKS5 握手超时")??
    } else {
        (rule.target_host, rule.target_port)
    };
    let peer = socket.peer_addr()?;
    let channel = tokio::time::timeout(
        CONNECT_TIMEOUT,
        session.0.channel_open_direct_tcpip(
            host,
            u32::from(port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        ),
    )
    .await;
    let channel = match channel {
        Ok(Ok(channel)) => channel,
        result => {
            if dynamic {
                let _ = socks_reply(&mut socket, 1).await;
            }
            return match result {
                Ok(Err(error)) => Err(error).context("SSH 服务端拒绝或无法连接转发目标"),
                Err(error) => Err(error).context("SSH 转发目标连接超时"),
                _ => unreachable!(),
            };
        }
    };
    let mut stream = channel.into_stream();
    if dynamic {
        socks_reply(&mut socket, 0).await?;
    }
    tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
    Ok(())
}

async fn socks_reply(socket: &mut TcpStream, code: u8) -> Result<()> {
    socket.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    Ok(())
}

async fn socks_target(socket: &mut TcpStream) -> Result<(String, u16)> {
    ensure!(socket.read_u8().await? == 5, "仅支持 SOCKS5");
    let count = socket.read_u8().await?;
    let mut methods = vec![0; usize::from(count)];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        socket.write_all(&[5, 255]).await?;
        bail!("SOCKS5 客户端未提供无认证方式");
    }
    socket.write_all(&[5, 0]).await?;
    let mut header = [0; 4];
    socket.read_exact(&mut header).await?;
    if header[0] != 5 || header[2] != 0 {
        socks_reply(socket, 1).await?;
        bail!("SOCKS5 请求头无效");
    }
    if header[1] != 1 {
        socks_reply(socket, 7).await?;
        bail!("SOCKS5 仅支持 TCP CONNECT");
    }
    let host = match header[3] {
        1 => {
            let mut bytes = [0; 4];
            socket.read_exact(&mut bytes).await?;
            std::net::Ipv4Addr::from(bytes).to_string()
        }
        4 => {
            let mut bytes = [0; 16];
            socket.read_exact(&mut bytes).await?;
            std::net::Ipv6Addr::from(bytes).to_string()
        }
        3 => {
            let len = socket.read_u8().await?;
            let mut bytes = vec![0; usize::from(len)];
            socket.read_exact(&mut bytes).await?;
            let host = String::from_utf8(bytes)?;
            if !valid_host(&host) || !host.is_ascii() {
                socks_reply(socket, 8).await?;
                bail!("SOCKS5 域名无效，请使用 ASCII / Punycode");
            }
            host
        }
        _ => {
            socks_reply(socket, 8).await?;
            bail!("SOCKS5 地址类型不支持");
        }
    };
    let port = socket.read_u16().await?;
    if port == 0 {
        socks_reply(socket, 1).await?;
        bail!("SOCKS5 目标端口不能为 0");
    }
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_bounds_and_exposure() {
        assert!(validate(&[], false).is_ok());
        assert!(validate(&[], true).is_err());
        let rule = Rule::default();
        assert!(validate(std::slice::from_ref(&rule), true).is_ok());
        assert!(validate(&vec![rule.clone(); MAX_RULES + 1], false).is_err());
        for invalid in [
            Rule {
                bind_host: "localhost".into(),
                ..rule.clone()
            },
            Rule {
                bind_port: 0,
                ..rule.clone()
            },
            Rule {
                target_host: "host\nother".into(),
                ..rule.clone()
            },
            Rule {
                target_port: 0,
                ..rule.clone()
            },
            Rule {
                kind: Kind::Dynamic,
                bind_host: "0.0.0.0".into(),
                ..rule.clone()
            },
        ] {
            assert!(validate(&[invalid], false).is_err());
        }
        assert!(
            validate(
                &[Rule {
                    kind: Kind::Dynamic,
                    bind_host: "::1".into(),
                    ..rule.clone()
                }],
                true
            )
            .is_ok()
        );
        assert!(validate(&[rule.clone(), rule.clone()], false).is_err());
        // Local and remote listeners are in distinct network namespaces.
        assert!(
            validate(
                &[
                    rule.clone(),
                    Rule {
                        kind: Kind::Remote,
                        ..rule.clone()
                    }
                ],
                false
            )
            .is_ok()
        );
        assert!(
            validate(
                &[
                    rule.clone(),
                    Rule {
                        enabled: false,
                        ..rule.clone()
                    }
                ],
                false
            )
            .is_ok()
        );
        assert!(
            validate(
                &[
                    rule.clone(),
                    Rule {
                        kind: Kind::Dynamic,
                        ..rule
                    }
                ],
                false
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_profiles_default_to_no_tunnels_and_rules_roundtrip() {
        let profile = crate::config::Profile::default();
        let mut json = serde_json::to_value(&profile).unwrap();
        json.as_object_mut().unwrap().remove("tunnels");
        json.as_object_mut().unwrap().remove("tunnel_only");
        let mut restored: crate::config::Profile = serde_json::from_value(json).unwrap();
        assert!(restored.tunnels.is_empty() && !restored.tunnel_only);
        restored.host = "server.test".into();
        restored.tunnels = vec![Rule::default()];
        restored.tunnel_only = true;
        let encoded = serde_json::to_string(&restored).unwrap();
        let decoded: crate::config::Profile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.tunnels, restored.tunnels);
        assert!(decoded.tunnel_only);
        let mut invalid = decoded;
        invalid.protocol = crate::config::Protocol::Telnet;
        assert!(invalid.validate().is_err());
    }
}
