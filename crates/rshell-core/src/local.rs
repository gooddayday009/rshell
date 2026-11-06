//! Native PTY session. All pipe operations run outside the UI thread.
use crate::connection::{Command, Connection, Event, Terminal};
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{
    io::{Read, Write},
    sync::mpsc as blocking,
    time::Duration,
};
use tokio::sync::{mpsc, watch};

pub fn connect(runtime: &tokio::runtime::Handle, terminal: impl Terminal) -> Connection {
    let (commands, rx) = mpsc::channel(64);
    let (tx, events) = mpsc::channel(16);
    let (cancel, cancelled) = watch::channel(false);
    runtime.spawn_blocking(move || {
        let result = run(terminal, rx, cancelled, &tx);
        let _ = tx.blocking_send(Event::Finished(result.map_err(|e| format!("{e:#}"))));
    });
    Connection {
        commands,
        events,
        cancel,
        ssh_session: Default::default(),
    }
}

fn run(
    mut terminal: impl Terminal,
    mut commands: mpsc::Receiver<Command>,
    cancelled: watch::Receiver<bool>,
    events: &mpsc::Sender<Event>,
) -> Result<()> {
    if *cancelled.borrow() {
        return Ok(());
    }
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("创建本地 PTY 失败")?;
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let command = shell_command();
    let mut child = pair
        .slave
        .spawn_command(command)
        .context("启动本地 Shell 失败")?;
    drop(pair.slave);
    let (output_tx, output) = blocking::sync_channel(32);
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // During shutdown keep draining until ConPTY closes its pipe.
                    let _ = output_tx.send(buffer[..n].to_vec());
                }
            }
        }
    });
    // Pipe writes may block; a separate cancellation watcher can terminate the child.
    let mut killer = child.clone_killer();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher_stop = stop.clone();
    let loop_cancelled = cancelled.clone();
    let watcher = std::thread::spawn(move || {
        while !watcher_stop.load(std::sync::atomic::Ordering::Relaxed) {
            if *cancelled.borrow() || cancelled.has_changed().is_err() {
                let _ = killer.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let result = (|| -> Result<()> {
        events.blocking_send(Event::Status(format!(
            "{} 进程已启动，正在初始化命令行…（可切换或关闭标签）",
            shell_label()
        )))?;
        #[cfg(windows)]
        let mut initial_prompt = InitialPrompt::default();
        #[cfg(windows)]
        let mut ready = false;
        #[cfg(not(windows))]
        events.blocking_send(Event::Connected)?;
        loop {
            if *loop_cancelled.borrow() || loop_cancelled.has_changed().is_err() {
                return Ok(());
            }
            // Bound input processing so output remains responsive during pastes.
            for _ in 0..16 {
                match commands.try_recv() {
                    Ok(Command::Input(bytes)) => {
                        writer.write_all(&bytes)?;
                        writer.flush()?;
                    }
                    Ok(Command::Resize { cols, rows }) => {
                        pair.master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        })?;
                        terminal.resize(cols, rows);
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
                }
            }
            match output.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => {
                    let reply = terminal.output(&bytes);
                    if !reply.is_empty() {
                        writer.write_all(&reply)?;
                        writer.flush()?;
                    }
                    #[cfg(windows)]
                    if !ready && initial_prompt.feed(&bytes) {
                        ready = true;
                        events.blocking_send(Event::Connected)?;
                    }
                }
                Err(blocking::RecvTimeoutError::Disconnected) => return Ok(()),
                Err(blocking::RecvTimeoutError::Timeout) => {
                    if child.try_wait()?.is_some() {
                        return Ok(());
                    }
                }
            }
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = watcher.join();
    drop(writer);
    // Continue draining ConPTY output while ClosePseudoConsole runs.
    drop(output);
    drop(pair.master);
    let _ = reader_thread.join();
    result
}

/// Observe only the initial default prompt of powershell.exe -NoProfile.
/// This is a startup indicator, not a detector for later nested shells or command completion.
#[cfg(windows)]
#[derive(Default)]
struct InitialPrompt {
    prefix: usize,
    in_prompt: bool,
}
#[cfg(windows)]
impl InitialPrompt {
    fn feed(&mut self, bytes: &[u8]) -> bool {
        // Constant memory even for long paths or unexpected startup output.
        for &byte in bytes {
            if self.in_prompt {
                if byte == b'>' {
                    return true;
                }
            } else if byte == b"PS "[self.prefix] {
                self.prefix += 1;
                if self.prefix == 3 {
                    self.in_prompt = true;
                }
            } else {
                self.prefix = usize::from(byte == b'P');
            }
        }
        false
    }
}

pub fn shell_label() -> &'static str {
    if cfg!(windows) {
        "Windows PowerShell"
    } else {
        "macOS zsh"
    }
}
fn shell_command() -> CommandBuilder {
    #[cfg(windows)]
    {
        let mut command = CommandBuilder::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile"]);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = CommandBuilder::new("/bin/zsh");
        command.args(["-l", "-i"]);
        command.env("TERM", "xterm-256color");
        command
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct Capture(Arc<Mutex<Vec<u8>>>, rshell_terminal::Terminal);
    impl Terminal for Capture {
        fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
            let mut captured = self.0.lock().unwrap();
            captured.extend_from_slice(bytes);
            self.1.feed(bytes)
        }
        fn resize(&mut self, cols: u16, rows: u16) {
            self.1.resize(cols, rows);
        }
    }

    async fn until(output: &Arc<Mutex<Vec<u8>>>, text: &str) {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if String::from_utf8_lossy(&output.lock().unwrap()).contains(text) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "missing {text:?}: {}",
                String::from_utf8_lossy(&output.lock().unwrap())
            )
        });
    }
    async fn finished(connection: &mut Connection) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(Event::Finished(result)) = connection.events.recv().await {
                    result.unwrap();
                    break;
                }
            }
        })
        .await
        .expect("ConPTY session did not shut down");
    }
    #[test]
    fn initial_prompt_waits_for_split_prompt_and_bounds_output() {
        let mut prompt = InitialPrompt::default();
        assert!(!prompt.feed(b"\x1b[?25h\x1b[0m"));
        assert!(!prompt.feed(b"P"));
        assert!(!prompt.feed(b"S C:\\work"));
        assert!(prompt.feed(b"> "));
        let mut noisy = InitialPrompt::default();
        assert!(!noisy.feed(&vec![b'x'; 100_000]));
        assert!(!noisy.in_prompt);
        assert!(noisy.feed(b"\r\nPS C:\\work> "));
    }

    #[tokio::test]
    async fn connected_means_default_prompt_observed() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut connection = connect(
            &tokio::runtime::Handle::current(),
            Capture(output.clone(), rshell_terminal::Terminal::new(80, 24)),
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut saw_starting = false;
            loop {
                match connection.events.recv().await.unwrap() {
                    Event::Status(_) => saw_starting = true,
                    Event::Connected => {
                        assert!(saw_starting);
                        assert!(InitialPrompt::default().feed(&output.lock().unwrap()));
                        break;
                    }
                    Event::Finished(result) => panic!("startup failed: {result:?}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        connection.disconnect();
        finished(&mut connection).await;
    }

    #[tokio::test]
    async fn cancel_during_startup_finishes() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut connection = connect(
            &tokio::runtime::Handle::current(),
            Capture(output, rshell_terminal::Terminal::new(80, 24)),
        );
        connection.disconnect();
        finished(&mut connection).await;
    }

    #[tokio::test]
    async fn powershell_input_unicode_resize_and_exit() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut connection = connect(
            &tokio::runtime::Handle::current(),
            Capture(output.clone(), rshell_terminal::Terminal::new(80, 24)),
        );
        until(&output, "PS ").await;
        connection
            .commands
            .send(Command::Resize {
                cols: 120,
                rows: 36,
            })
            .await
            .unwrap();
        // Reproduce an English Windows console, independent of the host locale.
        connection
            .commands
            .send(Command::Input(
                concat!(
                    "[Console]::OutputEncoding = [Text.Encoding]::GetEncoding(437); ",
                    "[Console]::WriteLine(('CODEPAGE=' + [Console]::OutputEncoding.CodePage))\r"
                )
                .as_bytes()
                .to_vec(),
            ))
            .await
            .unwrap();
        until(&output, "CODEPAGE=437").await;
        // Console.WriteLine encodes through the console code page before ConPTY
        // receives it. Explicit UTF-8 keeps this transport test locale-independent.
        // Split the expected text so command echo cannot satisfy the assertion.
        connection
            .commands
            .send(Command::Input(
                concat!(
                    "[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); ",
                    "[Console]::WriteLine(('本地'+'终端测试')); ssh -V\r"
                )
                .as_bytes()
                .to_vec(),
            ))
            .await
            .unwrap();
        until(&output, "本地终端测试").await;
        until(&output, "OpenSSH").await;
        connection
            .commands
            .send(Command::Input(b"exit\r".to_vec()))
            .await
            .unwrap();
        finished(&mut connection).await;
    }
    #[tokio::test]
    async fn cancel_running_local_command() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut connection = connect(
            &tokio::runtime::Handle::current(),
            Capture(output.clone(), rshell_terminal::Terminal::new(80, 24)),
        );
        until(&output, "PS ").await;
        connection
            .commands
            .send(Command::Input(b"Start-Sleep -Seconds 60\r".to_vec()))
            .await
            .unwrap();
        connection.disconnect();
        finished(&mut connection).await;
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Terminal for Capture {
        fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
            let mut output = self.0.lock().unwrap();
            if output.len() < 65536 {
                output.extend_from_slice(bytes);
            }
            Vec::new()
        }
        fn resize(&mut self, _: u16, _: u16) {}
    }
    #[tokio::test]
    async fn zsh_output_resize_exit_and_cancellation() {
        for cancel in [false, true] {
            let output = Arc::new(Mutex::new(Vec::new()));
            let mut connection =
                connect(&tokio::runtime::Handle::current(), Capture(output.clone()));
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    match connection.events.recv().await.unwrap() {
                        Event::Connected => break,
                        Event::Finished(result) => panic!("startup failed: {result:?}"),
                        _ => {}
                    }
                }
                connection
                    .commands
                    .send(Command::Resize {
                        cols: 100,
                        rows: 30,
                    })
                    .await
                    .unwrap();
                connection
                    .commands
                    .send(Command::Input(
                        "printf 'RSHELL_%s\\n' '本地'; stty size\r"
                            .as_bytes()
                            .to_vec(),
                    ))
                    .await
                    .unwrap();
                loop {
                    let text = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
                    if text.contains("RSHELL_本地") && text.contains("30 100") {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                if cancel {
                    connection.disconnect();
                } else {
                    connection
                        .commands
                        .send(Command::Input(b"exit\r".to_vec()))
                        .await
                        .unwrap();
                }
                loop {
                    if let Some(Event::Finished(result)) = connection.events.recv().await {
                        result.unwrap();
                        break;
                    }
                }
            })
            .await
            .expect("zsh session timed out");
        }
    }
}
