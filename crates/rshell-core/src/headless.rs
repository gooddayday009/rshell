//! Non-interactive SSH operations. No PTY, shell input, tunnels or terminal parsing.
use crate::{
    config::{Config, KnownHost, Profile, Protocol, Trust},
    connection::Event,
    ssh::{self, SharedSession},
    transfer::{self, Outcome, Request},
};
use anyhow::{Context, Result, bail};
use russh::ChannelMsg;
use std::{fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use zeroize::Zeroizing;

pub struct Session(Arc<SharedSession>);

impl Session {
    /// Known hosts are read from the same config as the GUI. Only first trust writes it.
    pub async fn connect(
        mut profile: Profile,
        secret: Zeroizing<String>,
        config_path: PathBuf,
    ) -> Result<Self> {
        if profile.protocol != Protocol::Ssh {
            bail!("CLI 仅支持 SSH 会话");
        }
        // Saved interactive settings must not open listeners or send idle shell text.
        profile.tunnels.clear();
        profile.tunnel_only = false;
        profile.keep_alive.idle_enabled = false;
        let path = config_path.clone();
        let config = tokio::task::spawn_blocking(move || Config::load(&path)).await??;
        let (tx, mut events) = mpsc::channel(16);
        let connecting = ssh::authenticate(&profile, secret, config, tx);
        tokio::pin!(connecting);
        loop {
            tokio::select! {
                result = &mut connecting => {
                    let (session, incoming) = result?;
                    drop(incoming); // Unsolicited forwarding channels are rejected.
                    return Ok(Self(session));
                }
                Some(event) = events.recv() => {
                    if let Event::HostKey { fingerprint, public_key, reply, .. } = event {
                        let path = config_path.clone();
                        let host = KnownHost::verified(profile.host.clone(), profile.port, &public_key, &fingerprint)?;
                        tokio::task::spawn_blocking(move || -> Result<()> {
                            let dir = path.parent().context("配置目录无效")?;
                            fs::create_dir_all(dir)?;
                            let lock = fs::OpenOptions::new().create(true).truncate(false).write(true)
                                .open(dir.join("instance.lock"))?;
                            lock.try_lock().context("首次保存主机密钥需要配置写锁，请关闭使用同一配置目录的 RShell 后重试")?;
                            if reply.is_closed() { bail!("连接已取消，未保存主机密钥"); }
                            // Re-read under the GUI's lock; never overwrite concurrent config edits.
                            let before = Config::load(&path)?;
                            let mut config = before.clone();
                            match config.trust(&host.host, host.port, &host.fingerprint) {
                                Trust::Changed => bail!("主机密钥已变化，已阻断连接"),
                                Trust::Unknown => config.known_hosts.push(host),
                                Trust::Trusted => { let _ = reply.send(true); return Ok(()); }
                            }
                            config.save(&path)?;
                            if reply.send(true).is_err() {
                                before.save(&path).context("连接取消后撤销主机信任失败")?;
                                bail!("连接已取消，主机信任已撤销");
                            }
                            Ok(())
                        }).await??;
                    }
                }
            }
        }
    }

    /// Stream bytes unchanged with backpressure; do not interpret remote control sequences.
    /// The caller supplies an overall deadline and owns stdout/stderr.
    pub async fn exec(
        &self,
        command: &str,
        stdout: &mut (impl tokio::io::AsyncWrite + Unpin),
        stderr: &mut (impl tokio::io::AsyncWrite + Unpin),
    ) -> Result<u32> {
        if command.trim().is_empty() || command.len() > 65536 || command.contains('\0') {
            bail!("命令不能为空、不能包含 NUL，且最多 65536 UTF-8 字节");
        }
        let mut channel = self.0.0.channel_open_session().await?;
        channel.exec(true, command.as_bytes()).await?;
        channel.eof().await?; // Non-interactive commands must not wait for stdin.
        let mut exit = None;
        let mut accepted = false;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Success => accepted = true,
                ChannelMsg::Failure => bail!("服务器拒绝 SSH exec 请求"),
                ChannelMsg::Data { data } => stdout.write_all(&data).await?,
                ChannelMsg::ExtendedData { data, ext: 1 } => stderr.write_all(&data).await?,
                ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                ChannelMsg::ExitSignal { .. } => bail!("远端命令被信号终止"),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        stdout.flush().await?;
        stderr.flush().await?;
        if !accepted {
            bail!("SSH exec 请求未被确认");
        }
        exit.context("连接已关闭，但服务器未返回命令退出码")
    }

    pub async fn sftp(&self, request: Request) -> Result<Outcome> {
        transfer::headless(self.0.clone(), request).await
    }

    pub async fn sync_directory(
        &self,
        request: crate::sftp_sync::Request,
    ) -> Result<crate::sftp_sync::Report> {
        crate::sftp_sync::run(self.0.clone(), request).await
    }

    pub async fn disconnect(self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.0.0.disconnect(
                russh::Disconnect::ByApplication,
                "RShell CLI finished",
                "en",
            ),
        )
        .await;
    }
}
