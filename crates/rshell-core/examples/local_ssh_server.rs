//! Manual GUI test fixture. Loopback only; never executes system commands.
use anyhow::{Context, Result, bail};
use russh::{
    Channel, ChannelId, Pty,
    keys::{Algorithm, HashAlg, PrivateKey, load_secret_key, ssh_key::LineEnding},
    server::{self, Server as _, Session},
};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(2222);
    if port == 0 {
        bail!("Specify a port from 1 to 65535");
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let key_path = root.join("target/local-ssh/host_ed25519");
    std::fs::create_dir_all(key_path.parent().unwrap())?;
    let key = if key_path.exists() {
        load_secret_key(&key_path, None).context("Unable to read the existing TEST host key")?
    } else {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let pem = key.to_openssh(LineEnding::LF)?;
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&key_path)?
            .write_all(pem.as_bytes())?;
        key
    };
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256);
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(200),
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(Duration::from_secs(3600)),
        ..Default::default()
    });
    let socket = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .context("Could not bind loopback port; stop this test server or select another port")?;
    println!("RShell LOCAL TEST server: 127.0.0.1:{port}");
    println!("Username: demo    Test password: rshell-demo");
    println!("Host fingerprint: {fingerprint}");
    println!("This fixture does NOT execute OS commands. Press Ctrl+C here to stop.");
    let mut server = EchoServer;
    server.run_on_socket(config, &socket).await?;
    Ok(())
}

struct EchoServer;
impl server::Server for EchoServer {
    type Handler = Echo;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Echo {
        Echo { line: vec![] }
    }
}

struct Echo {
    line: Vec<u8>,
}
impl server::Handler for Echo {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == "demo" && password == "rshell-demo" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(
            channel,
            concat!(
                "\x1b[36mRShell 本机 SSH 测试服务\x1b[0m\r\n",
                "这是回显测试，不执行操作系统命令。\r\n",
                "输入 help / colors / clear / exit，或输入任意文本。\r\n",
                "demo> "
            )
            .as_bytes(),
        )?;
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
        println!("Test terminal resized: {cols} x {rows}");
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        for &byte in data {
            match byte {
                b'\r' | b'\n' => {
                    let line = String::from_utf8_lossy(&self.line).trim().to_owned();
                    self.line.clear();
                    session.data(channel, b"\r\n".as_slice())?;
                    match line.as_str() {
                        "exit" => {
                            session.data(channel, b"Bye!\r\n".as_slice())?;
                            session.exit_status_request(channel, 0)?;
                            session.eof(channel)?;
                            session.close(channel)?;
                            return Ok(());
                        }
                        "help" => {
                            session.data(channel, "help: 本帮助\r\ncolors: 颜色测试\r\nclear: 清屏\r\nexit: 断开连接\r\n其他文本只回显，不执行。\r\n".as_bytes())?;
                        }
                        "colors" => {
                            session.data(channel, "\x1b[31m红色\x1b[0m \x1b[32m绿色\x1b[0m \x1b[34m蓝色\x1b[0m \x1b[1m粗体\x1b[0m\r\n".as_bytes())?;
                        }
                        "clear" => {
                            session.data(channel, b"\x1b[2J\x1b[H".as_slice())?;
                        }
                        "" => {}
                        _ => {
                            session.data(channel, format!("回显：{line}\r\n").into_bytes())?;
                        }
                    }
                    session.data(channel, b"demo> ".as_slice())?;
                }
                3 => {
                    self.line.clear();
                    session.data(channel, b"^C\r\ndemo> ".as_slice())?;
                }
                4 => {
                    session.eof(channel)?;
                    session.close(channel)?;
                    return Ok(());
                }
                8 | 127 => {
                    if !self.line.is_empty() {
                        let mut start = self.line.len() - 1;
                        while start > 0 && (self.line[start] & 0xc0) == 0x80 {
                            start -= 1;
                        }
                        let wide = self.line[start] >= 0xe0;
                        self.line.truncate(start);
                        session.data(
                            channel,
                            if wide {
                                b"\x08 \x08\x08 \x08".as_slice()
                            } else {
                                b"\x08 \x08".as_slice()
                            },
                        )?;
                    }
                }
                0..=31 => {} // Do not execute terminal-control input as commands.
                _ if self.line.len() < 8192 => {
                    self.line.push(byte);
                    session.data(channel, vec![byte])?;
                }
                _ => {
                    self.line.clear();
                    session.data(channel, b"\r\nInput limit reached.\r\ndemo> ".as_slice())?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshell_core::{
        config::{Config, KnownHost, Profile},
        ssh::{self, Command, Event},
    };
    use std::sync::Mutex;

    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl ssh::Terminal for Capture {
        fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            vec![]
        }
        fn resize(&mut self, _: u16, _: u16) {}
    }

    #[tokio::test]
    async fn manual_fixture_supports_documented_login_and_commands() {
        let socket = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = socket.local_addr().unwrap().port();
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let server = tokio::spawn(async move {
            EchoServer
                .run_on_socket(
                    Arc::new(server::Config {
                        keys: vec![key],
                        ..Default::default()
                    }),
                    &socket,
                )
                .await
                .unwrap();
        });
        let mut config = Config::default();
        config.known_hosts.push(KnownHost {
            host: "127.0.0.1".into(),
            port,
            fingerprint,
            algorithm: String::new(),
            public_key: String::new(),
            confirmed_at: None,
        });
        let profile = Profile {
            host: "127.0.0.1".into(),
            port,
            username: "demo".into(),
            ..Default::default()
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut connection = ssh::connect(
            &tokio::runtime::Handle::current(),
            profile,
            "rshell-demo".into(),
            config,
            Capture(output.clone()),
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match connection.events.recv().await.unwrap() {
                    Event::Connected => break,
                    Event::Finished(result) => panic!("test fixture login failed: {result:?}"),
                    _ => {}
                }
            }
            connection
                .commands
                .send(Command::Input(
                    "help\rcolors\r中文\rexit\r".as_bytes().to_vec(),
                ))
                .await
                .unwrap();
            loop {
                if let Event::Finished(result) = connection.events.recv().await.unwrap() {
                    result.unwrap();
                    break;
                }
            }
        })
        .await
        .unwrap();
        let data = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(data.contains("本机 SSH 测试服务"));
        assert!(data.contains("help: 本帮助"));
        assert!(data.contains("\x1b[31m红色"));
        assert!(data.contains("回显：中文"));
        assert!(data.contains("Bye!"));
        server.abort();
    }
}
