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
#[derive(Clone)]
pub enum Request {
    List(String),
    Manage {
        path: String,
        new_name: Option<String>,
    },
    Download {
        remote: String,
        local: PathBuf,
    },
    Upload {
        local: PathBuf,
        remote: String,
    },
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
    start_inner(runtime, source, request, None)
}
#[derive(Clone)]
pub struct Resume {
    id: uuid::Uuid,
    completed: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}
impl Default for Resume {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            completed: Default::default(),
        }
    }
}
pub fn start_resumable(
    runtime: &tokio::runtime::Handle,
    source: &Connection,
    request: Request,
    resume: Resume,
) -> Result<Job> {
    start_inner(runtime, source, request, Some(resume))
}
fn start_inner(
    runtime: &tokio::runtime::Handle,
    source: &Connection,
    request: Request,
    resume: Option<Resume>,
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
        let resumable = resume.is_some();
        let cancel_state = cancelled.clone();
        let result = tokio::select! {
            biased;
            _ = cancelled.changed(), if !resumable => Err(anyhow::anyhow!("已取消；未完成的 .rshell-*.part 文件可手动删除")),
            _ = disconnected.changed() => Err(anyhow::anyhow!("SSH 会话已关闭")),
            result = async {
                let channel = tokio::time::timeout(std::time::Duration::from_secs(15), session.0.channel_open_session()).await??;
                tokio::time::timeout(std::time::Duration::from_secs(15), channel.request_subsystem(true, "sftp")).await??;
                if let Request::List(path) = request {
                    let raw = russh_sftp::client::RawSftpSession::new(channel.into_stream());
                    tokio::time::timeout(std::time::Duration::from_secs(20), raw.init()).await??;
                    return tokio::time::timeout(std::time::Duration::from_secs(30), list(&raw, path)).await?;
                }
                if let Request::Manage { path, new_name } = request {
                    let raw = russh_sftp::client::RawSftpSession::new(channel.into_stream());
                    tokio::time::timeout(std::time::Duration::from_secs(20), raw.init()).await??;
                    tokio::time::timeout(std::time::Duration::from_secs(120), manage_remote(&raw, path, new_name)).await??;
                    return Ok(Outcome::Done);
                }
                let sftp = tokio::time::timeout(std::time::Duration::from_secs(20), SftpSession::new(channel.into_stream())).await??;
                let result = if let Some(resume) = resume {
                    let channel = tokio::time::timeout(std::time::Duration::from_secs(15), session.0.channel_open_session()).await??;
                    tokio::time::timeout(std::time::Duration::from_secs(15), channel.request_subsystem(true, "sftp")).await??;
                    let raw = russh_sftp::client::RawSftpSession::new(channel.into_stream());
                    tokio::time::timeout(std::time::Duration::from_secs(20), raw.init()).await??;
                    resume_tree(&sftp, &raw, request, &resume, &progress_tx, &cancel_state).await
                } else { execute(&sftp, request, &progress_tx).await };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), sftp.close()).await;
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
    copy_cancellable(reader, writer, progress, None).await
}
async fn copy_cancellable(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    progress: &watch::Sender<u64>,
    cancel: Option<&watch::Receiver<bool>>,
) -> Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    let mut total = *progress.borrow();
    loop {
        if cancel.is_some_and(|c| *c.borrow()) {
            bail!("已暂停，临时文件已保留");
        }
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
        // Acknowledged chunk boundaries make pause/retry deterministic: do not
        // report a completed chunk while remote writes are still in flight.
        if cancel.is_some() {
            tokio::time::timeout(std::time::Duration::from_secs(30), writer.flush()).await??;
        }
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
        Request::Manage { .. } => bail!("仅用于文件管理"),
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

pub(crate) async fn headless(
    session: std::sync::Arc<crate::ssh::SharedSession>,
    request: Request,
) -> Result<Outcome> {
    let mut channel = session.0.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Success) => break,
            Some(russh::ChannelMsg::WindowAdjusted { .. }) => {}
            _ => bail!("服务器拒绝 SFTP 子系统或连接已关闭"),
        }
    }
    if let Request::List(path) = request {
        let raw = russh_sftp::client::RawSftpSession::new(channel.into_stream());
        raw.init().await?;
        return list(&raw, path).await;
    }
    let sftp = SftpSession::new(channel.into_stream()).await?;
    // This entry point transfers single regular files. The GUI owns recursive/resume jobs.
    match &request {
        Request::Upload { local, .. } => {
            let meta = tokio::fs::symlink_metadata(local).await?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                bail!("CLI 上传仅支持普通文件");
            }
        }
        Request::Download { remote, .. } => {
            let meta = sftp.symlink_metadata(remote.clone()).await?;
            if !meta.is_regular() || meta.is_symlink() {
                bail!("CLI 下载仅支持普通文件");
            }
        }
        Request::Manage { .. } => bail!("仅用于文件管理"),
        Request::List(_) => unreachable!(),
    }
    let (progress, _) = watch::channel(0);
    let result = execute(&sftp, request, &progress).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), sftp.close()).await;
    result
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

// Compare the entire existing prefix before appending. A size-only check would
// silently combine unrelated source and partial files after a source edit.
async fn verify_prefix(
    source: &mut (impl tokio::io::AsyncRead + Unpin),
    partial: &mut (impl tokio::io::AsyncRead + Unpin),
    cancel: &watch::Receiver<bool>,
) -> Result<u64> {
    let mut a = vec![0; 64 * 1024];
    let mut b = vec![0; 64 * 1024];
    let mut offset = 0;
    loop {
        if *cancel.borrow() {
            bail!("已暂停，临时文件已保留");
        }
        let count = tokio::time::timeout(std::time::Duration::from_secs(30), partial.read(&mut a))
            .await??;
        if count == 0 {
            return Ok(offset);
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            source.read_exact(&mut b[..count]),
        )
        .await??;
        if a[..count] != b[..count] {
            bail!("源文件已变化或临时文件不匹配；已停止续传，请移除任务后重新添加");
        }
        offset += count as u64;
    }
}

async fn resume_tree(
    sftp: &SftpSession,
    raw: &russh_sftp::client::RawSftpSession,
    request: Request,
    resume: &Resume,
    progress: &watch::Sender<u64>,
    cancel: &watch::Receiver<bool>,
) -> Result<Outcome> {
    use tokio::io::AsyncSeekExt;
    let (local, remote, upload) = match request {
        Request::Upload { local, remote } => (local, remote, true),
        Request::Download { local, remote } => (local, remote, false),
        Request::List(_) | Request::Manage { .. } => bail!("不是传输任务"),
    };
    let mut pending = vec![(local, remote)];
    let mut visited = 0;
    while let Some((local, remote)) = pending.pop() {
        if *cancel.borrow() {
            bail!("已暂停，临时文件已保留");
        }
        visited += 1;
        if visited + pending.len() > 10000 {
            bail!("文件夹任务超过 10000 项，请分批传输");
        }
        let directory = if upload {
            let meta = tokio::fs::symlink_metadata(&local).await?;
            if meta.file_type().is_symlink() || (!meta.is_dir() && !meta.is_file()) {
                bail!("不传输链接或特殊文件：{}", local.display());
            }
            meta.is_dir()
        } else {
            let meta = sftp.symlink_metadata(remote.clone()).await?;
            if meta.is_symlink() || (!meta.is_dir() && !meta.is_regular()) {
                bail!("不传输链接或特殊文件：{remote}");
            }
            meta.is_dir()
        };
        if directory {
            if upload {
                match sftp.symlink_metadata(remote.clone()).await {
                    Ok(meta) if meta.is_dir() && !meta.is_symlink() => {}
                    Ok(_) => bail!("目标不是普通目录：{remote}"),
                    Err(russh_sftp::client::error::Error::Status(s))
                        if s.status_code == russh_sftp::protocol::StatusCode::NoSuchFile =>
                    {
                        sftp.create_dir(remote.clone()).await?
                    }
                    Err(e) => return Err(e.into()),
                }
                let mut entries = tokio::fs::read_dir(&local).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("文件名不是 UTF-8"))?;
                    safe_name(&name)?;
                    pending.push((
                        entry.path(),
                        format!("{}/{name}", remote.trim_end_matches('/')),
                    ));
                    if visited + pending.len() > 10000 {
                        bail!("文件夹任务超过 10000 项");
                    }
                }
            } else {
                match tokio::fs::symlink_metadata(&local).await {
                    Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                    Ok(_) => bail!("本地目标不是普通目录"),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tokio::fs::create_dir(&local).await?
                    }
                    Err(e) => return Err(e.into()),
                }
                let Outcome::Listed { entries, .. } = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    list(raw, remote.clone()),
                )
                .await??
                else {
                    unreachable!()
                };
                for entry in entries {
                    safe_name(&entry.name)?;
                    pending.push((
                        local.join(&entry.name),
                        format!("{}/{}", remote.trim_end_matches('/'), entry.name),
                    ));
                    if visited + pending.len() > 10000 {
                        bail!("文件夹任务超过 10000 项");
                    }
                }
            }
            continue;
        }
        let key = format!("{}\n{remote}", local.display());
        let completed = resume.completed.lock().unwrap().contains(&key);
        if upload {
            let mut source = tokio::fs::File::open(&local).await?;
            let before = source.metadata().await?;
            let partial = if completed {
                remote.clone()
            } else {
                format!("{remote}.rshell-{}.part", resume.id)
            };
            let exists = match sftp.symlink_metadata(partial.clone()).await {
                Ok(meta) if meta.is_regular() && !meta.is_symlink() => true,
                Ok(_) => bail!("续传目标不是普通文件"),
                Err(russh_sftp::client::error::Error::Status(s))
                    if s.status_code == russh_sftp::protocol::StatusCode::NoSuchFile =>
                {
                    false
                }
                Err(e) => return Err(e.into()),
            };
            if completed && !exists {
                bail!("已完成文件被删除，请重新添加任务");
            }
            let offset = if exists {
                verify_prefix(&mut source, &mut sftp.open(partial.clone()).await?, cancel).await?
            } else {
                0
            };
            progress.send_modify(|n| *n += offset);
            if completed {
                if offset != before.len() {
                    bail!("已完成文件的源已变化");
                }
                continue;
            }
            let flags = if exists {
                OpenFlags::WRITE
            } else {
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE
            };
            let mut output = sftp.open_with_flags(&partial, flags).await?;
            output.seek(std::io::SeekFrom::Start(offset)).await?;
            copy_cancellable(&mut source, &mut output, progress, Some(cancel)).await?;
            output.shutdown().await?;
            let after = source.metadata().await?;
            if before.len() != after.len() || before.modified()? != after.modified()? {
                bail!("传输期间源文件发生变化，未发布目标文件");
            }
            sftp.rename(&partial, remote).await?;
        } else {
            let mut source = sftp.open(remote.clone()).await?;
            let before = source.metadata().await?;
            let partial = if completed {
                local.clone()
            } else {
                local.with_file_name(format!(
                    ".{}.rshell-{}.part",
                    local.file_name().context("无文件名")?.to_string_lossy(),
                    resume.id
                ))
            };
            let exists = match tokio::fs::symlink_metadata(&partial).await {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => true,
                Ok(_) => bail!("续传目标不是普通文件"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.into()),
            };
            if completed && !exists {
                bail!("已完成文件被删除，请重新添加任务");
            }
            let offset = if exists {
                verify_prefix(
                    &mut source,
                    &mut tokio::fs::File::open(&partial).await?,
                    cancel,
                )
                .await?
            } else {
                0
            };
            progress.send_modify(|n| *n += offset);
            if completed {
                if Some(offset) != before.size {
                    bail!("已完成文件的源已变化");
                }
                continue;
            }
            let mut output = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(!exists)
                .open(&partial)
                .await?;
            output.seek(std::io::SeekFrom::Start(offset)).await?;
            copy_cancellable(&mut source, &mut output, progress, Some(cancel)).await?;
            output.sync_all().await?;
            drop(output);
            let after = source.metadata().await?;
            if before.size != after.size || before.mtime != after.mtime {
                bail!("传输期间源文件发生变化，未发布目标文件");
            }
            publish(&partial, &local).await?;
        }
        resume.completed.lock().unwrap().insert(key);
    }
    Ok(Outcome::Done)
}

async fn manage_remote(
    sftp: &russh_sftp::client::RawSftpSession,
    path: String,
    new_name: Option<String>,
) -> Result<()> {
    let (parent, name) = path.rsplit_once('/').context("不可操作根目录")?;
    safe_name(name)?;
    if let Some(name) = new_name {
        safe_name(&name)?;
        let target = format!("{parent}/{name}");
        match sftp.lstat(target.clone()).await {
            Err(russh_sftp::client::error::Error::Status(s))
                if s.status_code == russh_sftp::protocol::StatusCode::NoSuchFile => {}
            Err(e) => return Err(e.into()),
            Ok(_) => bail!("目标名称已存在，不覆盖"),
        }
        sftp.rename(path, target).await?;
    } else {
        let mut stack = vec![(path, false)];
        let mut count = 0;
        while let Some((path, visited)) = stack.pop() {
            count += 1;
            anyhow::ensure!(count <= 20000, "目录过大，删除已停止");
            let meta = sftp.lstat(path.clone()).await?.attrs;
            if meta.is_dir() && !meta.is_symlink() {
                if visited {
                    sftp.rmdir(path).await?;
                } else {
                    stack.push((path.clone(), true));
                    let Outcome::Listed { entries, .. } = list(sftp, path.clone()).await? else {
                        unreachable!()
                    };
                    for entry in entries {
                        let name = entry.name;
                        if name == "." || name == ".." {
                            continue;
                        }
                        safe_name(&name)?;
                        anyhow::ensure!(stack.len() < 10000, "目录过大，删除已停止");
                        stack.push((format!("{path}/{name}"), false));
                    }
                }
            } else {
                sftp.remove(path).await?;
            }
        }
    }
    Ok(())
}
pub async fn manage_local(path: PathBuf, new_name: Option<String>) -> Result<()> {
    let parent = path.parent().context("不可操作根目录")?;
    anyhow::ensure!(path.file_name().is_some(), "不可操作根目录");
    if let Some(name) = new_name {
        safe_name(&name)?;
        let target = parent.join(name);
        match tokio::fs::symlink_metadata(&target).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(_) => bail!("目标名称已存在，不覆盖"),
        }
        // Windows rename does not replace an existing destination.
        tokio::fs::rename(path, target).await?;
    } else {
        let meta = tokio::fs::symlink_metadata(&path).await?;
        if meta.is_dir() {
            tokio::fs::remove_dir_all(path).await?;
        } else {
            tokio::fs::remove_file(path).await?;
        }
    }
    Ok(())
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

    #[tokio::test]
    async fn local_rename_collision_and_recursive_delete() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-artifacts");
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        let source = dir.path().join("source");
        tokio::fs::create_dir_all(source.join("child"))
            .await
            .unwrap();
        tokio::fs::write(source.join("child/file"), b"data")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("occupied"), b"keep")
            .await
            .unwrap();
        assert!(
            manage_local(source.clone(), Some("occupied".into()))
                .await
                .is_err()
        );
        assert!(
            manage_local(source.clone(), Some("../escape".into()))
                .await
                .is_err()
        );
        manage_local(source.clone(), Some("renamed".into()))
            .await
            .unwrap();
        assert!(!source.exists());
        manage_local(dir.path().join("renamed"), None)
            .await
            .unwrap();
        assert!(!dir.path().join("renamed").exists());
        assert_eq!(
            tokio::fs::read(dir.path().join("occupied")).await.unwrap(),
            b"keep"
        );
    }
}
