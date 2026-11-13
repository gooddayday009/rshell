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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shell {
    #[default]
    System,
    Bash,
    Builtin,
}

impl Shell {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => shell_label(),
            Self::Bash => "Git Bash",
            Self::Builtin => "RShell 内置 Shell",
        }
    }
}

pub fn connect(runtime: &tokio::runtime::Handle, terminal: impl Terminal) -> Connection {
    connect_shell(runtime, terminal, Shell::System)
}

pub fn connect_shell(
    runtime: &tokio::runtime::Handle,
    terminal: impl Terminal,
    shell: Shell,
) -> Connection {
    connect_shell_path(runtime, terminal, shell, None)
}

pub fn connect_shell_path(
    runtime: &tokio::runtime::Handle,
    terminal: impl Terminal,
    shell: Shell,
    executable: Option<std::path::PathBuf>,
) -> Connection {
    let (commands, rx) = mpsc::channel(64);
    let (tx, events) = mpsc::channel(16);
    let (cancel, cancelled) = watch::channel(false);
    runtime.spawn_blocking(move || {
        let result = run(terminal, rx, cancelled, &tx, shell, executable);
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
    shell: Shell,
    executable: Option<std::path::PathBuf>,
) -> Result<()> {
    if *cancelled.borrow() {
        return Ok(());
    }
    let mut command = command_for(shell, executable)?;
    let bridge = if shell == Shell::Builtin {
        Some(crate::shell_bridge::Bridge::new()?)
    } else {
        None
    };
    if let Some(bridge) = &bridge {
        bridge.configure(&mut command)?;
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
            shell.label()
        )))?;
        #[cfg(windows)]
        let mut initial_prompt = InitialPrompt::default();
        #[cfg(windows)]
        let mut bash_prompt = BashPrompt::default();
        #[cfg(windows)]
        let started = std::time::Instant::now();
        #[cfg(windows)]
        let mut ready = false;
        #[cfg(not(windows))]
        events.blocking_send(Event::Connected)?;
        loop {
            if *loop_cancelled.borrow() || loop_cancelled.has_changed().is_err() {
                return Ok(());
            }
            #[cfg(windows)]
            if !ready && shell != Shell::System && started.elapsed() > Duration::from_secs(15) {
                anyhow::bail!("Bash 启动超时：15 秒内未出现提示符");
            }
            if let Some(target) = bridge.as_ref().and_then(|bridge| bridge.poll()) {
                let _ = events.try_send(Event::OpenSsh(target));
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
                    if !ready
                        && match shell {
                            Shell::System => initial_prompt.feed(&bytes),
                            Shell::Bash | Shell::Builtin => bash_prompt.feed(&bytes),
                        }
                    {
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

/// Match only our startup PS1 prefix, with constant memory across PTY chunks.
#[cfg(windows)]
#[derive(Default)]
struct BashPrompt(usize);
#[cfg(windows)]
impl BashPrompt {
    fn feed(&mut self, bytes: &[u8]) -> bool {
        const PREFIX: &[u8] = b"RShell ";
        for &byte in bytes {
            if byte == PREFIX[self.0] {
                self.0 += 1;
                if self.0 == PREFIX.len() {
                    self.0 = 0;
                    return true;
                }
            } else {
                self.0 = usize::from(byte == PREFIX[0]);
            }
        }
        false
    }
}

fn command_for(shell: Shell, executable: Option<std::path::PathBuf>) -> Result<CommandBuilder> {
    if shell == Shell::Builtin {
        let mut command = CommandBuilder::new(builtin_executable()?);
        command.env("TERM", "xterm-256color");
        return Ok(command);
    }
    if shell == Shell::System {
        return Ok(if let Some(path) = executable {
            let mut command = CommandBuilder::new(path);
            command.args(["-NoLogo", "-NoProfile"]);
            command
        } else {
            shell_command()
        });
    }
    #[cfg(windows)]
    let bash = executable.map(Ok).unwrap_or_else(git_bash)?;
    #[cfg(not(windows))]
    let bash = executable.unwrap_or_else(|| std::path::PathBuf::from("/bin/bash"));
    let mut command = CommandBuilder::new(&bash);
    command.args(["--noprofile", "--norc", "-i"]);
    command.env("TERM", "xterm-256color");
    command.env("PS1", r"RShell Bash \w\$ ");
    command.env("HISTFILE", "");
    command.env("HISTSIZE", "1000");
    command.env("HISTCONTROL", "ignorespace:ignoredups");
    #[cfg(windows)]
    {
        let mut root = bash
            .parent()
            .and_then(|p| p.parent())
            .context("Git Bash 安装路径无效")?;
        if root.file_name().is_some_and(|name| name == "usr") {
            root = root.parent().context("Git Bash 安装路径无效")?;
        }
        let mut paths = vec![
            root.join("usr/bin"),
            root.join("mingw64/bin"),
            root.join("bin"),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        command.env("PATH", std::env::join_paths(paths)?);
        command.env("MSYSTEM", "MINGW64");
        command.env("CHERE_INVOKING", "1");
        command.env("LANG", "en_US.UTF-8");
    }
    Ok(command)
}

#[cfg(windows)]
fn git_bash() -> Result<std::path::PathBuf> {
    // Never pick WindowsApps/bash.exe: that is a WSL alias, not Git Bash.
    let mut roots = Vec::new();
    for name in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(path) = std::env::var_os(name) {
            roots.push(std::path::PathBuf::from(path).join("Git"));
        }
    }
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        roots.push(std::path::PathBuf::from(path).join("Programs/Git"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("git.exe").is_file()
                && let Some(root) = dir.parent()
            {
                roots.push(root.to_owned());
            }
        }
    }
    roots.into_iter().map(|root| root.join("bin/bash.exe")).find(|path| path.is_file())
        .context("未找到 Git Bash。请安装 Git for Windows（或将 Git 的 cmd 目录加入 PATH），或在设置中选择本地 PowerShell。")
}

pub fn builtin_executable() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let mut dir = exe.parent().context("无法确定程序目录")?;
    if dir.file_name().is_some_and(|name| name == "deps") {
        dir = dir.parent().context("无法确定构建目录")?;
    }
    let path = dir.join(if cfg!(windows) {
        "rshell-shell.exe"
    } else {
        "rshell-shell"
    });
    if !path.is_file() {
        anyhow::bail!(
            "缺少内置 Shell 组件：{}；请使用完整 RShell 包或 cargo build -p rshell",
            path.display()
        );
    }
    Ok(path)
}

pub fn detect_path(shell: crate::config::DefaultShell) -> Option<std::path::PathBuf> {
    use crate::config::DefaultShell;
    match shell {
        DefaultShell::Builtin => None,
        DefaultShell::GitBash => {
            #[cfg(windows)]
            {
                git_bash().ok()
            }
            #[cfg(not(windows))]
            {
                Some(std::path::PathBuf::from("/bin/bash")).filter(|p| p.is_file())
            }
        }
        DefaultShell::PowerShell => {
            #[cfg(windows)]
            {
                let mut candidates = Vec::new();
                if let Some(root) = std::env::var_os("SystemRoot") {
                    candidates.push(
                        std::path::PathBuf::from(root)
                            .join("System32/WindowsPowerShell/v1.0/powershell.exe"),
                    );
                }
                if let Some(path) = std::env::var_os("PATH") {
                    candidates
                        .extend(std::env::split_paths(&path).map(|p| p.join("powershell.exe")));
                }
                candidates.into_iter().find(|p| p.is_file())
            }
            #[cfg(not(windows))]
            {
                Some(std::path::PathBuf::from("/bin/zsh")).filter(|p| p.is_file())
            }
        }
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

    #[test]
    fn bash_prompt_matches_split_chunks_without_buffering_output() {
        let mut prompt = BashPrompt::default();
        assert!(!prompt.feed(&vec![b'x'; 100_000]));
        assert!(!prompt.feed(b"RShe"));
        assert!(prompt.feed(b"ll Bash /d/work$ "));
    }

    #[tokio::test]
    #[ignore = "requires installed Git for Windows; runs a real Bash PTY"]
    async fn git_bash_full_syntax_completion_history_interrupt_and_exit() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::Builder::new()
            .prefix("bash syntax ")
            .tempdir_in(&root)
            .unwrap();
        std::fs::write(
            temp.path().join("rshell-bash-test.sh"),
            r#"
items=(alpha beta)
next() { local n=$1; printf '%s' "$((n+1))"; }
printf '%s\n' "${items[@]}" | grep '^b' | sed 's/^b/B/' > result.txt
for n in 1 2 3; do printf '%s\n' "$n"; done | awk '{s+=$1} END {print s}' >> result.txt
cat <<'EOF' > heredoc.txt
literal $HOME
EOF
[[ $(cat result.txt) == $'Beta\n6' ]] && [[ $(next 41) == 42 ]] || return 1
printf 'BASH-SYNTAX=%s\n' "$(next 41)"
printf 'UNICODE=本地终端测试\n'
RUN=$(( ${RUN:-0} + 1 ))
printf 'RUN=%s\n' "$RUN"
"#,
        )
        .unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        struct RetainedCapture(Arc<Mutex<Vec<u8>>>, Arc<Mutex<rshell_terminal::Terminal>>);
        impl Terminal for RetainedCapture {
            fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                self.1.lock().unwrap().feed(bytes)
            }
            fn resize(&mut self, cols: u16, rows: u16) {
                self.1.lock().unwrap().resize(cols, rows);
            }
        }
        let terminal = Arc::new(Mutex::new(rshell_terminal::Terminal::new(100, 30)));
        {
            let mut terminal = terminal.lock().unwrap();
            terminal.feed(b"retained-remote-output");
            terminal.archive_screen();
        }
        let mut connection = connect_shell(
            &tokio::runtime::Handle::current(),
            RetainedCapture(output.clone(), terminal.clone()),
            Shell::Bash,
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match connection.events.recv().await.unwrap() {
                    Event::Connected => break,
                    Event::Finished(result) => panic!("Bash startup failed: {result:?}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        connection
            .commands
            .send(Command::Resize {
                cols: 120,
                rows: 36,
            })
            .await
            .unwrap();
        let cwd = temp
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
            .replace('\'', "'\\''");
        connection
            .commands
            .send(Command::Input(
                format!("cd '{cwd}'; source rshell-bash-test.sh\r").into_bytes(),
            ))
            .await
            .unwrap();
        until(&output, "BASH-SYNTAX=42").await;
        until(&output, "UNICODE=本地终端测试").await;
        until(&output, "RUN=1").await;
        assert_eq!(
            std::fs::read_to_string(temp.path().join("result.txt")).unwrap(),
            "Beta\n6\n"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("heredoc.txt")).unwrap(),
            "literal $HOME\n"
        );
        // Readline filename completion, then history Up, both submitted through the PTY.
        connection
            .commands
            .send(Command::Input(b"source rshell-bas\t\r".to_vec()))
            .await
            .unwrap();
        until(&output, "RUN=2").await;
        connection
            .commands
            .send(Command::Input(b"\x1b[A\r".to_vec()))
            .await
            .unwrap();
        until(&output, "RUN=3").await;
        assert!(
            terminal
                .lock()
                .unwrap()
                .find("retained-remote-output", None, false)
                .is_some()
        );
        connection
            .commands
            .send(Command::Input(b"sleep 60\r".to_vec()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        output.lock().unwrap().clear();
        connection
            .commands
            .send(Command::Input(vec![3]))
            .await
            .unwrap();
        until(&output, "RShell Bash ").await;
        connection
            .commands
            .send(Command::Input(b"printf 'INTERRUPT=%s\\n' OK\r".to_vec()))
            .await
            .unwrap();
        until(&output, "INTERRUPT=OK").await;
        connection
            .commands
            .send(Command::Input(b"exit\r".to_vec()))
            .await
            .unwrap();
        finished(&mut connection).await;
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
