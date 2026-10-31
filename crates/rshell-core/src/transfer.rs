use crate::connection::Connection;
use anyhow::{Context, Result, bail};
use russh_sftp::{client::SftpSession, protocol::OpenFlags};
use std::path::{Path, PathBuf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{oneshot, watch},
};

pub struct Entry {
    pub name: String,
    pub directory: bool,
    pub size: u64,
}
pub enum Request {
    List(String),
    Download { remote: String, local: PathBuf },
    Upload { local: PathBuf, remote: String },
}
pub enum Outcome {
    Listed { path: String, entries: Vec<Entry> },
    Done,
}
pub struct Job {
    pub result: oneshot::Receiver<Result<Outcome, String>>,
    pub progress: watch::Receiver<u64>,
    cancel: watch::Sender<bool>,
}
impl Job {
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub fn start(
    runtime: &tokio::runtime::Handle,
    source: &Connection,
    request: Request,
) -> Result<Job> {
    let session = source
        .ssh_session
        .lock()
        .unwrap()
        .upgrade()
        .context("请先连接 SSH 会话")?;
    let mut disconnected = source.cancel.subscribe();
    if *disconnected.borrow() {
        bail!("SSH 会话已关闭");
    }
    let (cancel, mut cancelled) = watch::channel(false);
    let (progress_tx, progress) = watch::channel(0);
    let (tx, result) = oneshot::channel();
    runtime.spawn(async move {
        let result = tokio::select! {
            biased;
            _ = cancelled.changed() => Err(anyhow::anyhow!("已取消；未完成的 .rshell-*.part 文件可手动删除")),
            _ = disconnected.changed() => Err(anyhow::anyhow!("SSH 会话已关闭")),
            result = async {
                let channel = tokio::time::timeout(std::time::Duration::from_secs(15), session.0.channel_open_session()).await??;
                channel.request_subsystem(true, "sftp").await?;
                if let Request::List(path) = request {
                    let raw = russh_sftp::client::RawSftpSession::new(channel.into_stream());
                    raw.init().await?;
                    return tokio::time::timeout(std::time::Duration::from_secs(30), list(&raw, path)).await?;
                }
                let sftp = tokio::time::timeout(std::time::Duration::from_secs(20), SftpSession::new(channel.into_stream())).await??;
                let result = execute(&sftp, request, &progress_tx).await;
                let _ = sftp.close().await;
                result
            } => result,
        };
        let _ = tx.send(result.map_err(|e| format!("{e:#}")));
    });
    Ok(Job {
        result,
        progress,
        cancel,
    })
}

async fn copy(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    progress: &watch::Sender<u64>,
) -> Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0;
    loop {
        let count =
            tokio::time::timeout(std::time::Duration::from_secs(30), reader.read(&mut buffer))
                .await??;
        if count == 0 {
            break;
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            writer.write_all(&buffer[..count]),
        )
        .await??;
        total += count as u64;
        progress.send_replace(total);
    }
    writer.flush().await?;
    Ok(())
}
async fn execute(
    sftp: &SftpSession,
    request: Request,
    progress: &watch::Sender<u64>,
) -> Result<Outcome> {
    match request {
        Request::List(_) => unreachable!(),
        Request::Download { remote, local } => {
            let partial = partial_path(&local)?;
            let mut input = sftp.open(remote).await?;
            let mut output = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial)
                .await?;
            copy(&mut input, &mut output, progress).await?;
            output.sync_all().await?;
            drop(output);
            publish(&partial, &local).await?;
            Ok(Outcome::Done)
        }
        Request::Upload { local, remote } => {
            let mut input = tokio::fs::File::open(local).await?;
            let partial = format!("{remote}.rshell-{}.part", uuid::Uuid::new_v4());
            let mut output = sftp
                .open_with_flags(
                    &partial,
                    OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                )
                .await?;
            copy(&mut input, &mut output, progress).await?;
            output.shutdown().await?;
            // SFTP v3 RENAME fails if the destination already exists.
            sftp.rename(&partial, remote).await?;
            Ok(Outcome::Done)
        }
    }
}
pub(crate) fn partial_path(target: &Path) -> Result<PathBuf> {
    let parent = target.parent().context("未选择目标目录")?;
    Ok(parent.join(format!(".rshell-{}.part", uuid::Uuid::new_v4())))
}
pub(crate) async fn publish(partial: &Path, target: &Path) -> Result<()> {
    tokio::fs::hard_link(partial, target)
        .await
        .context("保存失败：目标已存在或文件系统不支持硬链接；临时文件已保留")?;
    tokio::fs::remove_file(partial).await?;
    Ok(())
}
pub fn safe_name(name: &str) -> Result<&str> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty()
        || name.len() > 240
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:<>\"|?*".contains(c))
        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
    {
        bail!("不安全或不支持的文件名");
    }
    Ok(name)
}
async fn list(raw: &russh_sftp::client::RawSftpSession, path: String) -> Result<Outcome> {
    let path = raw
        .realpath(path)
        .await?
        .files
        .into_iter()
        .next()
        .context("服务器未返回目录")?
        .filename;
    // Raw READDIR is paged to bound directory memory even for untrusted servers.

    let handle = raw.opendir(path.clone()).await?.handle;
    let mut entries = Vec::new();
    loop {
        match raw.readdir(handle.clone()).await {
            Ok(names) => {
                for entry in names.files {
                    if entries.len() >= 10000 {
                        bail!("目录超过 10000 项，请输入更具体的路径");
                    }
                    if entry.filename != "." && entry.filename != ".." {
                        entries.push(Entry {
                            name: entry.filename,
                            directory: entry.attrs.is_dir(),
                            size: entry.attrs.size.unwrap_or(0),
                        });
                    }
                }
            }
            Err(russh_sftp::client::error::Error::Status(status))
                if status.status_code == russh_sftp::protocol::StatusCode::Eof =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }
    raw.close(handle).await?;
    entries.sort_by(|a, b| b.directory.cmp(&a.directory).then(a.name.cmp(&b.name)));
    Ok(Outcome::Listed { path, entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_remote_paths_and_devices() {
        for name in [
            "../x", "..", "/tmp/x", "a\\b", "x:stream", "CON.txt", "LPT1", "x.", "x\n",
        ] {
            assert!(safe_name(name).is_err(), "{name}");
        }
        assert_eq!(safe_name("报告.txt").unwrap(), "报告.txt");
    }
    #[tokio::test]
    async fn publishing_never_overwrites() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-artifacts");
        tokio::fs::create_dir_all(&root).await.unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        let destination = dir.path().join("file");
        let partial = partial_path(&destination).unwrap();
        tokio::fs::write(&partial, b"new").await.unwrap();
        tokio::fs::write(&destination, b"old").await.unwrap();
        assert!(publish(&partial, &destination).await.is_err());
        assert_eq!(tokio::fs::read(destination).await.unwrap(), b"old");
    }
}
