use russh_sftp::protocol::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
pub struct Sftp {
    pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub attributes: Arc<Mutex<HashMap<String, FileAttributes>>>,
    pub disable_posix_rename: bool,
    pub fail_writes: bool,
    listed: bool,
}
fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}
impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
        let mut version = Version::new();
        if !self.disable_posix_rename {
            version
                .extensions
                .insert("posix-rename@openssh.com".into(), "1".into());
        }
        Ok(version)
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }
    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        self.stat(id, handle).await
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        if let Some(attrs) = self.attributes.lock().unwrap().get(&path) {
            return Ok(Attrs {
                id,
                attrs: attrs.clone(),
            });
        }
        let files = self.files.lock().unwrap();
        let dir = path == "/" || files.contains_key(&format!("{}/", path.trim_end_matches('/')));
        let size = if dir {
            0
        } else {
            files.get(&path).ok_or(StatusCode::NoSuchFile)?.len() as u64
        };
        Ok(Attrs {
            id,
            attrs: FileAttributes {
                size: Some(size),
                permissions: Some(if dir { 0o040755 } else { 0o100644 }),
                ..Default::default()
            },
        })
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<Status, StatusCode> {
        self.files
            .lock()
            .unwrap()
            .insert(format!("{}/", path.trim_end_matches('/')), vec![]);
        Ok(ok(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.files
            .lock()
            .unwrap()
            .remove(&path)
            .ok_or(StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }
    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        let path = format!("{}/", path.trim_end_matches('/'));
        let mut files = self.files.lock().unwrap();
        if files.keys().any(|p| p != &path && p.starts_with(&path)) {
            return Err(StatusCode::Failure);
        }
        files.remove(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }
    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, StatusCode> {
        if self.disable_posix_rename || request != "posix-rename@openssh.com" {
            return Err(StatusCode::OpUnsupported);
        }
        let mut data = data.as_slice();
        let mut string = || -> Result<String, StatusCode> {
            if data.len() < 4 {
                return Err(StatusCode::BadMessage);
            }
            let len = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
            data = &data[4..];
            if data.len() < len {
                return Err(StatusCode::BadMessage);
            }
            let text = std::str::from_utf8(&data[..len])
                .map_err(|_| StatusCode::BadMessage)?
                .to_owned();
            data = &data[len..];
            Ok(text)
        };
        let old = string()?;
        let new = string()?;
        if !data.is_empty() {
            return Err(StatusCode::BadMessage);
        }
        let mut files = self.files.lock().unwrap();
        let bytes = files.remove(&old).ok_or(StatusCode::NoSuchFile)?;
        files.insert(new, bytes);
        Ok(Packet::Status(ok(id)))
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(ok(id))
    }
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let mut files = self.files.lock().unwrap();
        if flags.contains(OpenFlags::CREATE) {
            if files.contains_key(&filename) && flags.contains(OpenFlags::EXCLUDE) {
                return Err(StatusCode::Failure);
            }
            files.insert(filename.clone(), Vec::new());
        }
        if !files.contains_key(&filename) {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Handle {
            id,
            handle: filename,
        })
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        if handle.starts_with("/slow-download") {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let files = self.files.lock().unwrap();
        let file = files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        if start >= file.len() {
            return Err(StatusCode::Eof);
        }
        Ok(Data {
            id,
            data: file[start..(start + len as usize).min(file.len())].to_vec(),
        })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        if self.fail_writes {
            return Err(StatusCode::PermissionDenied);
        }
        if handle.starts_with("/slow") {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let mut files = self.files.lock().unwrap();
        let file = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        file.resize(file.len().max(start + data.len()), 0);
        file[start..start + data.len()].copy_from_slice(&data);
        Ok(ok(id))
    }
    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, StatusCode> {
        let mut files = self.files.lock().unwrap();
        if files.contains_key(&new) {
            return Err(StatusCode::Failure);
        }
        let data = files.remove(&old).ok_or(StatusCode::NoSuchFile)?;
        files.insert(new, data);
        Ok(ok(id))
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name {
            id,
            files: vec![File::dummy(if path == "." { "/" } else { &path })],
        })
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        self.listed = false;
        Ok(Handle { id, handle: path })
    }
    async fn readdir(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        if self.listed {
            return Err(StatusCode::Eof);
        }
        self.listed = true;
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let files = self.files.lock().unwrap();
        let mut entries = std::collections::BTreeMap::new();
        for (name, data) in files.iter() {
            if let Some(relative) = name.strip_prefix(&prefix) {
                if relative.is_empty() {
                    continue;
                }
                let dir = relative.contains('/');
                let name = relative.split('/').next().unwrap().to_string();
                entries.insert(
                    name,
                    FileAttributes {
                        size: Some(data.len() as u64),
                        permissions: Some(if dir { 0o040755 } else { 0o100644 }),
                        ..Default::default()
                    },
                );
            }
        }
        Ok(Name {
            id,
            files: entries
                .into_iter()
                .map(|(name, attrs)| File::new(name, attrs))
                .collect(),
        })
    }
}

impl Sftp {
    pub fn new(files: Arc<Mutex<HashMap<String, Vec<u8>>>>) -> Self {
        Self {
            files,
            listed: false,
            attributes: Default::default(),
            disable_posix_rename: false,
            fail_writes: false,
        }
    }
}

pub fn tempdir() -> tempfile::TempDir {
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-artifacts");
    let root = std::path::absolute(root).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
