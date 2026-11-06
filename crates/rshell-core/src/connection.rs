use anyhow::Result;
use tokio::sync::{mpsc, oneshot, watch};

pub enum Command {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

pub enum Event {
    Status(String),
    Tunnels(Vec<String>),
    HostKey {
        fingerprint: String,
        algorithm: String,
        public_key: String,
        reply: oneshot::Sender<bool>,
    },
    TransferPrompt {
        upload: bool,
        reply: oneshot::Sender<Option<std::path::PathBuf>>,
    },
    Connected,
    Finished(Result<(), String>),
}

/// Called on the connection worker, never while holding a GUI lock.
pub trait Terminal: Send + 'static {
    /// Parse bytes and return protocol replies for the same terminal.
    fn output(&mut self, data: &[u8]) -> Vec<u8>;
    fn resize(&mut self, cols: u16, rows: u16);
}

pub struct Connection {
    pub commands: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<Event>,
    pub(crate) ssh_session:
        std::sync::Arc<std::sync::Mutex<std::sync::Weak<crate::ssh::SharedSession>>>,
    pub(crate) cancel: watch::Sender<bool>,
}

impl Connection {
    pub fn disconnect(&self) {
        let _ = self.cancel.send(true);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.disconnect();
    }
}
