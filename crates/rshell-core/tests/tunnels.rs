//! Real SSH/TCP over loopback. This is not Linux/OpenSSH acceptance.
use rshell_core::{
    config::{Config, KnownHost, Profile},
    ssh::{self, Event},
    tunnel::{Kind, Rule},
};
use russh::{
    Channel,
    server::{self, Session},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};

#[derive(Default)]
struct State {
    deny_remote: AtomicBool,
    allow_shell: AtomicBool,
    requested_hosts: Mutex<Vec<String>>,
    handle: Mutex<Option<server::Handle>>,
}

struct Peer {
    state: Arc<State>,
    listeners: HashMap<(String, u32), JoinHandle<()>>,
    streams: JoinSet<()>,
    shells: Vec<Channel<server::Msg>>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        for task in self.listeners.values() {
            task.abort();
        }
    }
}
impl server::Handler for Peer {
    type Error = anyhow::Error;
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        session: &mut Session,
    ) -> anyhow::Result<()> {
        if self.state.allow_shell.load(Ordering::SeqCst) {
            *self.state.handle.lock().unwrap() = Some(session.handle());
            self.shells.push(channel);
            reply.accept().await;
        }
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: russh::ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> anyhow::Result<()> {
        session.channel_success(channel)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: russh::ChannelId,
        session: &mut Session,
    ) -> anyhow::Result<()> {
        session.channel_success(channel)?;
        Ok(())
    }
    async fn auth_password(&mut self, user: &str, password: &str) -> anyhow::Result<server::Auth> {
        Ok(if user == "tunnel-test" && password == "fixture-only" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> anyhow::Result<()> {
        self.state.requested_hosts.lock().unwrap().push(host.into());
        if !matches!(host, "127.0.0.1" | "::1" | "loopback.test") {
            return Ok(());
        }
        let host = if host == "loopback.test" {
            "127.0.0.1"
        } else {
            host
        }
        .to_owned();
        self.streams.spawn(async move {
            if let Ok(mut tcp) = TcpStream::connect((host.as_str(), port as u16)).await {
                reply.accept().await;
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut channel.into_stream()).await;
            }
        });
        Ok(())
    }
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> anyhow::Result<bool> {
        *self.state.handle.lock().unwrap() = Some(session.handle());
        if self.state.deny_remote.load(Ordering::SeqCst) || address != "127.0.0.1" {
            return Ok(false);
        }
        let Ok(listener) = TcpListener::bind((address, *port as u16)).await else {
            return Ok(false);
        };
        let handle = session.handle();
        let address = address.to_owned();
        let key = (address.clone(), *port);
        let listen_port = *port;
        let task = tokio::spawn(async move {
            let mut streams = JoinSet::new();
            loop {
                tokio::select! {
                    Some(_) = streams.join_next(), if !streams.is_empty() => {}
                    result = listener.accept() => {
                        let Ok((mut tcp, peer)) = result else { break; };
                        let handle = handle.clone();
                        let address = address.clone();
                        streams.spawn(async move {
                            if let Ok(channel) = handle.channel_open_forwarded_tcpip(address, listen_port, peer.ip().to_string(), u32::from(peer.port())).await {
                                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut channel.into_stream()).await;
                            }
                        });
                    }
                }
            }
        });
        self.listeners.insert(key, task);
        Ok(true)
    }
    async fn cancel_tcpip_forward(
        &mut self,
        address: &str,
        port: u32,
        _: &mut Session,
    ) -> anyhow::Result<bool> {
        if let Some(task) = self.listeners.remove(&(address.into(), port)) {
            task.abort();
            return Ok(true);
        }
        Ok(false)
    }
    // Shell requests are rejected unless a test explicitly opts in.
}

struct Fixture {
    profile: Profile,
    config: Config,
    state: Arc<State>,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(rules: Vec<Rule>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let key =
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
                .unwrap();
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
            let mut sessions = JoinSet::new();
            while let Ok((stream, _)) = listener.accept().await {
                let config = config.clone();
                let state = clone.clone();
                sessions.spawn(async move {
                    if let Ok(session) = server::run_stream(
                        config,
                        stream,
                        Peer {
                            state,
                            listeners: HashMap::new(),
                            streams: JoinSet::new(),
                            shells: Vec::new(),
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
            name: "Tunnel fixture".into(),
            host: "127.0.0.1".into(),
            port,
            username: "tunnel-test".into(),
            tunnels: rules,
            tunnel_only: true,
            ..Default::default()
        };
        let mut config = Config::default();
        config.known_hosts.push(KnownHost {
            host: profile.host.clone(),
            port,
            fingerprint,
            algorithm: String::new(),
            public_key: String::new(),
            confirmed_at: None,
        });
        Self {
            profile,
            config,
            state,
            task,
        }
    }
    fn connect(&self) -> ssh::Connection {
        ssh::connect(
            &tokio::runtime::Handle::current(),
            self.profile.clone(),
            "fixture-only".into(),
            self.config.clone(),
            Sink,
        )
    }
}
struct Sink;
impl ssh::Terminal for Sink {
    fn output(&mut self, _: &[u8]) -> Vec<u8> {
        Vec::new()
    }
    fn resize(&mut self, _: u16, _: u16) {}
}
async fn next(connection: &mut ssh::Connection) -> Event {
    tokio::time::timeout(Duration::from_secs(10), connection.events.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn connected(connection: &mut ssh::Connection) {
    let mut listeners_ready = false;
    loop {
        match next(connection).await {
            Event::Tunnels(status) => {
                assert!(status.iter().all(|s| s == "监听中"));
                listeners_ready = true;
            }
            Event::Connected => {
                assert!(listeners_ready);
                break;
            }
            Event::Finished(result) => panic!("connection failed: {result:?}"),
            Event::HostKey { .. } => panic!("fixture host should be trusted"),
            _ => {}
        }
    }
}
async fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
async fn echo() -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        while let Ok((mut socket, _)) = listener.accept().await {
            tasks.spawn(async move {
                // Only respond after FIN: verifies half-close and complete binary payloads.
                let mut bytes = Vec::new();
                socket.read_to_end(&mut bytes).await.unwrap();
                let _ = socket.write_all(&bytes).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (port, task)
}
async fn roundtrip(mut socket: TcpStream) {
    let bytes: Vec<_> = (0..131_072).map(|i| (i % 251) as u8).collect();
    socket.write_all(&bytes).await.unwrap();
    socket.shutdown().await.unwrap();
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, bytes);
}
async fn released(port: u16) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn local_remote_binary_half_close_disconnect_and_reconnect() {
    let (target, echo) = echo().await;
    let local = free_port().await;
    let remote = free_port().await;
    let fixture = Fixture::new(vec![
        Rule {
            bind_port: local,
            target_port: target,
            ..Default::default()
        },
        Rule {
            kind: Kind::Remote,
            bind_port: remote,
            target_port: target,
            ..Default::default()
        },
    ])
    .await;
    for _ in 0..2 {
        let mut connection = fixture.connect();
        connected(&mut connection).await;
        let first = roundtrip(TcpStream::connect(("127.0.0.1", local)).await.unwrap());
        let second = roundtrip(TcpStream::connect(("127.0.0.1", remote)).await.unwrap());
        tokio::join!(first, second);
        // An active socket also closes when the connection is dropped.
        let mut active = TcpStream::connect(("127.0.0.1", local)).await.unwrap();
        drop(connection);
        released(local).await;
        released(remote).await;
        let mut byte = [0];
        let result = tokio::time::timeout(Duration::from_secs(5), active.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(result, Ok(0) | Err(_)));
    }
    echo.abort();
}

async fn socks(port: u16) -> TcpStream {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    socket.write_all(&[5, 1, 0]).await.unwrap();
    let mut reply = [0; 2];
    socket.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply, [5, 0]);
    socket
}
#[tokio::test]
async fn socks_domain_dns_at_server_ipv4_and_protocol_rejections() {
    let (target, echo) = echo().await;
    let port = free_port().await;
    let fixture = Fixture::new(vec![Rule {
        kind: Kind::Dynamic,
        bind_port: port,
        ..Default::default()
    }])
    .await;
    let mut connection = fixture.connect();
    connected(&mut connection).await;
    for address in [
        vec![1, 127, 0, 0, 1],
        [vec![3, 13], b"loopback.test".to_vec()].concat(),
    ] {
        let mut socket = socks(port).await;
        let mut request = vec![5, 1, 0];
        request.extend(address);
        request.extend(target.to_be_bytes());
        // Fragment the request to ensure the parser does not assume packet boundaries.
        for byte in request {
            socket.write_all(&[byte]).await.unwrap();
        }
        let mut reply = [0; 10];
        socket.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);
        roundtrip(socket).await;
    }
    assert!(
        fixture
            .state
            .requested_hosts
            .lock()
            .unwrap()
            .contains(&"loopback.test".into())
    );
    for (request, code) in [
        (vec![5, 3, 0, 1], 7),
        (vec![5, 1, 0, 9], 8),
        (vec![5, 1, 1, 1], 1),
    ] {
        let mut socket = socks(port).await;
        socket.write_all(&request).await.unwrap();
        let mut reply = [0; 10];
        socket.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], code);
    }
    let mut no_auth = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    no_auth.write_all(&[5, 1, 2]).await.unwrap();
    let mut reply = [0; 2];
    no_auth.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply, [5, 255]);
    connection.disconnect();
    released(port).await;
    echo.abort();
}

#[tokio::test]
async fn bind_conflict_and_remote_denial_roll_back_all_listeners() {
    for remote_denial in [false, true] {
        let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let busy = occupied.local_addr().unwrap().port();
        let local = free_port().await;
        let fixture = Fixture::new(vec![
            Rule {
                bind_port: local,
                ..Default::default()
            },
            Rule {
                kind: if remote_denial {
                    Kind::Remote
                } else {
                    Kind::Local
                },
                bind_port: busy,
                ..Default::default()
            },
        ])
        .await;
        fixture
            .state
            .deny_remote
            .store(remote_denial, Ordering::SeqCst);
        let mut connection = fixture.connect();
        loop {
            match next(&mut connection).await {
                Event::Connected => panic!("partially configured tunnels must not report ready"),
                Event::Finished(result) => {
                    assert!(result.unwrap_err().contains("隧道 2 启动失败"));
                    break;
                }
                _ => {}
            }
        }
        released(local).await;
    }
}

#[tokio::test]
async fn unsolicited_remote_channel_is_rejected_and_target_failure_is_visible() {
    let remote = free_port().await;
    let unavailable = free_port().await;
    let fixture = Fixture::new(vec![Rule {
        kind: Kind::Remote,
        bind_port: remote,
        target_port: unavailable,
        ..Default::default()
    }])
    .await;
    let mut connection = fixture.connect();
    connected(&mut connection).await;
    let handle = fixture.state.handle.lock().unwrap().clone().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        handle.channel_open_forwarded_tcpip("127.0.0.1", u32::from(unavailable), "127.0.0.1", 1),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    let _socket = TcpStream::connect(("127.0.0.1", remote)).await.unwrap();
    loop {
        match next(&mut connection).await {
            Event::Tunnels(statuses) => {
                assert!(statuses[0].contains("最近连接失败"));
                break;
            }
            Event::Finished(result) => {
                panic!("target failure must not terminate listener: {result:?}")
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn shell_channel_duplicate_survives_while_owner_tunnels_are_removed() {
    let local = free_port().await;
    let remote = free_port().await;
    let mut fixture = Fixture::new(vec![
        Rule {
            bind_port: local,
            ..Default::default()
        },
        Rule {
            kind: Kind::Remote,
            bind_port: remote,
            ..Default::default()
        },
    ])
    .await;
    fixture.profile.tunnel_only = false;
    fixture.state.allow_shell.store(true, Ordering::SeqCst);
    let mut original = fixture.connect();
    connected(&mut original).await;
    let mut duplicate =
        ssh::duplicate_channel(&tokio::runtime::Handle::current(), &original, Sink).unwrap();
    loop {
        match next(&mut duplicate).await {
            Event::Connected => break,
            Event::Finished(result) => panic!("duplicate failed: {result:?}"),
            _ => {}
        }
    }
    original.disconnect();
    loop {
        if let Event::Finished(result) = next(&mut original).await {
            result.unwrap();
            break;
        }
    }
    released(local).await;
    released(remote).await;
    // The shared transport remains usable after owner cancellation and remote-listener cleanup.
    let mut third =
        ssh::duplicate_channel(&tokio::runtime::Handle::current(), &duplicate, Sink).unwrap();
    loop {
        match next(&mut third).await {
            Event::Connected => break,
            Event::Finished(result) => panic!("shared transport was disconnected: {result:?}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn authentication_and_changed_host_key_never_open_listeners() {
    for changed_key in [false, true] {
        let local = free_port().await;
        let mut fixture = Fixture::new(vec![Rule {
            bind_port: local,
            ..Default::default()
        }])
        .await;
        if changed_key {
            fixture.config.known_hosts[0].fingerprint = "wrong-fingerprint".into();
        } else {
            fixture.profile.username = "wrong-user".into();
        }
        let mut connection = fixture.connect();
        loop {
            match next(&mut connection).await {
                Event::Connected | Event::Tunnels(_) => {
                    panic!("must not start tunnels before authentication")
                }
                Event::Finished(result) => {
                    assert!(result.is_err());
                    break;
                }
                _ => {}
            }
        }
        assert!(TcpStream::connect(("127.0.0.1", local)).await.is_err());
    }
}

#[tokio::test]
async fn remote_disconnect_stops_local_listener_and_socks_target_denial_is_not_success() {
    let port = free_port().await;
    let remote = free_port().await;
    let fixture = Fixture::new(vec![
        Rule {
            kind: Kind::Dynamic,
            bind_port: port,
            ..Default::default()
        },
        Rule {
            kind: Kind::Remote,
            bind_port: remote,
            ..Default::default()
        },
    ])
    .await;
    let mut connection = fixture.connect();
    connected(&mut connection).await;
    let mut socket = socks(port).await;
    let host = b"denied.test";
    let request = [
        vec![5, 1, 0, 3, host.len() as u8],
        host.to_vec(),
        vec![0, 80],
    ]
    .concat();
    socket.write_all(&request).await.unwrap();
    let mut reply = [0; 10];
    socket.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 1);
    let handle = fixture.state.handle.lock().unwrap().clone().unwrap();
    handle
        .disconnect(
            russh::Disconnect::ByApplication,
            "test disconnect".into(),
            "en".into(),
        )
        .await
        .unwrap();
    loop {
        if let Event::Finished(result) = next(&mut connection).await {
            assert!(result.is_err());
            break;
        }
    }
    released(port).await;
    released(remote).await;
}
