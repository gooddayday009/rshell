use crate::config::{AuthMethod, Config, KeepAlive, Profile, Trust};
#[cfg(any(windows, target_os = "macos"))]
use crate::user_keys::load as load_secret_key;
use anyhow::{Context, Result, bail};
#[cfg(not(any(windows, target_os = "macos")))]
use russh::keys::load_secret_key;
use russh::{
    ChannelMsg, Disconnect, client,
    keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use std::{
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::Zeroizing;

pub use crate::connection::{Command, Connection, Event, Terminal};

// Only workers own the transport. Finished tabs hold weak references.
pub(crate) struct SharedSession(
    pub(crate) Arc<client::Handle<Handler>>,
    KeepAlive,
    Arc<Mutex<tokio::time::Instant>>,
);

// Track actual encrypted transport traffic, including other channels and SSH keepalives.
struct ActivityStream {
    stream: tokio::net::TcpStream,
    activity: Arc<Mutex<tokio::time::Instant>>,
}
impl tokio::io::AsyncRead for ActivityStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = std::pin::Pin::new(&mut self.stream).poll_read(cx, buf);
        if buf.filled().len() > before {
            *self.activity.lock().unwrap() = tokio::time::Instant::now();
        }
        result
    }
}
impl tokio::io::AsyncWrite for ActivityStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = std::pin::Pin::new(&mut self.stream).poll_write(cx, buf);
        if matches!(result, std::task::Poll::Ready(Ok(n)) if n > 0) {
            *self.activity.lock().unwrap() = tokio::time::Instant::now();
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

async fn wait_for_idle(session: &SharedSession) {
    if !session.1.idle_enabled {
        std::future::pending::<()>().await;
    }
    let interval = Duration::from_secs(session.1.idle_interval_secs);
    loop {
        let deadline = *session.2.lock().unwrap() + interval;
        tokio::time::sleep_until(deadline).await;
        let mut last = session.2.lock().unwrap();
        if last.elapsed() >= interval {
            // Reserve this tick across channels sharing the same transport.
            *last = tokio::time::Instant::now();
            return;
        }
    }
}
impl Drop for SharedSession {
    fn drop(&mut self) {
        let session = self.0.clone();
        tokio::spawn(async move {
            let _ = session
                .disconnect(Disconnect::ByApplication, "RShell disconnected", "en")
                .await;
        });
    }
}
struct ChannelCleanup {
    writer: Arc<russh::ChannelWriteHalf<client::Msg>>,
    session: Arc<SharedSession>,
}
impl Drop for ChannelCleanup {
    fn drop(&mut self) {
        let writer = self.writer.clone();
        let session = self.session.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), writer.close()).await;
            drop(session);
        });
    }
}

pub fn duplicate_channel(
    runtime: &tokio::runtime::Handle,
    source: &Connection,
    terminal: impl Terminal,
) -> Result<Connection> {
    let session = source
        .ssh_session
        .lock()
        .unwrap()
        .upgrade()
        .context("SSH 连接已经结束，无法复制渠道")?;
    let (commands, rx) = mpsc::channel(64);
    let (tx, events) = mpsc::channel(16);
    let (cancel, mut cancelled) = watch::channel(false);
    let ssh_session = Arc::new(Mutex::new(Arc::downgrade(&session)));
    let published = ssh_session.clone();
    runtime.spawn(async move {
        let result = tokio::select! {
            biased;
            _ = cancelled.changed() => Ok(()),
            result = run_channel(session, terminal, rx, tx.clone()) => result,
        };
        *published.lock().unwrap() = Weak::new();
        let _ = tx
            .send(Event::Finished(result.map_err(|e| format!("{e:#}"))))
            .await;
    });
    Ok(Connection {
        commands,
        events,
        cancel,
        ssh_session,
    })
}

pub(crate) struct Handler {
    profile: Profile,
    config: Config,
    events: mpsc::Sender<Event>,
    forwarded: mpsc::Sender<crate::tunnel::Forwarded>,
}

impl client::Handler for Handler {
    type Error = anyhow::Error;

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _: &str,
        _: u32,
        reply: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<()> {
        // Reject when full/closed; never block SSH protocol processing on a TCP connection.
        let _ = self.forwarded.try_send(crate::tunnel::Forwarded {
            channel,
            address: connected_address.into(),
            port: connected_port,
            reply,
        });
        Ok(())
    }

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        if key.certificate().is_some() {
            bail!("首版暂不支持主机证书，请使用普通 SSH 主机密钥");
        }
        let public = key.public_key();
        let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
        match self
            .config
            .trust(&self.profile.host, self.profile.port, &fingerprint)
        {
            Trust::Trusted => Ok(true),
            Trust::Changed => bail!(
                "主机密钥已变化，已阻断连接。请通过可信渠道核实后再更新 known_hosts。新指纹：{fingerprint}"
            ),
            Trust::Unknown => {
                let (tx, rx) = oneshot::channel();
                self.events
                    .send(Event::HostKey {
                        fingerprint,
                        algorithm: public.algorithm().to_string(),
                        public_key: public.to_openssh()?,
                        reply: tx,
                    })
                    .await?;
                Ok(tokio::time::timeout(Duration::from_secs(90), rx)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false))
            }
        }
    }
}

pub fn connect(
    runtime: &tokio::runtime::Handle,
    profile: Profile,
    secret: String,
    config: Config,
    terminal: impl Terminal,
) -> Connection {
    let (commands, rx) = mpsc::channel(64);
    let (events_tx, events) = mpsc::channel(16);
    let (cancel, mut cancelled) = watch::channel(false);
    let ssh_session = Arc::new(Mutex::new(Weak::new()));
    let published = ssh_session.clone();
    runtime.spawn(async move {
        let result = tokio::select! {
            biased;
            _ = cancelled.changed() => Ok(()),
            result = run(profile, Zeroizing::new(secret), config, terminal, rx, events_tx.clone(), published.clone()) => result,
        };
        *published.lock().unwrap() = Weak::new();
        let _ = events_tx.send(Event::Finished(result.map_err(|e| format!("{e:#}")))).await;
    });
    Connection {
        commands,
        events,
        cancel,
        ssh_session,
    }
}

async fn run(
    profile: Profile,
    secret: Zeroizing<String>,
    config: Config,
    terminal: impl Terminal,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
    published: Arc<Mutex<Weak<SharedSession>>>,
) -> Result<()> {
    let (session, incoming) = authenticate(&profile, secret, config, events.clone()).await?;
    *published.lock().unwrap() = Arc::downgrade(&session);
    if !profile.tunnel_only && !profile.tunnels.iter().any(|r| r.enabled) {
        return run_channel(session, terminal, commands, events).await;
    }
    let mut tunnels =
        crate::tunnel::Tunnels::start(session.clone(), profile.tunnels, incoming, &events).await?;
    if profile.tunnel_only {
        events.send(Event::Connected).await?;
        tunnels.run(&events).await
    } else {
        tokio::select! {
            biased;
            result = run_channel(session, terminal, commands, events.clone()) => result,
            result = tunnels.run(&events) => result,
        }
    }
}

pub(crate) async fn authenticate(
    profile: &Profile,
    secret: Zeroizing<String>,
    config: Config,
    events: mpsc::Sender<Event>,
) -> Result<(Arc<SharedSession>, mpsc::Receiver<crate::tunnel::Forwarded>)> {
    profile.validate()?;
    events
        .send(Event::Status("连接中 / 校验主机身份".into()))
        .await?;
    let ssh_config = Arc::new(client::Config {
        keepalive_interval: profile
            .keep_alive
            .ssh_enabled
            .then(|| Duration::from_secs(profile.keep_alive.ssh_interval_secs)),
        keepalive_max: 3,
        channel_buffer_size: 32,
        ..Default::default()
    });
    let (forwarded, incoming) = mpsc::channel(64);
    let handler = Handler {
        profile: profile.clone(),
        config,
        events: events.clone(),
        forwarded,
    };
    let stream = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::net::TcpStream::connect((profile.host.as_str(), profile.port)),
    )
    .await
    .context("TCP 连接超时")?
    .context("TCP / DNS 连接失败")?;
    socket2::SockRef::from(&stream)
        .set_keepalive(profile.keep_alive.tcp_enabled)
        .context("设置 TCP 保持活动失败")?;
    let activity = Arc::new(Mutex::new(tokio::time::Instant::now()));
    let stream = ActivityStream {
        stream,
        activity: activity.clone(),
    };
    let mut session = tokio::time::timeout(
        Duration::from_secs(110),
        client::connect_stream(ssh_config, stream, handler),
    )
    .await
    .context("SSH 握手或指纹确认超时")??;
    events.send(Event::Status("认证中".into())).await?;
    let auth = tokio::time::timeout(Duration::from_secs(30), async {
        match profile.auth {
            AuthMethod::Password => session
                .authenticate_password(&profile.username, secret.as_str())
                .await
                .map_err(anyhow::Error::from),
            AuthMethod::PrivateKey => {
                let key_path = profile.key_path.clone();
                let key = tokio::task::spawn_blocking(move || {
                    load_secret_key(
                        key_path,
                        if secret.is_empty() {
                            None
                        } else {
                            Some(secret.as_str())
                        },
                    )
                })
                .await?
                .context("读取或解密私钥失败")?;
                let hash = session.best_supported_rsa_hash().await?.flatten();
                Ok(session
                    .authenticate_publickey(
                        &profile.username,
                        PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                    )
                    .await?)
            }
        }
    })
    .await
    .context("认证超时")??;
    if !auth.success() {
        bail!("认证失败：请检查用户名、密码或私钥（首版不支持 MFA）");
    }

    let session = Arc::new(SharedSession(
        Arc::new(session),
        profile.keep_alive.clone(),
        activity,
    ));
    Ok((session, incoming))
}

async fn run_channel(
    session: Arc<SharedSession>,
    mut terminal: impl Terminal,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
) -> Result<()> {
    let channel =
        tokio::time::timeout(Duration::from_secs(15), session.0.channel_open_session()).await??;
    let (mut reader, writer) = channel.split();
    let writer = Arc::new(writer);
    let _cleanup = ChannelCleanup {
        writer: writer.clone(),
        session: session.clone(),
    };
    writer
        .request_pty(true, "xterm-256color", 80, 24, 0, 0, &[])
        .await?;
    await_success(&mut reader).await.context("服务器拒绝 PTY")?;
    writer.request_shell(true).await?;
    await_success(&mut reader)
        .await
        .context("服务器拒绝交互 shell")?;
    events.send(Event::Connected).await?;
    let (write_tx, mut write_rx) = mpsc::channel::<Command>(64);
    let writing = async {
        while let Some(cmd) = write_rx.recv().await {
            match cmd {
                Command::Input(bytes) => writer.data_bytes(bytes).await?,
                Command::Resize { cols, rows } => {
                    writer.window_change(cols as u32, rows as u32, 0, 0).await?
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::pin!(writing);
    let mut detector = crate::zmodem::Detector::default();
    let detection_timeout = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(detection_timeout);
    let mut detection_timer_armed = false;
    let mut remote_exited = false;
    async {
        loop {
            tokio::select! {
                result = &mut writing => { result?; break; }
                _ = &mut detection_timeout, if detection_timer_armed => {
                    let replies = terminal.output(&detector.flush());
                    if !replies.is_empty() { write_tx.try_send(Command::Input(replies)).context("终端回复队列已满")?; }
                }
                _ = wait_for_idle(&session) => {
                    write_tx.try_send(Command::Input(session.1.idle_text.as_bytes().to_vec()))
                        .context("保持活动发送队列已满")?;
                }
                cmd = commands.recv() => {
                    match cmd {
                        Some(Command::Resize { cols, rows }) => {
                            terminal.resize(cols, rows);
                            write_tx.try_send(Command::Resize { cols, rows }).context("发送队列已满，连接已停止以避免尺寸不同步")?;
                        }
                        Some(cmd) => write_tx.try_send(cmd).context("发送队列已满，连接已停止以避免输入丢失")?,
                        None => break,
                    }
                }
                msg = reader.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                            let (plain, transfer) = detector.feed(&data);
                            for chunk in plain.chunks(8192) {
                                let replies = terminal.output(chunk);
                                if !replies.is_empty() { write_tx.try_send(Command::Input(replies)).context("终端回复队列已满")?; }
                                tokio::task::yield_now().await;
                            }
                            if let Some((upload, initial)) = transfer {
                                let result = crate::zmodem::run(upload, initial, &mut reader, &writer, &mut commands, &events, &mut terminal).await;
                                if let Ok(tail) = &result {
                                    let replies = terminal.output(tail);
                                    if !replies.is_empty() { writer.data_bytes(replies).await?; }
                                }
                                if let Err(error) = result {
                                    writer.data_bytes([vec![0x18; 10], vec![8; 10]].concat()).await?;
                                    let _ = events.try_send(Event::Status(format!("ZMODEM：{error:#}")));
                                }
                            }
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            remote_exited = true;
                            let _ = events.try_send(Event::Status(format!("远程进程退出：{exit_status}")));
                        }
                        Some(ChannelMsg::ExitSignal { .. }) => remote_exited = true,
                        Some(ChannelMsg::Close) => break,
                        None if remote_exited => break,
                        None => bail!("SSH 传输意外中断"),
                        _ => {}
                    }
                }
            }
            if detector.has_pending() {
                if !detection_timer_armed {
                    detection_timeout.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(2));
                    detection_timer_armed = true;
                }
            } else {
                detection_timer_armed = false;
            }
        }
        Ok(())
    }.await
}

async fn await_success(channel: &mut russh::ChannelReadHalf) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if request_reply(channel.wait().await)? {
                return Ok(());
            }
        }
    })
    .await
    .context("SSH channel 请求超时")?
}

fn request_reply(message: Option<ChannelMsg>) -> Result<bool> {
    match message {
        Some(ChannelMsg::Success) => Ok(true),
        // OpenSSH can increase its channel window before acknowledging the shell.
        // This is flow control, not the reply to our PTY/shell request.
        Some(ChannelMsg::WindowAdjusted { .. }) => Ok(false),
        Some(ChannelMsg::Failure) => bail!("SSH channel 请求被服务器拒绝"),
        Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
            bail!("等待 SSH channel 请求响应时连接关闭")
        }
        _ => bail!("SSH channel 请求未成功：收到非预期响应"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_adjustment_is_not_a_shell_rejection() {
        for new_size in [0, 2097152] {
            assert!(!request_reply(Some(ChannelMsg::WindowAdjusted { new_size })).unwrap());
        }
        assert!(request_reply(Some(ChannelMsg::Success)).unwrap());
    }

    #[test]
    fn failed_or_closed_requests_still_fail() {
        for message in [
            Some(ChannelMsg::Failure),
            Some(ChannelMsg::Close),
            Some(ChannelMsg::Eof),
            None,
        ] {
            assert!(request_reply(message).is_err());
        }
    }
}
