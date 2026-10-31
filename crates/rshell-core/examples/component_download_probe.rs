//! Opt-in download/cache/self-test check. Never calls a model or runs terminal commands.
use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, time::Duration};
fn main() -> Result<()> {
    ensure!(
        std::env::args().any(|arg| arg == "--download"),
        "Pass --download to explicitly approve the pinned component download"
    );
    tokio::runtime::Runtime::new()?.block_on(async {
        let absent = std::env::current_exe()?
            .parent()
            .unwrap()
            .join("component-probe-no-bundle/rshell-agent-sdk.exe");
        let executable = rshell_core::agent_component::ensure_component(absent.clone(), |status| {
            println!("{status}")
        })
        .await?;
        let reused = rshell_core::agent_component::ensure_component(absent, |_| {
            panic!("cache reuse must not download")
        })
        .await?;
        ensure!(executable == reused, "component cache path mismatch");
        let mut command = tokio::process::Command::new(&executable);
        command.arg("--self-test").kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let result = tokio::time::timeout(Duration::from_secs(60), command.output())
            .await
            .context("component self-test timeout")??;
        ensure!(result.status.success(), "component self-test failed");
        ensure!(
            result.stdout.len() <= 16384,
            "component self-test output exceeded limit"
        );
        let info: serde_json::Value = serde_json::from_slice(&result.stdout)?;
        ensure!(
            info["protocol_version"] == 1 && info["bundled_engine"] == true,
            "component protocol mismatch"
        );
        println!(
            "Verified download, pinned hashes, cache reuse and SDK self-test: {}",
            info
        );
        let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA missing")?);
        ensure!(executable.starts_with(root), "component outside user cache");
        Ok(())
    })
}
