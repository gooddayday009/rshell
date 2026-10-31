//! Telnet character-mode transport (RFC 854/855, terminal type and window size).
use crate::{
    config::{Profile, Protocol},
    connection::{Command, Connection, Event, Terminal},
};
use anyhow::{Context, Result, ensure};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, watch},
};

const IAC: u8 = 255;
const WILL: u8 = 251;
const WONT: u8 = 252;
const DO: u8 = 253;
const DONT: u8 = 254;
const SB: u8 = 250;
const SE: u8 = 240;

#[derive(Default)]
enum State {
    #[default]
    Data,
    Iac,
    Option(u8),
    Sub,
    SubIac,
}

struct Codec {
    state: State,
    local: [bool; 256],
    remote: [bool; 256],
    sub: Vec<u8>,
    size: (u16, u16),
    after_cr: bool,
}
impl Default for Codec {
    fn default() -> Self {
        Self {
            state: State::Data,
            local: [false; 256],
            remote: [false; 256],
            sub: Vec::new(),
            size: (80, 24),
            after_cr: false,
        }
    }
}
fn escaped(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        out.push(b);
        if b == IAC {
            out.push(b);
        }
    }
    out
}
impl Codec {
    fn window(&self) -> Vec<u8> {
        let mut out = vec![IAC, SB, 31];
        out.extend(escaped(
            &[self.size.0.to_be_bytes(), self.size.1.to_be_bytes()].concat(),
        ));
        out.extend([IAC, SE]);
        out
    }
    fn input(&self, data: &[u8]) -> Vec<u8> {
        if self.local[0] {
            return escaped(data);
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            out.push(b);
            if b == IAC {
                out.push(b);
            }
            if b == b'\r' {
                if data.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                out.push(b'\n');
            }
            i += 1;
        }
        out
    }
    fn receive(&mut self, bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut data = Vec::new();
        let mut reply = Vec::new();
        for &b in bytes {
            match self.state {
                State::Data if b == IAC => self.state = State::Iac,
                State::Data => {
                    if !(self.after_cr && b == 0 && !self.remote[0]) {
                        data.push(b);
                    }
                    self.after_cr = b == b'\r' && !self.remote[0];
                }
                State::Iac => match b {
                    IAC => {
                        data.push(IAC);
                        self.after_cr = false;
                        self.state = State::Data;
                    }
                    WILL | WONT | DO | DONT => self.state = State::Option(b),
                    SB => {
                        self.sub.clear();
                        self.state = State::Sub;
                    }
                    _ => self.state = State::Data,
                },
                State::Option(command) => {
                    let n = b as usize;
                    match command {
                        DO if matches!(b, 0 | 3 | 24 | 31) => {
                            if !self.local[n] {
                                self.local[n] = true;
                                reply.extend([IAC, WILL, b]);
                                if b == 31 {
                                    reply.extend(self.window());
                                }
                            }
                        }
                        DO => reply.extend([IAC, WONT, b]),
                        DONT => {
                            if self.local[n] {
                                reply.extend([IAC, WONT, b]);
                            }
                            self.local[n] = false;
                        }
                        WILL if matches!(b, 0 | 1 | 3) => {
                            if !self.remote[n] {
                                self.remote[n] = true;
                                reply.extend([IAC, DO, b]);
                            }
                        }
                        WILL => reply.extend([IAC, DONT, b]),
                        WONT => {
                            if self.remote[n] {
                                reply.extend([IAC, DONT, b]);
                            }
                            self.remote[n] = false;
                        }
                        _ => unreachable!(),
                    }
                    self.state = State::Data;
                }
                State::Sub if b == IAC => self.state = State::SubIac,
                State::Sub => self.sub.push(b),
                State::SubIac => match b {
                    IAC => {
                        self.sub.push(IAC);
                        self.state = State::Sub;
                    }
                    SE => {
                        if self.local[24] && self.sub == [24, 1] {
                            reply.extend([IAC, SB, 24, 0]);
                            reply.extend(b"xterm-256color");
                            reply.extend([IAC, SE]);
                        }
                        self.sub.clear();
                        self.state = State::Data;
                    }
                    _ => anyhow::bail!("无效的 Telnet 子协商"),
                },
            }
            ensure!(self.sub.len() <= 1024, "Telnet 子协商超过容量限制");
        }
        Ok((data, reply))
    }
}

pub fn connect(
    runtime: &tokio::runtime::Handle,
    profile: Profile,
    mut terminal: impl Terminal,
) -> Connection {
    let (commands, mut rx) = mpsc::channel(64);
    let (tx, events) = mpsc::channel(16);
    let (cancel, mut cancelled) = watch::channel(false);
    runtime.spawn(async move {
        let work = async {
            profile.validate()?;
            ensure!(profile.protocol == Protocol::Telnet, "不是 Telnet 会话");
            tx.send(Event::Status("Telnet 明文连接中…".into())).await?;
            let mut stream = tokio::time::timeout(
                Duration::from_secs(15),
                TcpStream::connect((profile.host.as_str(), profile.port)),
            )
            .await
            .context("Telnet 连接超时")?
            .context("Telnet 连接失败")?;
            stream.set_nodelay(true)?;
            tx.send(Event::Connected).await?;
            let mut codec = Codec::default();
            let mut buffer = [0u8; 8192];
            loop {
                let output = tokio::select! {
                    command = rx.recv() => match command {
                        Some(Command::Input(bytes)) => {
                            ensure!(bytes.len() <= 64*1024+16, "单次 Telnet 输入超过 64 KiB");
                            codec.input(&bytes)
                        }
                        Some(Command::Resize {cols, rows}) => {
                            terminal.resize(cols, rows); codec.size = (cols, rows);
                            if codec.local[31] { codec.window() } else { Vec::new() }
                        }
                        None => return Ok(()),
                    },
                    count = stream.read(&mut buffer) => {
                        let count = count.context("Telnet 读取失败")?;
                        if count == 0 { return Ok(()); }
                        let (data, mut reply) = codec.receive(&buffer[..count])?;
                        if !data.is_empty() { reply.extend(codec.input(&terminal.output(&data))); }
                        reply
                    }
                };
                if !output.is_empty() {
                    tokio::time::timeout(Duration::from_secs(10), stream.write_all(&output))
                        .await
                        .context("Telnet 写入超时")?
                        .context("Telnet 写入失败")?;
                }
            }
        };
        let result: Result<()> = tokio::select! {
            biased;
            _ = cancelled.changed() => Ok(()),
            result = work => result,
        };
        let _ = tx
            .send(Event::Finished(result.map_err(|e| format!("{e:#}"))))
            .await;
    });
    Connection {
        commands,
        events,
        cancel,
        ssh_session: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_mode_and_disable_negotiation() {
        let mut codec = Codec::default();
        let (_, reply) = codec.receive(&[IAC, DO, 0, IAC, WILL, 0]).unwrap();
        assert_eq!(reply, [IAC, WILL, 0, IAC, DO, 0]);
        assert_eq!(codec.input(&[b'\r', 0, IAC]), [b'\r', 0, IAC, IAC]);
        assert_eq!(codec.receive(&[b'\r', 0]).unwrap().0, [b'\r', 0]);
        let (_, reply) = codec.receive(&[IAC, DONT, 0, IAC, WONT, 0]).unwrap();
        assert_eq!(reply, [IAC, WONT, 0, IAC, DONT, 0]);
        assert!(
            codec
                .receive(&[IAC, DONT, 0, IAC, WONT, 0])
                .unwrap()
                .1
                .is_empty()
        );
        assert_eq!(codec.input(b"\r\n"), b"\r\n");
    }

    #[test]
    fn telnet_config_requires_endpoint_but_not_credentials() {
        let mut profile = Profile {
            protocol: Protocol::Telnet,
            host: "localhost".into(),
            port: 23,
            username: String::new(),
            ..Default::default()
        };
        profile.validate().unwrap();
        assert_eq!(profile.endpoint(), "telnet://localhost:23");
        profile.remember_secret = true;
        assert!(profile.validate().is_err());
        profile.remember_secret = false;
        profile.port = 0;
        assert!(profile.validate().is_err());
    }

    #[test]
    fn fragmented_negotiation_and_nvt() {
        let mut codec = Codec::default();
        let mut data = Vec::new();
        let mut reply = Vec::new();
        for b in [
            IAC, WILL, 1, IAC, DO, 31, IAC, DO, 24, IAC, SB, 24, 1, IAC, SE, b'a', b'\r', 0, IAC,
            IAC,
        ] {
            let (d, r) = codec.receive(&[b]).unwrap();
            data.extend(d);
            reply.extend(r);
        }
        assert_eq!(data, [b'a', b'\r', IAC]);
        assert!(reply.starts_with(&[IAC, DO, 1, IAC, WILL, 31]));
        assert!(
            reply
                .windows(b"xterm-256color\xff".len())
                .any(|s| s == b"xterm-256color\xff")
        );
        assert!(
            codec
                .receive(&[IAC, DO, 31, IAC, WILL, 1])
                .unwrap()
                .1
                .is_empty()
        );
        assert_eq!(codec.input(&[b'\r', IAC]), [b'\r', b'\n', IAC, IAC]);
        assert_eq!(codec.receive(&[IAC, DO, 99]).unwrap().1, [IAC, WONT, 99]);
        codec.receive(&[IAC, SB, 99]).unwrap();
        assert!(codec.receive(&[0; 1025]).is_err());
    }

    #[tokio::test]
    async fn loopback_io_resize_and_cancel() {
        use std::sync::{Arc, Mutex};
        struct Screen(Arc<Mutex<Vec<u8>>>);
        impl Terminal for Screen {
            fn output(&mut self, data: &[u8]) -> Vec<u8> {
                self.0.lock().unwrap().extend(data);
                Vec::new()
            }
            fn resize(&mut self, _: u16, _: u16) {}
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let screen = Arc::new(Mutex::new(Vec::new()));
            let mut connection = connect(
                &tokio::runtime::Handle::current(),
                Profile {
                    protocol: Protocol::Telnet,
                    host: "127.0.0.1".into(),
                    port: listener.local_addr().unwrap().port(),
                    ..Default::default()
                },
                Screen(screen.clone()),
            );
            let (mut peer, _) = listener.accept().await.unwrap();
            while !matches!(connection.events.recv().await, Some(Event::Connected)) {}
            peer.write_all(&[IAC, DO, 31]).await.unwrap();
            let mut response = [0; 12];
            peer.read_exact(&mut response).await.unwrap();
            assert_eq!(&response[..3], &[IAC, WILL, 31]);
            connection
                .commands
                .send(Command::Resize {
                    cols: 255,
                    rows: 40,
                })
                .await
                .unwrap();
            let mut size = [0; 10];
            peer.read_exact(&mut size).await.unwrap();
            assert_eq!(size, [IAC, SB, 31, 0, IAC, IAC, 0, 40, IAC, SE]);
            peer.write_all("登录：".as_bytes()).await.unwrap();
            connection
                .commands
                .send(Command::Input(b"demo\r".to_vec()))
                .await
                .unwrap();
            let mut login = [0; 6];
            peer.read_exact(&mut login).await.unwrap();
            assert_eq!(&login, b"demo\r\n");
            while screen.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
            connection.disconnect();
            assert!(matches!(
                connection.events.recv().await,
                Some(Event::Finished(Ok(())))
            ));
            assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
        })
        .await
        .unwrap();
    }
}
