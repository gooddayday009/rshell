use crate::{
    connection::{Command, Event},
    transfer::{partial_path, publish, safe_name},
};
use anyhow::{Context, Result, bail};
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf, client};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use zmodem2::{Action, Event as ZEvent, FileInfo, Position, Receiver, Sender};

const SEND: &[u8] = b"**\x18B01";
const RECEIVE: &[u8] = b"**\x18B00";
#[derive(Default)]
pub(crate) struct Detector {
    pending: Vec<u8>,
    // Printable padding is displayed immediately, but retained for split handshakes.
    displayed: usize,
}
impl Detector {
    pub fn has_pending(&self) -> bool {
        self.pending.len() > self.displayed
    }
    pub fn flush(&mut self) -> Vec<u8> {
        let plain = self.pending.split_off(self.displayed);
        self.pending.clear();
        self.displayed = 0;
        plain
    }

    pub fn feed(&mut self, data: &[u8]) -> (Vec<u8>, Option<(bool, Vec<u8>)>) {
        self.pending.extend_from_slice(data);
        let mut offset = 0;
        while offset < self.pending.len() {
            let tail = &self.pending[offset..];
            if tail.starts_with(SEND) || tail.starts_with(RECEIVE) {
                let upload = tail.starts_with(SEND);
                let wire = self.pending.split_off(offset);
                let plain = self.pending[self.displayed.min(offset)..].to_vec();
                self.pending.clear();
                self.displayed = 0;
                return (plain, Some((upload, wire)));
            }
            if SEND.starts_with(tail) || RECEIVE.starts_with(tail) {
                break;
            }
            offset += 1;
        }
        let padding = self.pending[offset..]
            .iter()
            .take_while(|&&b| b == b'*')
            .count();
        let end = offset + padding;
        let plain = self.pending[self.displayed..end].to_vec();
        self.pending.drain(..offset);
        self.displayed = padding;
        (plain, None)
    }
}
enum Engine {
    Send(Sender),
    Receive(Receiver),
}
impl Engine {
    fn poll(&mut self) -> Action<'_> {
        match self {
            Self::Send(s) => s.poll(),
            Self::Receive(r) => r.poll(),
        }
    }
    fn submit(&mut self, bytes: &[u8]) -> Result<usize> {
        Ok(match self {
            Self::Send(s) => s.submit_wire(bytes)?,
            Self::Receive(r) => r.submit_wire(bytes)?,
        })
    }
    fn written(&mut self, n: usize) {
        match self {
            Self::Send(s) => s.wire_written(n),
            Self::Receive(r) => r.wire_written(n),
        }
    }
    fn timeout(&mut self) -> Result<()> {
        match self {
            Self::Send(s) => s.timeout()?,
            Self::Receive(r) => r.timeout()?,
        };
        Ok(())
    }
}

pub(crate) async fn run(
    upload: bool,
    mut pending: Vec<u8>,
    reader: &mut ChannelReadHalf,
    writer: &ChannelWriteHalf<client::Msg>,
    commands: &mut mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    terminal: &mut impl crate::connection::Terminal,
) -> Result<Vec<u8>> {
    let (reply, choice) = oneshot::channel();
    events.send(Event::TransferPrompt { upload, reply }).await?;
    let path = tokio::time::timeout(Duration::from_secs(120), choice)
        .await??
        .context("用户取消传输")?;
    let mut file = None;
    let mut expected_upload_size = None;
    let mut target: Option<(PathBuf, PathBuf)> = None;
    let mut engine = if upload {
        let input = tokio::fs::File::open(&path).await?;
        let size = u32::try_from(input.metadata().await?.len())
            .context("ZMODEM 文件必须小于 4 GiB，请使用 SFTP")?;
        expected_upload_size = Some(size as u64);
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .context("文件名不是 UTF-8")?;
        safe_name(name)?;
        let mut sender = Sender::new()?;
        sender.start_file(
            FileInfo::new(name.as_bytes(), Some(Position::new(size))).with_mode(0o100644),
        )?;
        file = Some(input);
        Engine::Send(sender)
    } else {
        Engine::Receive(Receiver::new()?)
    };
    let mut cancel_run = 0;
    if remote_cancelled(&pending, &mut cancel_run) {
        bail!("远端中止传输");
    }
    let mut quiet_timeouts = 0;
    let mut bytes = 0u64;
    let mut last_update = std::time::Instant::now();
    loop {
        // Cancel and resize remain responsive even while file blocks are flowing.
        match commands.try_recv() {
            Ok(Command::Input(data)) if data.contains(&3) || data.contains(&27) => {
                bail!("用户取消传输；未完成的 .rshell-*.part 文件可手动删除")
            }
            Ok(Command::Resize { cols, rows }) => {
                terminal.resize(cols, rows);
                writer.window_change(cols as u32, rows as u32, 0, 0).await?
            }
            Err(mpsc::error::TryRecvError::Disconnected) => bail!("会话关闭"),
            _ => {}
        }
        match engine.poll() {
            Action::WriteWire(data) => {
                let n = data.len();
                tokio::time::timeout(Duration::from_secs(30), writer.data_bytes(data.to_vec()))
                    .await??;
                engine.written(n);
            }
            Action::ReadFile { offset, max_len } => {
                let input = file.as_mut().context("缺少上传文件")?;
                input
                    .seek(std::io::SeekFrom::Start(offset.get() as u64))
                    .await?;
                let mut buffer = vec![0; max_len.min(64 * 1024)];
                let n = input.read(&mut buffer).await?;
                if let Engine::Send(sender) = &mut engine {
                    sender.submit_file(&buffer[..n])?;
                }
                bytes = offset.get() as u64 + n as u64;
            }
            Action::WriteFile(data) => {
                let n = data.len();
                file.as_mut()
                    .context("缺少接收文件")?
                    .write_all(data)
                    .await?;
                if let Engine::Receive(receiver) = &mut engine {
                    receiver.file_written(n)?;
                }
                bytes += n as u64;
            }
            Action::Event(ZEvent::FileStarted(info)) => {
                let name =
                    safe_name(std::str::from_utf8(info.name).context("远端文件名不是 UTF-8")?)?;
                let destination = path.join(name);
                let partial = partial_path(&destination)?;
                file = Some(
                    tokio::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&partial)
                        .await?,
                );
                target = Some((partial, destination));
                bytes = 0;
            }
            Action::Event(ZEvent::FileCompleted) => {
                if expected_upload_size.is_some_and(|expected| bytes < expected) {
                    bail!("远端跳过文件或未完整接收，请检查远端同名文件和 rz 参数");
                }
                if let Some(output) = file.take()
                    && !upload
                {
                    output.sync_all().await?;
                }
                if let Some((partial, destination)) = target.take() {
                    publish(&partial, &destination).await?;
                }
                if let Engine::Send(sender) = &mut engine {
                    sender.finish()?;
                }
            }
            Action::Event(ZEvent::SessionCompleted) => {
                // Completion events precede the queued ZFIN/OO bytes in zmodem2.
                while let Action::WriteWire(data) = engine.poll() {
                    let n = data.len();
                    writer.data_bytes(data.to_vec()).await?;
                    engine.written(n);
                }
                if !upload {
                    pending = finish_receive(reader, pending).await;
                }
                let _ = events.try_send(Event::Status(format!("ZMODEM 传输完成：{bytes} 字节")));
                return Ok(pending);
            }
            Action::Event(ZEvent::Aborted) => bail!("远端中止传输"),
            Action::Idle => {
                if !pending.is_empty() {
                    let n = engine.submit(&pending)?;
                    if n == 0 {
                        bail!("ZMODEM 解析无进展");
                    }
                    pending.drain(..n);
                    continue;
                }
                tokio::select! {
                    msg = reader.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => {
                            quiet_timeouts = 0;
                            if remote_cancelled(&data, &mut cancel_run) { bail!("远端中止传输"); }
                            pending.extend_from_slice(&data);
                        },
                        Some(ChannelMsg::Close | ChannelMsg::Eof) | None => bail!("传输期间远端关闭连接"),
                        _ => {}
                    },
                    cmd = commands.recv() => match cmd {
                        Some(Command::Input(data)) if data.contains(&3) || data.contains(&27) => bail!("用户取消传输"),
                        Some(Command::Resize { cols, rows }) => { terminal.resize(cols, rows); writer.window_change(cols as u32, rows as u32, 0, 0).await?; },
                        None => bail!("会话关闭"),
                        _ => {}
                    },
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        quiet_timeouts += 1;
                        if quiet_timeouts >= 3 { bail!("远端连续 30 秒无响应，已中止传输"); }
                        engine.timeout()?;
                    },
                }
            }
            _ => bail!("不支持的 ZMODEM 状态"),
        }
        if last_update.elapsed() > Duration::from_millis(250) {
            let _ = events.try_send(Event::Status(format!(
                "ZMODEM 已传输 {bytes} 字节（Esc / Ctrl+C 取消）"
            )));
            last_update = std::time::Instant::now();
        }
        tokio::task::yield_now().await;
    }
}
fn remote_cancelled(data: &[u8], consecutive: &mut usize) -> bool {
    for byte in data {
        *consecutive = if *byte == 0x18 { *consecutive + 1 } else { 0 };
        if *consecutive >= 5 {
            return true;
        }
    }
    false
}

async fn finish_receive(reader: &mut ChannelReadHalf, mut pending: Vec<u8>) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let framing = pending
            .iter()
            .take_while(|b| matches!(b, b'\r' | b'\n' | 0x11))
            .count();
        pending.drain(..framing);
        if pending.starts_with(b"OO") {
            return pending.split_off(2);
        }
        if !b"OO".starts_with(&pending) {
            return pending;
        }
        match tokio::time::timeout_at(deadline, reader.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => pending.extend_from_slice(&data),
            _ => return pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_cancel_sequence_can_cross_packets() {
        let mut consecutive = 0;
        assert!(!remote_cancelled(&[0x18; 3], &mut consecutive));
        assert!(remote_cancelled(&[0x18; 2], &mut consecutive));
        consecutive = 0;
        assert!(!remote_cancelled(
            &[0x18, 0x58, 0x18, 0x58],
            &mut consecutive
        ));
    }
    #[test]
    fn detect_fragmented_handshakes_without_eating_text() {
        for marker in [SEND, RECEIVE] {
            for split in 0..marker.len() {
                let mut detector = Detector::default();
                let mut initial = b"hello\r\n".to_vec();
                initial.extend_from_slice(&marker[..split]);
                let (plain, transfer) = detector.feed(&initial);
                let mut expected = b"hello\r\n".to_vec();
                expected.extend_from_slice(&marker[..split.min(2)]);
                assert_eq!(plain, expected);
                assert!(transfer.is_none());
                let (_, transfer) = detector.feed(&marker[split..]);
                let (upload, data) = transfer.unwrap();
                assert_eq!(upload, marker == SEND);
                assert_eq!(data, marker);
            }
        }
        let mut detector = Detector::default();
        assert_eq!(detector.feed(b"a*b**c").0, b"a*b**c");
    }

    #[test]
    fn stars_echo_immediately_without_duplication_or_timeout() {
        let mut detector = Detector::default();
        for _ in 0..100 {
            assert_eq!(detector.feed(b"*").0, b"*");
            assert!(!detector.has_pending());
            assert!(detector.pending.len() <= 2);
        }
        assert_eq!(detector.feed(b"abc").0, b"abc");
        assert!(detector.flush().is_empty());
        assert_eq!(detector.feed(b"**\x18").0, b"**");
        assert!(detector.has_pending());
        assert_eq!(detector.flush(), b"\x18");
        assert!(!detector.has_pending());
    }

    #[test]
    fn handshake_survives_one_byte_packets_after_visible_stars() {
        for marker in [SEND, RECEIVE] {
            let mut detector = Detector::default();
            for &byte in &marker[..marker.len() - 1] {
                let (plain, transfer) = detector.feed(&[byte]);
                assert_eq!(plain, if byte == b'*' { b"*".as_slice() } else { b"" });
                assert!(transfer.is_none());
            }
            let mut last = vec![marker[marker.len() - 1]];
            last.extend_from_slice(b"payload");
            let (plain, transfer) = detector.feed(&last);
            assert!(plain.is_empty());
            let (upload, wire) = transfer.unwrap();
            assert_eq!(upload, marker == SEND);
            assert_eq!(wire, [marker, b"payload"].concat());
            assert!(detector.flush().is_empty());
        }
    }
}
