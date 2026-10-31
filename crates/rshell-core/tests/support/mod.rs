use russh_sftp::protocol::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
pub struct Sftp {
    pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
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
        Ok(Version::new())
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
    async fn realpath(&mut self, id: u32, _: String) -> Result<Name, StatusCode> {
        Ok(Name {
            id,
            files: vec![File::dummy("/")],
        })
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        self.listed = false;
        Ok(Handle { id, handle: path })
    }
    async fn readdir(&mut self, id: u32, _: String) -> Result<Name, StatusCode> {
        if self.listed {
            return Err(StatusCode::Eof);
        }
        self.listed = true;
        Ok(Name {
            id,
            files: self
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|(name, data)| {
                    File::new(
                        name.trim_start_matches('/'),
                        FileAttributes {
                            size: Some(data.len() as u64),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        })
    }
}

impl Sftp {
    pub fn new(files: Arc<Mutex<HashMap<String, Vec<u8>>>>) -> Self {
        Self {
            files,
            listed: false,
        }
    }
}

pub fn tempdir() -> tempfile::TempDir {
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-artifacts");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
