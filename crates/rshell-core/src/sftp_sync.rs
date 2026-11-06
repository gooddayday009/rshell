//! One-way, content-based directory sync. Destructive work is explicit and runs last.
use anyhow::{Context, Result, bail, ensure};
use russh_sftp::{
    client::{RawSftpSession, error::Error as SftpError},
    protocol::{FileAttributes, OpenFlags, Packet, StatusCode},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_ENTRIES: usize = 10000;
const MAX_DEPTH: usize = 32;
const CHUNK: usize = 32768;
const POSIX_RENAME: &str = "posix-rename@openssh.com";

pub struct Request {
    pub local: PathBuf,
    pub remote: String,
    pub upload: bool,
    pub dry_run: bool,
    pub delete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Mkdir,
    Create,
    Update,
    Skip,
    DeleteFile,
    DeleteDir,
}

#[derive(Debug, Serialize)]
pub struct Operation {
    pub kind: Kind,
    /// Relative to the selected root. Empty string denotes the root itself.
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub upload: bool,
    pub dry_run: bool,
    pub delete: bool,
    pub local: PathBuf,
    pub remote: String,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    directory: bool,
    size: u64,
    hash: Option<[u8; 32]>,
}
type Tree = BTreeMap<String, Node>;

struct Roots {
    local: PathBuf,
    remote: String,
    sftp: RawSftpSession,
    posix_rename: bool,
}

pub(crate) async fn run(
    session: Arc<crate::ssh::SharedSession>,
    request: Request,
) -> Result<Report> {
    let local = local_root(&request.local)?;
    let remote = remote_root(&request.remote)?;
    if request.delete {
        ensure!(
            if request.upload {
                remote != "/"
            } else {
                local.parent().is_some()
            },
            "镜像删除不允许以文件系统根目录为目标"
        );
    }
    let mut channel = session.0.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Success) => break,
            Some(russh::ChannelMsg::WindowAdjusted { .. }) => {}
            _ => bail!("服务器拒绝 SFTP 子系统或连接已关闭"),
        }
    }
    let sftp = RawSftpSession::new(channel.into_stream());
    sftp.set_timeout(30);
    let version = sftp.init().await?;
    let roots = Roots {
        local,
        remote,
        posix_rename: version
            .extensions
            .get(POSIX_RENAME)
            .is_some_and(|v| v == "1"),
        sftp,
    };
    let local_tree = roots.scan(true).await?;
    let remote_tree = roots.scan(false).await?;
    let (source, destination) = if request.upload {
        (&local_tree, &remote_tree)
    } else {
        (&remote_tree, &local_tree)
    };
    ensure!(
        source.get("").is_some_and(|n| n.directory),
        "同步源必须是已存在的普通目录"
    );
    let operations = plan(source, destination, request.delete)?;
    if !request.dry_run {
        if request.upload && operations.iter().any(|op| op.kind == Kind::Update) {
            ensure!(
                roots.posix_rename,
                "服务器未声明 posix-rename@openssh.com v1，无法安全替换已有文件；未执行同步"
            );
        }
        roots
            .apply(
                &operations,
                source,
                destination,
                request.upload,
                request.delete,
            )
            .await?;
    }
    Ok(Report {
        upload: request.upload,
        dry_run: request.dry_run,
        delete: request.delete,
        local: roots.local,
        remote: roots.remote,
        operations,
    })
}

fn local_root(path: &Path) -> Result<PathBuf> {
    ensure!(
        !path.as_os_str().is_empty() && !path.components().any(|p| p == Component::ParentDir),
        "本地根路径不能为空或包含 .."
    );
    Ok(std::path::absolute(path)?)
}
fn remote_root(path: &str) -> Result<String> {
    ensure!(
        path.starts_with('/')
            && path.len() <= 4096
            && !path.chars().any(|c| c.is_control() || c == '\\'),
        "远端根目录必须是绝对路径，且不含控制字符或反斜杠"
    );
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    ensure!(
        parts.iter().all(|p| *p != "." && *p != ".."),
        "远端根路径不能包含 . 或 .."
    );
    Ok(format!("/{}", parts.join("/")))
}
fn child(parent: &str, name: &str) -> Result<String> {
    crate::transfer::safe_name(name)?;
    ensure!(!matches!(name, "." | ".."), "目录返回了不安全路径");
    let path = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    };
    ensure!(
        path.len() <= 4096 && path.split('/').count() <= MAX_DEPTH,
        "目录超过 32 层或相对路径超过 4096 字节"
    );
    Ok(path)
}
fn plan(source: &Tree, destination: &Tree, delete: bool) -> Result<Vec<Operation>> {
    let folded: BTreeMap<_, _> = destination.keys().map(|p| (p.to_lowercase(), p)).collect();
    let mut operations = Vec::new();
    for (path, node) in source {
        if let Some(other) = folded.get(&path.to_lowercase()) {
            ensure!(
                *other == path,
                "源与目标存在大小写冲突：{path:?} / {other:?}"
            );
        }
        let kind = match destination.get(path) {
            Some(old) => {
                ensure!(
                    old.directory == node.directory,
                    "文件与目录类型冲突：{path:?}"
                );
                if node.directory {
                    continue;
                }
                if old == node {
                    Kind::Skip
                } else {
                    Kind::Update
                }
            }
            None if node.directory => Kind::Mkdir,
            None => Kind::Create,
        };
        operations.push(Operation {
            kind,
            path: path.clone(),
            bytes: node.size,
        });
    }
    if delete {
        // Children precede parents; neither root is ever deleted. No recursive remove.
        for (path, node) in destination.iter().rev() {
            if !path.is_empty() && !source.contains_key(path) {
                operations.push(Operation {
                    kind: if node.directory {
                        Kind::DeleteDir
                    } else {
                        Kind::DeleteFile
                    },
                    path: path.clone(),
                    bytes: node.size,
                });
            }
        }
    }
    Ok(operations)
}

impl Roots {
    fn remote_path(&self, path: &str) -> String {
        if path.is_empty() {
            self.remote.clone()
        } else {
            format!("{}/{path}", self.remote.trim_end_matches('/'))
        }
    }
    /// Check every ancestor again before I/O. Never follow existing symlinks/junctions.
    async fn guard(&self, local: bool, path: &str, missing_leaf: bool) -> Result<()> {
        if local {
            let target = self.local.join(path);
            let mut prefix = PathBuf::new();
            for part in target.components() {
                prefix.push(part.as_os_str());
                match tokio::fs::symlink_metadata(&prefix).await {
                    Ok(meta) => ensure!(
                        !local_link(&meta),
                        "不跟随本地符号链接或重解析点：{}",
                        prefix.display()
                    ),
                    Err(e)
                        if e.kind() == std::io::ErrorKind::NotFound
                            && missing_leaf
                            && prefix == target => {}
                    Err(e) => return Err(e).context("读取本地路径失败；根目录的父目录必须存在"),
                }
            }
        } else {
            let target = self.remote_path(path);
            let mut prefix = String::new();
            for part in target.split('/').filter(|p| !p.is_empty()) {
                prefix.push('/');
                prefix.push_str(part);
                match self.sftp.lstat(&prefix).await {
                    Ok(meta) => ensure!(!meta.attrs.is_symlink(), "不跟随远端符号链接：{prefix:?}"),
                    Err(e) if missing(&e) && missing_leaf && prefix == target => {}
                    Err(e) => return Err(e).context("读取远端路径失败；根目录的父目录必须存在"),
                }
            }
        }
        Ok(())
    }
    async fn node(&self, local: bool, path: &str) -> Result<Option<Node>> {
        self.guard(local, path, true).await?;
        if local {
            let path = self.local.join(path);
            let before = match tokio::fs::symlink_metadata(&path).await {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            ensure!(
                !local_link(&before) && (before.is_file() || before.is_dir()),
                "不支持链接或特殊文件：{}",
                path.display()
            );
            if before.is_dir() {
                return Ok(Some(Node {
                    directory: true,
                    size: 0,
                    hash: None,
                }));
            }
            let mut file = tokio::fs::File::open(&path).await?;
            let mut hash = Sha256::new();
            let mut buffer = vec![0; CHUNK];
            let mut size = 0;
            loop {
                let count = file.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                size += count as u64;
                ensure!(size <= before.len(), "扫描时本地文件发生变化");
                hash.update(&buffer[..count]);
            }
            let after = file.metadata().await?;
            ensure!(
                size == before.len()
                    && before.len() == after.len()
                    && before.modified()? == after.modified()?,
                "扫描时本地文件发生变化"
            );
            Ok(Some(Node {
                directory: false,
                size,
                hash: Some(hash.finalize().into()),
            }))
        } else {
            let path = self.remote_path(path);
            let before = match self.sftp.lstat(&path).await {
                Ok(meta) => meta.attrs,
                Err(e) if missing(&e) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            ensure!(
                !before.is_symlink() && (before.is_dir() || before.is_regular()),
                "不支持远端链接或特殊文件：{path:?}"
            );
            if before.is_dir() {
                return Ok(Some(Node {
                    directory: true,
                    size: 0,
                    hash: None,
                }));
            }
            let expected = before.size.context("服务器未返回文件大小")?;
            let handle = self
                .sftp
                .open(&path, OpenFlags::READ, FileAttributes::default())
                .await?
                .handle;
            let result = async {
                let mut hash = Sha256::new();
                let mut size = 0;
                while let Some(data) = self.read(&handle, size).await? {
                    size += data.len() as u64;
                    ensure!(size <= expected, "扫描时远端文件发生变化");
                    hash.update(&data);
                }
                let after = self.sftp.fstat(&handle).await?.attrs;
                ensure!(
                    size == expected && before.size == after.size && before.mtime == after.mtime,
                    "扫描时远端文件发生变化"
                );
                Ok(Some(Node {
                    directory: false,
                    size,
                    hash: Some(hash.finalize().into()),
                }))
            }
            .await;
            let closed = self.sftp.close(handle).await;
            let node = result?;
            closed?;
            Ok(node)
        }
    }
    async fn read(&self, handle: &str, offset: u64) -> Result<Option<Vec<u8>>> {
        match self.sftp.read(handle, offset, CHUNK as u32).await {
            Ok(data) => {
                ensure!(
                    !data.data.is_empty() && data.data.len() <= CHUNK,
                    "服务器返回无效文件块"
                );
                Ok(Some(data.data))
            }
            Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    async fn children(&self, local: bool, path: &str) -> Result<Vec<String>> {
        self.guard(local, path, false).await?;
        let mut paths = Vec::new();
        if local {
            let mut entries = tokio::fs::read_dir(self.local.join(path)).await?;
            while let Some(entry) = entries.next_entry().await? {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("文件名必须为 Unicode"))?;
                paths.push(child(path, &name)?);
                ensure!(paths.len() <= MAX_ENTRIES, "目录超过 10000 项");
            }
        } else {
            let handle = self.sftp.opendir(self.remote_path(path)).await?.handle;
            let result = async {
                loop {
                    match self.sftp.readdir(&handle).await {
                        Ok(names) => {
                            for entry in names.files {
                                if matches!(entry.filename.as_str(), "." | "..") {
                                    continue;
                                }
                                paths.push(child(path, &entry.filename)?);
                                ensure!(paths.len() <= MAX_ENTRIES, "目录超过 10000 项");
                            }
                        }
                        Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => {
                            break;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let closed = self.sftp.close(handle).await;
            result?;
            closed?;
        }
        paths.sort();
        Ok(paths)
    }
    async fn scan(&self, local: bool) -> Result<Tree> {
        let mut tree = Tree::new();
        let mut folded = HashSet::new();
        let mut pending = vec![String::new()];
        while let Some(path) = pending.pop() {
            let Some(node) = self.node(local, &path).await? else {
                ensure!(path.is_empty(), "扫描时目录条目已消失：{path:?}");
                continue;
            };
            ensure!(
                folded.insert(path.to_lowercase()),
                "重复或大小写冲突路径：{path:?}"
            );
            if node.directory {
                pending.extend(self.children(local, &path).await?);
            }
            tree.insert(path, node);
            ensure!(
                tree.len() + pending.len() <= MAX_ENTRIES,
                "目录树超过 10000 项"
            );
        }
        Ok(tree)
    }
    async fn apply(
        &self,
        operations: &[Operation],
        source: &Tree,
        destination: &Tree,
        upload: bool,
        delete: bool,
    ) -> Result<()> {
        let mut expected = destination.clone();
        for op in operations {
            if matches!(op.kind, Kind::DeleteFile | Kind::DeleteDir) {
                continue;
            }
            if op.kind == Kind::Skip {
                continue;
            }
            ensure!(
                self.node(upload, &op.path).await?.as_ref() == source.get(&op.path),
                "同步源已变化：{:?}",
                op.path
            );
            ensure!(
                self.node(!upload, &op.path).await?.as_ref() == destination.get(&op.path),
                "同步目标已变化：{:?}",
                op.path
            );
            match op.kind {
                Kind::Mkdir => {
                    if upload {
                        self.sftp
                            .mkdir(self.remote_path(&op.path), FileAttributes::default())
                            .await?;
                    } else {
                        tokio::fs::create_dir(self.local.join(&op.path)).await?;
                    }
                }
                Kind::Create | Kind::Update => {
                    self.copy_file(
                        &op.path,
                        source.get(&op.path).unwrap(),
                        destination.get(&op.path),
                        upload,
                    )
                    .await?
                }
                _ => unreachable!(),
            }
            expected.insert(op.path.clone(), source[&op.path].clone());
        }
        if delete
            && operations
                .iter()
                .any(|op| matches!(op.kind, Kind::DeleteFile | Kind::DeleteDir))
        {
            // A failed/incomplete scan or transfer must never trigger pruning.
            ensure!(
                self.scan(upload).await? == *source,
                "源目录在同步期间发生变化，未执行镜像删除"
            );
            ensure!(
                self.scan(!upload).await? == expected,
                "目标目录在同步期间发生变化，未执行镜像删除"
            );
            for op in operations
                .iter()
                .filter(|op| matches!(op.kind, Kind::DeleteFile | Kind::DeleteDir))
            {
                ensure!(!op.path.is_empty(), "不能删除同步根目录");
                ensure!(
                    self.node(!upload, &op.path).await?.as_ref() == destination.get(&op.path),
                    "删除前目标已变化：{:?}",
                    op.path
                );
                if upload {
                    if op.kind == Kind::DeleteDir {
                        self.sftp.rmdir(self.remote_path(&op.path)).await?;
                    } else {
                        self.sftp.remove(self.remote_path(&op.path)).await?;
                    }
                } else if op.kind == Kind::DeleteDir {
                    tokio::fs::remove_dir(self.local.join(&op.path)).await?;
                } else {
                    tokio::fs::remove_file(self.local.join(&op.path)).await?;
                }
            }
        }
        Ok(())
    }
    async fn copy_file(
        &self,
        path: &str,
        source: &Node,
        destination: Option<&Node>,
        upload: bool,
    ) -> Result<()> {
        let suffix = format!(".rshell-sync-{}.part", uuid::Uuid::new_v4());
        let partial = match path.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{suffix}"),
            None => suffix,
        };
        self.guard(!upload, &partial, true).await?;
        if upload {
            let mut input = tokio::fs::File::open(self.local.join(path)).await?;
            let handle = self
                .sftp
                .open(
                    self.remote_path(&partial),
                    OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                    FileAttributes::default(),
                )
                .await?
                .handle;
            let copied = async {
                let mut buffer = vec![0; CHUNK];
                let mut hash = Sha256::new();
                let mut size = 0;
                loop {
                    let count = input.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    ensure!(size + count as u64 <= source.size, "上传期间源文件发生变化");
                    self.sftp
                        .write(&handle, size, buffer[..count].to_vec())
                        .await?;
                    size += count as u64;
                    hash.update(&buffer[..count]);
                }
                ensure!(
                    size == source.size && Some(<[u8; 32]>::from(hash.finalize())) == source.hash,
                    "上传期间源文件内容发生变化"
                );
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let closed = self.sftp.close(handle).await;
            copied?;
            closed?;
        } else {
            let mut output = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(self.local.join(&partial))
                .await?;
            let handle = self
                .sftp
                .open(
                    self.remote_path(path),
                    OpenFlags::READ,
                    FileAttributes::default(),
                )
                .await?
                .handle;
            let copied = async {
                let mut size = 0;
                let mut hash = Sha256::new();
                while let Some(data) = self.read(&handle, size).await? {
                    size += data.len() as u64;
                    ensure!(size <= source.size, "下载期间源文件发生变化");
                    output.write_all(&data).await?;
                    hash.update(&data);
                }
                output.sync_all().await?;
                ensure!(
                    size == source.size && Some(<[u8; 32]>::from(hash.finalize())) == source.hash,
                    "下载期间源文件内容发生变化"
                );
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let closed = self.sftp.close(handle).await;
            drop(output);
            copied?;
            closed?;
        }
        // Verify both source and staged content, then re-check the old destination.
        ensure!(
            self.node(upload, path).await?.as_ref() == Some(source),
            "发布前同步源已变化"
        );
        ensure!(
            self.node(!upload, &partial).await?.as_ref() == Some(source),
            "临时文件内容校验失败，未发布目标"
        );
        ensure!(
            self.node(!upload, path).await?.as_ref() == destination,
            "发布前同步目标已变化"
        );
        if upload {
            let old = self.remote_path(&partial);
            let new = self.remote_path(path);
            if destination.is_some() {
                // OpenSSH PROTOCOL 4.3: two SSH strings, then SSH_FXP_STATUS.
                let mut data = Vec::new();
                for value in [&old, &new] {
                    data.extend_from_slice(&(value.len() as u32).to_be_bytes());
                    data.extend_from_slice(value.as_bytes());
                }
                match self.sftp.extended(POSIX_RENAME, data).await? {
                    Packet::Status(status) if status.status_code == StatusCode::Ok => {}
                    _ => bail!("原子替换远端文件失败；原目标未主动删除，临时文件保留"),
                }
            } else {
                self.sftp.rename(old, new).await?;
            }
        } else if destination.is_some() {
            tokio::fs::rename(self.local.join(partial), self.local.join(path)).await?;
        } else {
            crate::transfer::publish(&self.local.join(partial), &self.local.join(path)).await?;
        }
        Ok(())
    }
}
fn missing(error: &SftpError) -> bool {
    matches!(error, SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile)
}
fn local_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
