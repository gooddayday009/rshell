//! Serial I/O runs on a blocking worker with bounded queues and finite I/O timeouts.
use crate::{
    config::{SerialFlowControl, SerialParity, SerialSettings},
    connection::{Command, Connection, Event, Terminal},
};
use anyhow::{Context, Result};
use std::{
    io::{Read, Write},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

pub fn available_ports() -> Result<Vec<String>> {
    let mut names: Vec<_> = serialport::available_ports()
        .context("枚举串口失败")?
        .into_iter()
        .map(|p| p.port_name)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn open(settings: &SerialSettings) -> Result<Box<dyn serialport::SerialPort>> {
    settings.validate()?;
    serialport::new(settings.port_name.to_ascii_uppercase(), settings.baud_rate)
        .data_bits(match settings.data_bits {
            5 => serialport::DataBits::Five,
            6 => serialport::DataBits::Six,
            7 => serialport::DataBits::Seven,
            _ => serialport::DataBits::Eight,
        })
        .stop_bits(if settings.stop_bits == 2 {
            serialport::StopBits::Two
        } else {
            serialport::StopBits::One
        })
        .parity(match settings.parity {
            SerialParity::None => serialport::Parity::None,
            SerialParity::Odd => serialport::Parity::Odd,
            SerialParity::Even => serialport::Parity::Even,
        })
        .flow_control(match settings.flow_control {
            SerialFlowControl::None => serialport::FlowControl::None,
            SerialFlowControl::Software => serialport::FlowControl::Software,
            SerialFlowControl::Hardware => serialport::FlowControl::Hardware,
        })
        .timeout(Duration::from_millis(250))
        .open()
        .with_context(|| {
            format!(
                "打开串口 {} 失败（请检查设备、占用和参数）",
                settings.port_name
            )
        })
}

struct PortIo(Box<dyn serialport::SerialPort>);
impl Read for PortIo {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.set_timeout(Duration::from_millis(10))?;
        self.0.read(bytes)
    }
}
impl Write for PortIo {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.timeout() != Duration::from_millis(250) {
            self.0.set_timeout(Duration::from_millis(250))?;
        }
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn connect(
    runtime: &tokio::runtime::Handle,
    settings: SerialSettings,
    terminal: impl Terminal,
) -> Connection {
    let (commands, rx) = mpsc::channel(64);
    let (tx, events) = mpsc::channel(16);
    let (cancel, cancelled) = watch::channel(false);
    runtime.spawn_blocking(move || {
        let result = (|| -> Result<()> {
            if stopped(&cancelled) {
                return Ok(());
            }
            let mut port = PortIo(open(&settings)?);
            if stopped(&cancelled) {
                return Ok(());
            }
            tx.blocking_send(Event::Connected)?;
            pump(&mut port, terminal, rx, cancelled)
        })();
        // The port is dropped before reporting completion, releasing exclusive access.
        let _ = tx.blocking_send(Event::Finished(result.map_err(|e| format!("{e:#}"))));
    });
    Connection {
        commands,
        events,
        cancel,
        ssh_session: Default::default(),
    }
}

fn stopped(cancel: &watch::Receiver<bool>) -> bool {
    *cancel.borrow() || cancel.has_changed().is_err()
}

fn write_bytes(port: &mut impl Write, bytes: &[u8], cancel: &watch::Receiver<bool>) -> Result<()> {
    // Single-byte writes bound cancellation latency even at low baud rates or blocked CTS.
    // Never retry a timed-out write: the driver may already have transmitted part of it.
    for byte in bytes {
        if stopped(cancel) {
            return Ok(());
        }
        port.write_all(std::slice::from_ref(byte))
            .context("串口写入失败，连接已停止；输入不会自动重发")?;
    }
    Ok(())
}

fn pump(
    port: &mut (impl Read + Write),
    mut terminal: impl Terminal,
    mut commands: mpsc::Receiver<Command>,
    cancel: watch::Receiver<bool>,
) -> Result<()> {
    let mut buffer = [0u8; 8192];
    let mut pending = Vec::new();
    let mut offset = 0;
    while !stopped(&cancel) {
        if offset == pending.len() {
            match commands.try_recv() {
                Ok(Command::Input(bytes)) => {
                    anyhow::ensure!(
                        bytes.len() <= 64 * 1024 + 16,
                        "单次串口输入超过 64 KiB 限制"
                    );
                    pending = bytes;
                    offset = 0;
                }
                Ok(Command::Resize { cols, rows }) => terminal.resize(cols, rows),
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
        }
        // Interleave reads with large pastes, including when a slow driver blocks writes.
        let started = std::time::Instant::now();
        while offset < pending.len() && !stopped(&cancel) {
            write_bytes(port, &pending[offset..offset + 1], &cancel)?;
            offset += 1;
            if started.elapsed() >= Duration::from_millis(5) {
                break;
            }
        }
        if stopped(&cancel) {
            break;
        }
        match port.read(&mut buffer) {
            Ok(0) => std::thread::sleep(Duration::from_millis(5)),
            Ok(n) => write_bytes(port, &terminal.output(&buffer[..n]), &cancel)?,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e).context("串口读取失败（设备可能已断开）"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    #[derive(Default)]
    struct Capture {
        bytes: Vec<u8>,
        size: (u16, u16),
    }
    struct Screen(Arc<Mutex<Capture>>);
    impl Terminal for Screen {
        fn output(&mut self, data: &[u8]) -> Vec<u8> {
            self.0.lock().unwrap().bytes.extend_from_slice(data);
            b"reply".to_vec()
        }
        fn resize(&mut self, cols: u16, rows: u16) {
            self.0.lock().unwrap().size = (cols, rows);
        }
    }
    struct FakePort {
        reads: usize,
        writes: Vec<u8>,
        cancel: watch::Sender<bool>,
        fail_write: bool,
    }
    impl Read for FakePort {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            match self.reads {
                1 => Err(io::ErrorKind::TimedOut.into()),
                2 => {
                    buffer[..3].copy_from_slice(&[0xe4, 0xb8, 0xad]);
                    Ok(3)
                }
                _ => {
                    self.cancel.send(true).unwrap();
                    Ok(0)
                }
            }
        }
    }
    impl Write for FakePort {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::ErrorKind::TimedOut.into());
            }
            self.writes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("must not block on driver flush")
        }
    }
    #[test]
    fn input_output_replies_resize_timeout_and_cancel() {
        let (tx, rx) = mpsc::channel(4);
        tx.try_send(Command::Input(b"hello\r".to_vec())).unwrap();
        tx.try_send(Command::Resize {
            cols: 100,
            rows: 30,
        })
        .unwrap();
        let (cancel, cancelled) = watch::channel(false);
        let capture = Arc::new(Mutex::new(Capture::default()));
        let mut port = FakePort {
            reads: 0,
            writes: vec![],
            cancel,
            fail_write: false,
        };
        pump(&mut port, Screen(capture.clone()), rx, cancelled).unwrap();
        assert_eq!(port.writes, b"hello\rreply");
        assert_eq!(capture.lock().unwrap().bytes, "中".as_bytes());
        assert_eq!(capture.lock().unwrap().size, (100, 30));
    }
    #[test]
    fn write_failure_stops_without_retry_and_cancel_skips_input() {
        let (cancel, cancelled) = watch::channel(false);
        let mut port = FakePort {
            reads: 0,
            writes: vec![],
            cancel,
            fail_write: true,
        };
        assert!(write_bytes(&mut port, b"x", &cancelled).is_err());
        port.fail_write = false;
        port.cancel.send(true).unwrap();
        write_bytes(&mut port, b"x", &cancelled).unwrap();
        assert!(port.writes.is_empty());
    }
    #[tokio::test]
    async fn invalid_port_reports_finished_without_connected() {
        let mut connection = connect(
            &tokio::runtime::Handle::current(),
            SerialSettings {
                port_name: "invalid".into(),
                ..Default::default()
            },
            Screen(Arc::new(Mutex::new(Capture::default()))),
        );
        let event = tokio::time::timeout(Duration::from_secs(3), connection.events.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(Event::Finished(Err(_)))));
    }
}
