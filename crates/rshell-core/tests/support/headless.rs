use rshell_core::config::Profile;
use russh::{
    Channel, ChannelId,
    server::{self, Session, VerifiedPublicKey},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
pub struct State {
    pub auth_calls: AtomicUsize,
    pub connections: AtomicUsize,
    pub commands: Mutex<Vec<Vec<u8>>>,
    pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub attributes: Arc<Mutex<HashMap<String, russh_sftp::protocol::FileAttributes>>>,
    pub disable_posix_rename: AtomicBool,
    pub fail_writes: AtomicBool,
}
struct Handler {
    state: Arc<State>,
    channels: HashMap<ChannelId, Channel<server::Msg>>,
    execs: HashMap<ChannelId, Vec<u8>>,
}
impl Drop for Handler {
    fn drop(&mut self) {
        self.state.connections.fetch_sub(1, Ordering::SeqCst);
    }
}
impl server::Handler for Handler {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        self.state.auth_calls.fetch_add(1, Ordering::SeqCst);
        Ok(if user == "tester" && password == "test-only-secret" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_publickey(
        &mut self,
        user: &str,
        _: VerifiedPublicKey<'_>,
    ) -> Result<server::Auth, Self::Error> {
        self.state.auth_calls.fetch_add(1, Ordering::SeqCst);
        Ok(if user == "tester" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        self.state.commands.lock().unwrap().push(command.to_vec());
        if command == b"reject" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        self.execs.insert(channel, command.to_vec());
        Ok(())
    }
    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(command) = self.execs.remove(&channel) {
            if command == b"hang" {
                return Ok(());
            }
            session.data(channel, "hello 你好\n".as_bytes())?;
            session.extended_data(channel, 1, b"remote error\n".to_vec())?;
            if command != b"missing-exit" {
                session.exit_status_request(channel, 7)?;
            }
            session.eof(channel)?;
            session.close(channel)?;
        }
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let channel = self.channels.remove(&channel).unwrap();
        let files = self.state.files.clone();
        let mut sftp = crate::support::Sftp::new(files);
        sftp.attributes = self.state.attributes.clone();
        sftp.disable_posix_rename = self.state.disable_posix_rename.load(Ordering::SeqCst);
        sftp.fail_writes = self.state.fail_writes.load(Ordering::SeqCst);
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), sftp).await;
        });
        Ok(())
    }
}

pub struct Server {
    pub profile: Profile,
    pub state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub async fn start() -> Server {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let state = Arc::new(State::default());
    let copy = state.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            let state = copy.clone();
            state.connections.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(
                    config,
                    stream,
                    Handler {
                        state,
                        channels: HashMap::new(),
                        execs: HashMap::new(),
                    },
                )
                .await
                {
                    let _ = session.await;
                }
            });
        }
    });
    Server {
        profile: Profile {
            name: "CLI loopback".into(),
            host: "127.0.0.1".into(),
            port,
            username: "tester".into(),
            ..Default::default()
        },
        state,
        task,
    }
}
