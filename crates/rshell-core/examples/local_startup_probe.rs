//! Controlled startup measurements. No profile scripts, credentials or user commands.
use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

fn sample(kind: &str) -> Result<serde_json::Value> {
    let start = Instant::now();
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let pty_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let mut command = CommandBuilder::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile"]);
    match kind {
        "engine" => command.args(["-NonInteractive", "-Command", "[Console]::WriteLine('__RS_ENGINE__')"]),
        "module" => command.args(["-NonInteractive", "-Command", "[Console]::WriteLine('__RS_ENGINE__'); $timer=[Diagnostics.Stopwatch]::StartNew(); Import-Module PSReadLine -ErrorAction Stop; [Console]::WriteLine(('__RS_MODULE_MS__:'+ $timer.Elapsed.TotalMilliseconds.ToString([Globalization.CultureInfo]::InvariantCulture))); [Console]::WriteLine('__RS_DONE__')"]),
        "interactive" => {},
        _ => bail!("unknown probe"),
    }
    let mut child = pair.slave.spawn_command(command)?;
    let spawned_ms = start.elapsed().as_secs_f64() * 1000.0;
    drop(pair.slave);
    let (tx, rx) = mpsc::sync_channel(32);
    let read_thread = std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        while let Ok(n) = reader.read(&mut bytes) {
            if n == 0 {
                break;
            }
            let _ = tx.send(bytes[..n].to_vec());
        }
    });
    let result = (|| -> Result<_> {
        let mut terminal = rshell_terminal::Terminal::new(80, 24);
        let mut text = String::new();
        let mut first_ms = None;
        let mut engine_ms = None;
        let mut done_ms = None;
        while start.elapsed() < Duration::from_secs(15) {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => {
                    first_ms.get_or_insert(start.elapsed().as_secs_f64() * 1000.0);
                    let replies = terminal.feed(&bytes);
                    if !replies.is_empty() {
                        writer.write_all(&replies)?;
                        writer.flush()?;
                    }
                    if text.len() + bytes.len() > 65536 {
                        bail!("probe output exceeded limit");
                    }
                    text.push_str(&String::from_utf8_lossy(&bytes));
                    if engine_ms.is_none() && text.contains("__RS_ENGINE__") {
                        engine_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    let complete = match kind {
                        "engine" => engine_ms.is_some(),
                        "module" => text.contains("__RS_DONE__"),
                        _ => text.find("PS ").is_some_and(|p| text[p..].contains('>')),
                    };
                    if complete {
                        done_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let done_ms = done_ms.context("startup marker not reached within 15 seconds")?;
        let module_ms = text
            .split("__RS_MODULE_MS__:")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit() && c != '.').next())
            .and_then(|s| s.parse::<f64>().ok());
        Ok(
            serde_json::json!({"kind":kind,"pty_ms":pty_ms,"spawned_ms":spawned_ms,"first_output_ms":first_ms,"engine_marker_ms":engine_ms,"module_import_ms":module_ms,"completed_ms":done_ms}),
        )
    })();
    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(rx);
    drop(pair.master);
    let _ = read_thread.join();
    result
}
fn main() -> Result<()> {
    for _ in 0..3 {
        for kind in ["engine", "module", "interactive"] {
            println!("{}", sample(kind)?);
        }
    }
    Ok(())
}
