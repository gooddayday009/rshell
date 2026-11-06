#[path = "../../rshell-core/tests/support/headless.rs"]
mod server;
#[path = "../../rshell-core/tests/support/mod.rs"]
mod support;
use rshell_core::config::Config;
use std::{
    path::Path,
    process::{Output, Stdio},
    sync::atomic::Ordering,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

async fn cli(dir: &Path, args: &[&str], secret: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rshell-cli"))
        .args(args)
        .env("RSHELL_CONFIG_DIR", dir)
        .stdin(if secret.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    if let Some(secret) = secret {
        child.stdin.take().unwrap().write_all(secret).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
fn save_profile(dir: &Path, server: &server::Server) {
    Config {
        cli_enabled: true,
        profiles: vec![server.profile.clone()],
        ..Default::default()
    }
    .save(&dir.join("config.json"))
    .unwrap();
}

#[tokio::test]
async fn process_help_errors_and_output_contract() {
    let dir = support::tempdir();
    let help = cli(dir.path(), &["--help"], None).await;
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("sftp upload"));
    assert!(!dir.path().join("config.json").exists());
    let error = cli(dir.path(), &["exec", "--password", "do-not-log-this"], None).await;
    assert_eq!(error.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&error.stderr).contains("do-not-log-this"));
    let server = server::start().await;
    save_profile(dir.path(), &server);
    let output = cli(
        dir.path(),
        &[
            "exec",
            "--profile",
            "CLI loopback",
            "--password-stdin",
            "--",
            "quoted 'a b'; exit 7",
        ],
        Some(b"test-only-secret\r\n"),
    )
    .await;
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, "hello 你好\n".as_bytes());
    assert_eq!(output.stderr, b"remote error\n");
    assert_eq!(
        server.state.commands.lock().unwrap()[0],
        b"quoted 'a b'; exit 7"
    );
    let config = Config::load(&dir.path().join("config.json")).unwrap();
    assert_eq!(config.known_hosts.len(), 1);
    assert_eq!(config.profiles.len(), 1);
    let bad_auth = cli(
        dir.path(),
        &[
            "exec",
            "--profile",
            "CLI loopback",
            "--password-stdin",
            "--",
            "id",
        ],
        Some(b"incorrect-secret\n"),
    )
    .await;
    assert_eq!(bad_auth.status.code(), Some(255));
    assert!(!String::from_utf8_lossy(&bad_auth.stderr).contains("incorrect-secret"));
    let no_exit = cli(
        dir.path(),
        &[
            "exec",
            "--profile",
            "CLI loopback",
            "--password-stdin",
            "--",
            "missing-exit",
        ],
        Some(b"test-only-secret\n"),
    )
    .await;
    assert_eq!(no_exit.status.code(), Some(255));
    let timed = cli(
        dir.path(),
        &[
            "exec",
            "--profile",
            "CLI loopback",
            "--password-stdin",
            "--timeout",
            "1",
            "--",
            "hang",
        ],
        Some(b"test-only-secret\n"),
    )
    .await;
    assert_eq!(timed.status.code(), Some(124));
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.state.connections.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn process_sftp_binary_roundtrip_and_json_with_direct_connection() {
    let server = server::start().await;
    let dir = support::tempdir();
    save_profile(dir.path(), &server);
    let source = dir.path().join("输入 空格.bin");
    let target = dir.path().join("输出 空格.bin");
    let data: Vec<u8> = (0..131073).map(|i| (i % 256) as u8).collect();
    std::fs::write(&source, &data).unwrap();
    let port = server.profile.port.to_string();
    let connection = [
        "--host",
        "127.0.0.1",
        "--user",
        "tester",
        "--port",
        port.as_str(),
        "--password-stdin",
    ];
    let mut upload = vec!["sftp", "upload"];
    upload.extend(connection);
    upload.extend(["--", source.to_str().unwrap(), "/测试 file.bin"]);
    let result = cli(dir.path(), &upload, Some(b"test-only-secret\n")).await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut download = vec!["sftp", "download"];
    download.extend(connection);
    download.extend(["--", "/测试 file.bin", target.to_str().unwrap()]);
    assert!(
        cli(dir.path(), &download, Some(b"test-only-secret\n"))
            .await
            .status
            .success()
    );
    assert_eq!(std::fs::read(&target).unwrap(), data);
    assert_eq!(
        cli(dir.path(), &download, Some(b"test-only-secret\n"))
            .await
            .status
            .code(),
        Some(255)
    );
    assert_eq!(std::fs::read(&target).unwrap(), data);
    let mut list = vec!["sftp", "ls"];
    list.extend(connection);
    list.extend(["--json", "--", "/"]);
    let output = cli(dir.path(), &list, Some(b"test-only-secret\n")).await;
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "测试 file.bin" && entry["size"] == 131073)
    );
}

#[tokio::test]
async fn cli_disabled_by_default_and_after_revocation_before_credentials_or_network() {
    let dir = support::tempdir();
    let server = server::start().await;
    let args = [
        "exec",
        "--host",
        "127.0.0.1",
        "--user",
        "tester",
        "--port",
        &server.profile.port.to_string(),
        "--password-stdin",
        "--",
        "id",
    ];
    // No stdin secret: a disabled invocation must reject before trying to read it.
    let output = cli(dir.path(), &args, None).await;
    assert_eq!(output.status.code(), Some(255));
    assert!(String::from_utf8_lossy(&output.stderr).contains("CLI 默认关闭"));
    assert!(!dir.path().join("config.json").exists());
    save_profile(dir.path(), &server);
    let path = dir.path().join("config.json");
    let mut config = Config::load(&path).unwrap();
    config.cli_enabled = false;
    config.profiles[0].remember_secret = true;
    config.save(&path).unwrap();
    std::fs::create_dir(dir.path().join("credentials")).unwrap();
    std::fs::write(
        dir.path()
            .join("credentials")
            .join(format!("{}.bin", server.profile.id)),
        b"deliberately invalid credential fixture",
    )
    .unwrap();
    for args in [
        vec!["exec", "--profile", "CLI loopback", "--", "id"],
        vec![
            "sftp",
            "sync",
            "upload",
            "--profile",
            "CLI loopback",
            "--delete",
            "--",
            "missing",
            "/target",
        ],
    ] {
        let output = cli(dir.path(), &args, None).await;
        assert_eq!(output.status.code(), Some(255));
        assert!(String::from_utf8_lossy(&output.stderr).contains("CLI 默认关闭"));
    }
    assert_eq!(server.state.auth_calls.load(Ordering::SeqCst), 0);
    assert!(Config::load(&path).unwrap().known_hosts.is_empty());
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires current-user DPAPI; run scripts/test.ps1 -WithCredentials"]
async fn cli_saved_credentials_read_without_password_arguments() {
    use rshell_core::{credentials::EncryptedCredentials, store::Credentials};
    let mut server = server::start().await;
    server.profile.remember_secret = true;
    let dir = support::tempdir();
    save_profile(dir.path(), &server);
    let mut credentials = EncryptedCredentials(dir.path().join("credentials"));
    credentials
        .write(server.profile.id, "test-only-secret")
        .unwrap();
    let output = cli(
        dir.path(),
        &["exec", "--profile", "CLI loopback", "--", "id"],
        None,
    )
    .await;
    credentials.delete(server.profile.id).unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, "hello 你好\n".as_bytes());
}

#[tokio::test]
async fn process_directory_sync_dry_run_default_keep_and_explicit_delete() {
    let server = server::start().await;
    let dir = support::tempdir();
    save_profile(dir.path(), &server);
    let local = dir.path().join("同步 source");
    std::fs::create_dir_all(local.join("nested/empty")).unwrap();
    std::fs::write(local.join("nested/file"), b"payload").unwrap();
    {
        let mut files = server.state.files.lock().unwrap();
        files.insert("/sync/".into(), vec![]);
        files.insert("/sync/extra".into(), b"keep".to_vec());
    }
    let base = [
        "sftp",
        "sync",
        "upload",
        "--profile",
        "CLI loopback",
        "--password-stdin",
        "--json",
    ];
    let mut args = base.to_vec();
    args.extend([
        "--dry-run",
        "--delete",
        "--",
        local.to_str().unwrap(),
        "/sync",
    ]);
    let output = cli(dir.path(), &args, Some(b"test-only-secret\n")).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["dry_run"], true);
    assert!(
        plan["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op["kind"] == "delete_file" && op["path"] == "extra")
    );
    assert!(
        !server
            .state
            .files
            .lock()
            .unwrap()
            .contains_key("/sync/nested/file")
    );
    for delete in [false, true] {
        let mut args = base.to_vec();
        if delete {
            args.push("--delete");
        }
        args.extend(["--", local.to_str().unwrap(), "/sync"]);
        let output = cli(dir.path(), &args, Some(b"test-only-secret\n")).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let files = server.state.files.lock().unwrap();
        assert_eq!(files["/sync/nested/file"], b"payload");
        assert!(files.contains_key("/sync/nested/empty/"));
        assert_eq!(files.contains_key("/sync/extra"), !delete);
    }
    let download = dir.path().join("download");
    let output = cli(
        dir.path(),
        &[
            "sftp",
            "sync",
            "download",
            "--profile",
            "CLI loopback",
            "--password-stdin",
            "--",
            "/sync",
            download.to_str().unwrap(),
        ],
        Some(b"test-only-secret\n"),
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(download.join("nested/file")).unwrap(),
        b"payload"
    );
}
