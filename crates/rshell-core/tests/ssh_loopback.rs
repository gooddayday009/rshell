mod support;
use rshell_core::{
    config::{AuthMethod, Config, KnownHost, Profile},
    ssh::{self, Command, Event},
};
use russh::{
    Channel, ChannelId, Pty,
    server::{self, Session, VerifiedPublicKey},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
struct State {
    reject_shell: AtomicBool,
    auth_calls: AtomicUsize,
    dropped: AtomicUsize,
    received: Mutex<Vec<u8>>,
    transferred: Mutex<Vec<u8>>,
    files: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    size: Mutex<(u32, u32)>,
}
struct Echo {
    state: Arc<State>,
    transfer: Option<Peer>,
    channels: std::collections::HashMap<ChannelId, Channel<server::Msg>>,
    sftp_channels: std::collections::HashSet<ChannelId>,
}
impl Drop for Echo {
    fn drop(&mut self) {
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl server::Handler for Echo {
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
        self.sftp_channels.insert(channel);
        let channel = self.channels.remove(&channel).unwrap();
        let files = self.state.files.clone();
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), support::Sftp::new(files)).await;
        });
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.state.size.lock().unwrap() = (cols, rows);
        session.channel_success(channel)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        if self.state.reject_shell.load(Ordering::SeqCst) {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        session.data(channel, "hello 你好\r\n".as_bytes())?;
        Ok(())
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.sftp_channels.contains(&channel) {
            return Ok(());
        }
        if data == b"OO" {
            return Ok(());
        }
        if data == b"sz-test\r" {
            let mut sender = zmodem2::Sender::new().unwrap();
            sender
                .start_file(zmodem2::FileInfo::new(
                    b"payload.bin",
                    Some(zmodem2::Position::new(payload().len() as u32)),
                ))
                .unwrap();
            self.transfer = Some(Peer::Send(sender));
        } else if data == b"rz-test\r" {
            self.transfer = Some(Peer::Receive(zmodem2::Receiver::new().unwrap()));
        }
        // The standard raw CAN sequence is independent of framed ZMODEM packets.
        if self.transfer.is_some() && data.windows(5).any(|part| part == [0x18; 5]) {
            self.transfer = None;
            session.data(channel, b"transfer-shell-ready\r\n".to_vec())?;
            return Ok(());
        }
        if let Some(peer) = &mut self.transfer {
            let initial = data == b"sz-test\r" || data == b"rz-test\r";
            if pump(
                peer,
                if initial { &[] } else { data },
                channel,
                session,
                &self.state,
            )? {
                self.transfer = None;
                session.data(channel, b"transfer-shell-ready\r\n".to_vec())?;
            }
            return Ok(());
        }
        self.state.received.lock().unwrap().extend_from_slice(data);
        if data == b"exit\r" {
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
        } else {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        _: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.state.size.lock().unwrap() = (cols, rows);
        Ok(())
    }
}

struct Sink(Arc<Mutex<Vec<u8>>>);
impl ssh::Terminal for Sink {
    fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        vec![]
    }
    fn resize(&mut self, _: u16, _: u16) {}
}

async fn server() -> (Profile, Config, Arc<State>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let fingerprint = key
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let state = Arc::new(State::default());
    let clone = state.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            let state = clone.clone();
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(
                    config,
                    stream,
                    Echo {
                        state,
                        transfer: None,
                        channels: Default::default(),
                        sftp_channels: Default::default(),
                    },
                )
                .await
                {
                    let _ = session.await;
                }
            });
        }
    });
    let profile = Profile {
        name: "local test".into(),
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        ..Default::default()
    };
    let mut trust = Config::default();
    trust.known_hosts.push(KnownHost {
        host: profile.host.clone(),
        port,
        fingerprint,
        algorithm: String::new(),
        public_key: String::new(),
        confirmed_at: None,
    });
    (profile, trust, state, task)
}

async fn next(connection: &mut ssh::Connection) -> Event {
    tokio::time::timeout(Duration::from_secs(10), connection.events.recv())
        .await
        .expect("event timeout")
        .expect("event channel closed")
}
async fn connected(connection: &mut ssh::Connection) {
    loop {
        match next(connection).await {
            Event::Connected => break,
            Event::Status(_) => {}
            Event::Finished(r) => panic!("connection failed: {r:?}"),
            _ => panic!("unexpected host prompt"),
        }
    }
}
async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn password_shell_resize_unicode_and_cancel_release_socket() {
    let (profile, config, state, server) = server().await;
    let output = Arc::new(Mutex::new(vec![]));
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(output.clone()),
    );
    connected(&mut connection).await;
    connection
        .commands
        .send(Command::Resize {
            cols: 123,
            rows: 37,
        })
        .await
        .unwrap();
    connection
        .commands
        .send(Command::Input("echo 中文\r".as_bytes().to_vec()))
        .await
        .unwrap();
    eventually(|| {
        *state.size.lock().unwrap() == (123, 37)
            && state
                .received
                .lock()
                .unwrap()
                .ends_with("echo 中文\r".as_bytes())
    })
    .await;
    eventually(|| output.lock().unwrap().ends_with("echo 中文\r".as_bytes())).await;
    connection.disconnect();
    loop {
        if let Event::Finished(result) = next(&mut connection).await {
            assert!(result.is_ok());
            break;
        }
    }
    eventually(|| state.dropped.load(Ordering::SeqCst) == 1).await;
    server.abort();
}

#[tokio::test]
async fn star_echo_is_immediate_and_partial_handshake_timeout_is_not_restarted() {
    let (profile, config, _, server) = server().await;
    let output = Arc::new(Mutex::new(vec![]));
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(output.clone()),
    );
    connected(&mut connection).await;
    eventually(|| {
        output
            .lock()
            .unwrap()
            .ends_with("hello 你好\r\n".as_bytes())
    })
    .await;
    output.lock().unwrap().clear();
    for expected in [b"*".as_slice(), b"**", b"***"] {
        connection
            .commands
            .send(Command::Input(b"*".to_vec()))
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_millis(500),
            eventually(|| *output.lock().unwrap() == expected),
        )
        .await
        .expect("ordinary star echo must not wait for ZMODEM timeout");
    }
    connection
        .commands
        .send(Command::Input(vec![0x18]))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !output.lock().unwrap().ends_with(&[0x18]) {
            connection
                .commands
                .send(Command::Resize { cols: 80, rows: 24 })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("resize traffic must not restart partial handshake timeout");
    assert_eq!(*output.lock().unwrap(), b"***\x18");
    connection.disconnect();
    server.abort();
}

#[tokio::test]
async fn unknown_host_rejection_prevents_authentication() {
    let (profile, _, state, server) = server().await;
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        Config::default(),
        Sink(Arc::default()),
    );
    loop {
        match next(&mut connection).await {
            Event::HostKey { reply, .. } => {
                assert_eq!(state.auth_calls.load(Ordering::SeqCst), 0);
                reply.send(false).unwrap();
            }
            Event::Finished(result) => {
                assert!(result.is_err());
                break;
            }
            _ => {}
        }
    }
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn changed_host_key_is_rejected_without_prompt_or_authentication() {
    let (profile, mut config, state, server) = server().await;
    config.known_hosts[0].fingerprint = "SHA256:changed".into();
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(Arc::default()),
    );
    loop {
        match next(&mut connection).await {
            Event::HostKey { .. } | Event::Connected => panic!("must reject changed host key"),
            Event::Finished(result) => {
                assert!(result.unwrap_err().contains("主机密钥已变化"));
                break;
            }
            _ => {}
        }
    }
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn wrong_password_fails_without_opening_shell() {
    let (profile, config, _, server) = server().await;
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "wrong".into(),
        config,
        Sink(Arc::default()),
    );
    loop {
        match next(&mut connection).await {
            Event::Connected => panic!("wrong password must not authenticate"),
            Event::Finished(result) => {
                assert!(result.unwrap_err().contains("认证失败"));
                break;
            }
            _ => {}
        }
    }
    server.abort();
}

#[tokio::test]
async fn encrypted_private_key_and_remote_close() {
    let (mut profile, config, _, server) = server().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("id_ed25519");
    let key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let encrypted = key.encrypt(&mut rand::rng(), "passphrase").unwrap();
    std::fs::write(
        &path,
        encrypted
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    profile.auth = AuthMethod::PrivateKey;
    profile.key_path = path.display().to_string();
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "passphrase".into(),
        config,
        Sink(Arc::default()),
    );
    connected(&mut connection).await;
    connection
        .commands
        .send(Command::Input(b"exit\r".to_vec()))
        .await
        .unwrap();
    loop {
        if let Event::Finished(result) = next(&mut connection).await {
            assert!(result.is_ok());
            break;
        }
    }
    server.abort();
}

#[tokio::test]
async fn ten_connections_do_not_mix_input_and_output() {
    let (profile, config, _, server) = server().await;
    let mut connections = Vec::new();
    for index in 0..10 {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut connection = ssh::connect(
            &tokio::runtime::Handle::current(),
            profile.clone(),
            "test-only-secret".into(),
            config.clone(),
            Sink(output.clone()),
        );
        connected(&mut connection).await;
        let marker = format!("connection-{index:02}-独立输入\r");
        connection
            .commands
            .send(Command::Input(marker.as_bytes().to_vec()))
            .await
            .unwrap();
        connections.push((connection, output, marker));
    }
    for (connection, output, marker) in &mut connections {
        eventually(|| output.lock().unwrap().ends_with(marker.as_bytes())).await;
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("独立输入").count(), 1);
        connection.disconnect();
    }
    server.abort();
}

#[tokio::test]
async fn cancel_while_waiting_for_host_confirmation_does_not_authenticate() {
    let (profile, _, state, server) = server().await;
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        Config::default(),
        Sink(Arc::default()),
    );
    let reply = loop {
        if let Event::HostKey { reply, .. } = next(&mut connection).await {
            break reply;
        }
    };
    connection.disconnect();
    loop {
        if let Event::Finished(_) = next(&mut connection).await {
            break;
        }
    }
    drop(reply);
    eventually(|| state.dropped.load(Ordering::SeqCst) == 1).await;
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn repeated_connect_close_releases_all_server_sessions() {
    let (profile, config, state, server) = server().await;
    for iteration in 1..=100 {
        let mut connection = ssh::connect(
            &tokio::runtime::Handle::current(),
            profile.clone(),
            "test-only-secret".into(),
            config.clone(),
            Sink(Arc::default()),
        );
        connected(&mut connection).await;
        drop(connection);
        eventually(|| state.dropped.load(Ordering::SeqCst) == iteration).await;
    }
    server.abort();
}

#[tokio::test]
async fn host_acceptance_is_durable_before_authentication() {
    let (profile, _, state, server) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.json");
    let store = rshell_core::store::Store::start(
        path.clone(),
        Config::default(),
        rshell_core::credentials::EncryptedCredentials(temp.path().join("credentials")),
        || {},
    )
    .unwrap();
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile.clone(),
        "test-only-secret".into(),
        Config::default(),
        Sink(Arc::default()),
    );
    loop {
        match next(&mut connection).await {
            Event::HostKey {
                reply,
                public_key,
                fingerprint,
                algorithm,
            } => {
                assert_eq!(state.auth_calls.load(Ordering::SeqCst), 0);
                let host = KnownHost::verified(
                    profile.host.clone(),
                    profile.port,
                    &public_key,
                    &fingerprint,
                )
                .unwrap();
                assert_eq!(host.algorithm, algorithm);
                store
                    .submit(rshell_core::store::Action::TrustHost { host, reply })
                    .unwrap();
            }
            Event::Connected => {
                let stored = Config::load(&path).unwrap();
                assert_eq!(stored.known_hosts.len(), 1);
                assert!(!stored.known_hosts[0].public_key.is_empty());
                assert!(stored.known_hosts[0].confirmed_at.is_some());
                break;
            }
            Event::Finished(r) => panic!("connection failed: {r:?}"),
            _ => {}
        }
    }
    connection.disconnect();
    server.abort();
}

#[tokio::test]
async fn duplicate_channels_share_authentication_and_close_independently() {
    let (profile, config, state, server) = server().await;
    let original_output = Arc::new(Mutex::new(vec![]));
    let mut original = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(original_output.clone()),
    );
    connected(&mut original).await;
    let copy_output = Arc::new(Mutex::new(vec![]));
    let mut copy = ssh::duplicate_channel(
        &tokio::runtime::Handle::current(),
        &original,
        Sink(copy_output.clone()),
    )
    .unwrap();
    connected(&mut copy).await;
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 1);
    copy.commands
        .send(Command::Input(b"only-copy".to_vec()))
        .await
        .unwrap();
    eventually(|| copy_output.lock().unwrap().ends_with(b"only-copy")).await;
    assert!(
        !original_output
            .lock()
            .unwrap()
            .windows(9)
            .any(|s| s == b"only-copy")
    );
    // Closing the original must not invalidate the shared transport.
    original.disconnect();
    loop {
        if let Event::Finished(result) = next(&mut original).await {
            result.unwrap();
            break;
        }
    }
    copy.commands
        .send(Command::Input(b"still-alive".to_vec()))
        .await
        .unwrap();
    eventually(|| copy_output.lock().unwrap().ends_with(b"still-alive")).await;
    let mut third = ssh::duplicate_channel(
        &tokio::runtime::Handle::current(),
        &copy,
        Sink(Arc::default()),
    )
    .unwrap();
    connected(&mut third).await;
    third.disconnect();
    loop {
        if let Event::Finished(result) = next(&mut third).await {
            result.unwrap();
            break;
        }
    }
    copy.commands
        .send(Command::Input(b"after-copy-close".to_vec()))
        .await
        .unwrap();
    eventually(|| copy_output.lock().unwrap().ends_with(b"after-copy-close")).await;
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 1);
    copy.disconnect();
    loop {
        if let Event::Finished(result) = next(&mut copy).await {
            result.unwrap();
            break;
        }
    }
    eventually(|| state.dropped.load(Ordering::SeqCst) == 1).await;
    assert!(
        ssh::duplicate_channel(
            &tokio::runtime::Handle::current(),
            &copy,
            Sink(Arc::default())
        )
        .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn refused_duplicate_shell_does_not_disconnect_original() {
    let (profile, config, state, server) = server().await;
    let output = Arc::new(Mutex::new(vec![]));
    let mut original = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(output.clone()),
    );
    connected(&mut original).await;
    state.reject_shell.store(true, Ordering::SeqCst);
    let mut copy = ssh::duplicate_channel(
        &tokio::runtime::Handle::current(),
        &original,
        Sink(Arc::default()),
    )
    .unwrap();
    loop {
        if let Event::Finished(result) = next(&mut copy).await {
            assert!(result.is_err());
            break;
        }
    }
    assert!(
        ssh::duplicate_channel(
            &tokio::runtime::Handle::current(),
            &copy,
            Sink(Arc::default())
        )
        .is_err()
    );
    original
        .commands
        .send(Command::Input(b"survived-rejection".to_vec()))
        .await
        .unwrap();
    eventually(|| output.lock().unwrap().ends_with(b"survived-rejection")).await;
    original.disconnect();
    loop {
        if let Event::Finished(result) = next(&mut original).await {
            result.unwrap();
            break;
        }
    }
    eventually(|| state.dropped.load(Ordering::SeqCst) == 1).await;
    server.abort();
}

#[tokio::test]
async fn idle_text_waits_for_network_silence_and_stops_on_disconnect() {
    let (mut profile, config, state, server) = server().await;
    profile.keep_alive.ssh_enabled = false;
    profile.keep_alive.tcp_enabled = true;
    profile.keep_alive.idle_enabled = true;
    profile.keep_alive.idle_interval_secs = 1;
    profile.keep_alive.idle_text = "保活".into();
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(Arc::new(Mutex::new(vec![]))),
    );
    connected(&mut connection).await;
    for _ in 0..4 {
        connection
            .commands
            .send(Command::Input(b"x".to_vec()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    assert_eq!(*state.received.lock().unwrap(), b"xxxx");
    eventually(|| state.received.lock().unwrap().ends_with("保活".as_bytes())).await;
    assert_eq!(*state.received.lock().unwrap(), "xxxx保活".as_bytes());
    connection.disconnect();
    eventually(|| state.dropped.load(Ordering::SeqCst) == 1).await;
    let snapshot = state.received.lock().unwrap().clone();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(*state.received.lock().unwrap(), snapshot);
    server.abort();
}

#[tokio::test]
async fn disabled_idle_text_is_never_sent() {
    let (mut profile, config, state, server) = server().await;
    profile.keep_alive.ssh_enabled = false;
    profile.keep_alive.idle_enabled = false;
    profile.keep_alive.idle_interval_secs = 1;
    profile.keep_alive.idle_text = "must-not-send".into();
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(Arc::new(Mutex::new(vec![]))),
    );
    connected(&mut connection).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(state.received.lock().unwrap().is_empty());
    connection.disconnect();
    eventually(|| state.dropped.load(Ordering::SeqCst) == 1).await;
    server.abort();
}

enum Peer {
    Send(zmodem2::Sender),
    Receive(zmodem2::Receiver),
}
fn payload() -> Vec<u8> {
    (0..131072).map(|i| (i % 256) as u8).collect()
}
fn pump(
    peer: &mut Peer,
    mut input: &[u8],
    channel: ChannelId,
    session: &mut Session,
    state: &State,
) -> Result<bool, russh::Error> {
    use zmodem2::{Action, Event};
    loop {
        let action = match peer {
            Peer::Send(s) => s.poll(),
            Peer::Receive(r) => r.poll(),
        };
        match action {
            Action::WriteWire(bytes) => {
                let n = bytes.len();
                session.data(channel, bytes.to_vec())?;
                match peer {
                    Peer::Send(s) => s.wire_written(n),
                    Peer::Receive(r) => r.wire_written(n),
                }
            }
            Action::ReadFile { offset, max_len } => {
                let bytes = payload();
                let start = offset.get() as usize;
                if let Peer::Send(s) = peer {
                    s.submit_file(&bytes[start..(start + max_len).min(bytes.len())])
                        .unwrap();
                }
            }
            Action::WriteFile(bytes) => {
                let n = bytes.len();
                state.transferred.lock().unwrap().extend_from_slice(bytes);
                if let Peer::Receive(r) = peer {
                    r.file_written(n).unwrap();
                }
            }
            Action::Event(Event::FileStarted(_)) => {}
            Action::Event(Event::FileCompleted) => {
                if let Peer::Send(s) = peer {
                    s.finish().unwrap();
                }
            }
            Action::Event(Event::SessionCompleted | Event::Aborted) => {
                loop {
                    let action = match peer {
                        Peer::Send(s) => s.poll(),
                        Peer::Receive(r) => r.poll(),
                    };
                    let Action::WriteWire(bytes) = action else {
                        break;
                    };
                    let n = bytes.len();
                    session.data(channel, bytes.to_vec())?;
                    match peer {
                        Peer::Send(s) => s.wire_written(n),
                        Peer::Receive(r) => r.wire_written(n),
                    }
                }
                return Ok(true);
            }
            Action::Idle => {
                if input.is_empty() {
                    return Ok(false);
                }
                let n = match peer {
                    Peer::Send(s) => s.submit_wire(input).unwrap(),
                    Peer::Receive(r) => r.submit_wire(input).unwrap(),
                };
                assert!(n > 0);
                input = &input[n..];
            }
            other => panic!("unexpected peer action {other:?}"),
        }
    }
}
#[tokio::test]
async fn zmodem_upload_download_binary_and_shell_recovery() {
    let (profile, config, state, server) = server().await;
    let output = Arc::new(Mutex::new(vec![]));
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(output.clone()),
    );
    connected(&mut connection).await;
    let folder = support::tempdir();
    let source = folder.path().join("upload.bin");
    tokio::fs::write(&source, payload()).await.unwrap();
    for upload in [false, true] {
        connection
            .commands
            .send(Command::Input(if upload {
                b"rz-test\r".to_vec()
            } else {
                b"sz-test\r".to_vec()
            }))
            .await
            .unwrap();
        loop {
            match next(&mut connection).await {
                Event::TransferPrompt {
                    upload: direction,
                    reply,
                } => {
                    assert_eq!(direction, upload);
                    reply
                        .send(Some(if upload {
                            source.clone()
                        } else {
                            folder.path().to_owned()
                        }))
                        .unwrap();
                }
                Event::Status(s) if s.contains("传输完成") => break,
                Event::Status(s) => assert!(!s.starts_with("ZMODEM："), "{s}"),
                Event::Finished(r) => panic!("transfer disconnected {r:?}"),
                _ => {}
            }
        }
        if upload {
            assert_eq!(*state.transferred.lock().unwrap(), payload());
        } else {
            assert_eq!(
                tokio::fs::read(folder.path().join("payload.bin"))
                    .await
                    .unwrap(),
                payload()
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let snapshot = output.lock().unwrap().clone();
        assert!(
            snapshot.ends_with(b"transfer-shell-ready\r\n"),
            "upload={upload} output={snapshot:?}"
        );
        output.lock().unwrap().clear();
    }
    connection
        .commands
        .send(Command::Input(b"echo-after\r".to_vec()))
        .await
        .unwrap();
    eventually(|| output.lock().unwrap().ends_with(b"echo-after\r")).await;
    connection.disconnect();
    server.abort();
}

#[tokio::test]
async fn sftp_list_binary_roundtrip_collision_cancel_and_session_reuse() {
    use rshell_core::transfer::{self, Outcome, Request};
    let (profile, config, state, server) = server().await;
    let output = Arc::new(Mutex::new(vec![]));
    let runtime = tokio::runtime::Handle::current();
    let mut connection = ssh::connect(
        &runtime,
        profile,
        "test-only-secret".into(),
        config,
        Sink(output.clone()),
    );
    connected(&mut connection).await;
    let directory = support::tempdir();
    let source = directory.path().join("source");
    tokio::fs::write(&source, payload()).await.unwrap();
    let mut upload = transfer::start(
        &runtime,
        &connection,
        Request::Upload {
            local: source.clone(),
            remote: "/binary".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        (&mut upload.result).await.unwrap().unwrap(),
        Outcome::Done
    ));
    assert_eq!(*upload.progress.borrow(), payload().len() as u64);
    assert_eq!(state.files.lock().unwrap()["/binary"], payload());
    let mut listing = transfer::start(&runtime, &connection, Request::List(".".into())).unwrap();
    let Outcome::Listed { entries, .. } = (&mut listing.result).await.unwrap().unwrap() else {
        panic!("expected listing")
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "binary");
    let destination = directory.path().join("download");
    let mut download = transfer::start(
        &runtime,
        &connection,
        Request::Download {
            remote: "/binary".into(),
            local: destination.clone(),
        },
    )
    .unwrap();
    (&mut download.result).await.unwrap().unwrap();
    assert_eq!(tokio::fs::read(destination).await.unwrap(), payload());
    let mut duplicate = transfer::start(
        &runtime,
        &connection,
        Request::Upload {
            local: source.clone(),
            remote: "/binary".into(),
        },
    )
    .unwrap();
    assert!((&mut duplicate.result).await.unwrap().is_err());
    assert_eq!(state.files.lock().unwrap()["/binary"], payload());
    let mut streaming = transfer::start(
        &runtime,
        &connection,
        Request::Upload {
            local: source,
            remote: "/slow".into(),
        },
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), streaming.progress.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*streaming.progress.borrow() > 0);
    streaming.cancel();
    assert!((&mut streaming.result).await.unwrap().is_err());
    assert!(!state.files.lock().unwrap().contains_key("/slow"));
    let mut cancelled = transfer::start(&runtime, &connection, Request::List(".".into())).unwrap();
    cancelled.cancel();
    assert!((&mut cancelled.result).await.unwrap().is_err());
    connection
        .commands
        .send(Command::Input(b"sftp-shell-alive\r".to_vec()))
        .await
        .unwrap();
    eventually(|| output.lock().unwrap().ends_with(b"sftp-shell-alive\r")).await;
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 1);
    connection.disconnect();
    server.abort();
}

#[tokio::test]
async fn zmodem_cancel_file_prompt_restores_shell_without_writing() {
    let (profile, config, _, server) = server().await;
    let output = Arc::new(Mutex::new(vec![]));
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "test-only-secret".into(),
        config,
        Sink(output.clone()),
    );
    connected(&mut connection).await;
    for command in [b"sz-test\r", b"rz-test\r"] {
        connection
            .commands
            .send(Command::Input(command.to_vec()))
            .await
            .unwrap();
        loop {
            match next(&mut connection).await {
                Event::TransferPrompt { reply, .. } => {
                    reply.send(None).unwrap();
                }
                Event::Status(s) if s.contains("用户取消") => break,
                Event::Finished(result) => panic!("unexpected disconnect {result:?}"),
                _ => {}
            }
        }
        eventually(|| {
            output
                .lock()
                .unwrap()
                .windows(b"transfer-shell-ready".len())
                .any(|w| w == b"transfer-shell-ready")
        })
        .await;
        output.lock().unwrap().clear();
    }
    connection
        .commands
        .send(Command::Input(b"cancel-shell-alive\r".to_vec()))
        .await
        .unwrap();
    eventually(|| output.lock().unwrap().ends_with(b"cancel-shell-alive\r")).await;
    connection.disconnect();
    server.abort();
}

#[cfg(windows)]
#[tokio::test]
async fn managed_private_key_authentication() {
    let (mut profile, config, _, server) = server().await;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/key-tests");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let key = rshell_core::user_keys::generate("loopback", None).unwrap();
    let path = rshell_core::user_keys::save(dir.path(), &key, "loopback").unwrap();
    profile.auth = AuthMethod::PrivateKey;
    profile.key_path = path.display().to_string();
    let mut connection = ssh::connect(
        &tokio::runtime::Handle::current(),
        profile,
        "".into(),
        config,
        Sink(Arc::default()),
    );
    connected(&mut connection).await;
    connection
        .commands
        .send(Command::Input(b"exit\r".to_vec()))
        .await
        .unwrap();
    loop {
        if let Event::Finished(result) = next(&mut connection).await {
            assert!(result.is_ok());
            break;
        }
    }
    server.abort();
}

#[tokio::test]
async fn sftp_resume_upload_download_tree_and_changed_source() {
    use rshell_core::transfer::{self, Request, Resume};
    let (profile, config, state, server) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let mut connection = ssh::connect(
        &runtime,
        profile,
        "test-only-secret".into(),
        config,
        Sink(Arc::new(Mutex::new(vec![]))),
    );
    connected(&mut connection).await;
    let dir = support::tempdir();
    let source = dir.path().join("source");
    let bytes = vec![37_u8; 1024 * 1024];
    tokio::fs::write(&source, &bytes).await.unwrap();
    let resume = Resume::default();
    let request = Request::Upload {
        local: source.clone(),
        remote: "/slow-resume".into(),
    };
    let mut job =
        transfer::start_resumable(&runtime, &connection, request.clone(), resume.clone()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), job.progress.changed())
        .await
        .unwrap()
        .unwrap();
    while *job.progress.borrow() == 0 {
        job.progress.changed().await.unwrap();
    }
    job.cancel();
    assert!((&mut job.result).await.unwrap().is_err());
    let partial_len = state
        .files
        .lock()
        .unwrap()
        .iter()
        .find(|(name, _)| name.starts_with("/slow-resume.rshell-"))
        .unwrap()
        .1
        .len();
    assert!(partial_len > 0 && partial_len < bytes.len());
    let mut job =
        transfer::start_resumable(&runtime, &connection, request.clone(), resume.clone()).unwrap();
    (&mut job.result).await.unwrap().unwrap();
    assert_eq!(state.files.lock().unwrap()["/slow-resume"], bytes);
    assert_eq!(*job.progress.borrow(), bytes.len() as u64);
    // Completed files are checked, not overwritten or blindly skipped.
    tokio::fs::write(&source, vec![99_u8; bytes.len()])
        .await
        .unwrap();
    let mut job = transfer::start_resumable(&runtime, &connection, request, resume).unwrap();
    assert!((&mut job.result).await.unwrap().is_err());
    assert_eq!(state.files.lock().unwrap()["/slow-resume"], bytes);
    // Pause a real download, then reuse the same partial file and validate bytes.
    state
        .files
        .lock()
        .unwrap()
        .insert("/slow-download".into(), bytes.clone());
    let target = dir.path().join("download-resumed");
    let resume = Resume::default();
    let request = Request::Download {
        remote: "/slow-download".into(),
        local: target.clone(),
    };
    let mut job =
        transfer::start_resumable(&runtime, &connection, request.clone(), resume.clone()).unwrap();
    while *job.progress.borrow() == 0 {
        tokio::time::timeout(Duration::from_secs(10), job.progress.changed())
            .await
            .unwrap()
            .unwrap();
    }
    job.cancel();
    assert!((&mut job.result).await.unwrap().is_err());
    assert!(!target.exists());
    // A changed prefix must fail without publishing or discarding the partial.
    state
        .files
        .lock()
        .unwrap()
        .get_mut("/slow-download")
        .unwrap()[0] = 98;
    let mut job =
        transfer::start_resumable(&runtime, &connection, request.clone(), resume.clone()).unwrap();
    assert!((&mut job.result).await.unwrap().is_err());
    assert!(!target.exists());
    state
        .files
        .lock()
        .unwrap()
        .get_mut("/slow-download")
        .unwrap()[0] = 37;
    let mut job = transfer::start_resumable(&runtime, &connection, request, resume).unwrap();
    (&mut job.result).await.unwrap().unwrap();
    assert_eq!(tokio::fs::read(target).await.unwrap(), bytes);
    let tree = dir.path().join("tree");
    tokio::fs::create_dir_all(tree.join("nested/empty"))
        .await
        .unwrap();
    tokio::fs::write(tree.join("nested/data"), &bytes[..1024])
        .await
        .unwrap();
    let resume = Resume::default();
    let request = Request::Upload {
        local: tree,
        remote: "/tree".into(),
    };
    for _ in 0..2 {
        let mut job =
            transfer::start_resumable(&runtime, &connection, request.clone(), resume.clone())
                .unwrap();
        (&mut job.result).await.unwrap().unwrap();
    }
    let target = dir.path().join("download-tree");
    let resume = Resume::default();
    let request = Request::Download {
        remote: "/tree".into(),
        local: target.clone(),
    };
    for _ in 0..2 {
        let mut job =
            transfer::start_resumable(&runtime, &connection, request.clone(), resume.clone())
                .unwrap();
        (&mut job.result).await.unwrap().unwrap();
    }
    assert_eq!(
        tokio::fs::read(target.join("nested/data")).await.unwrap(),
        bytes[..1024]
    );
    assert!(target.join("nested/empty").is_dir());
    // Independent tasks cannot overwrite a completed target.
    let mut job =
        transfer::start_resumable(&runtime, &connection, request, Resume::default()).unwrap();
    assert!((&mut job.result).await.unwrap().is_err());
    connection.disconnect();
    server.abort();
}
