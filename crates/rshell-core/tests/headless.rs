#[path = "support/headless.rs"]
mod server;
mod support;
use rshell_core::{
    config::{AuthMethod, Config},
    headless::Session,
    transfer::{Outcome, Request},
};
use std::{sync::atomic::Ordering, time::Duration};
use zeroize::Zeroizing;

#[tokio::test]
async fn exec_preserves_streams_exit_and_command_and_persists_trust() {
    let server = server::start().await;
    let dir = support::tempdir();
    let path = dir.path().join("config.json");
    let session = Session::connect(
        server.profile.clone(),
        Zeroizing::new("test-only-secret".into()),
        path.clone(),
    )
    .await
    .unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(config.known_hosts.len(), 1);
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("test-only-secret")
    );
    let mut out = Vec::new();
    let mut err = Vec::new();
    let command = "printf '%s' \"a b\"; exit 7";
    assert_eq!(session.exec(command, &mut out, &mut err).await.unwrap(), 7);
    assert_eq!(out, "hello 你好\n".as_bytes());
    assert_eq!(err, b"remote error\n");
    assert_eq!(server.state.commands.lock().unwrap()[0], command.as_bytes());
    for command in ["reject", "missing-exit"] {
        assert!(
            session
                .exec(command, &mut Vec::new(), &mut Vec::new())
                .await
                .is_err()
        );
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            session.exec("hang", &mut Vec::new(), &mut Vec::new())
        )
        .await
        .is_err()
    );
    session.disconnect().await;
}

#[tokio::test]
async fn host_key_changes_and_locked_first_trust_fail_before_authentication() {
    let server = server::start().await;
    let dir = support::tempdir();
    let path = dir.path().join("config.json");
    let lock = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dir.path().join("instance.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(
        Session::connect(
            server.profile.clone(),
            Zeroizing::new("test-only-secret".into()),
            path.clone()
        )
        .await
        .is_err()
    );
    assert_eq!(server.state.auth_calls.load(Ordering::SeqCst), 0);
    assert!(!path.exists());
    drop(lock);
    let session = Session::connect(
        server.profile.clone(),
        Zeroizing::new("test-only-secret".into()),
        path.clone(),
    )
    .await
    .unwrap();
    session.disconnect().await;
    let mut config = Config::load(&path).unwrap();
    // A trusted host needs no write lock, even while the GUI owns the config.
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("instance.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let session = Session::connect(
        server.profile.clone(),
        Zeroizing::new("test-only-secret".into()),
        path.clone(),
    )
    .await
    .unwrap();
    session.disconnect().await;
    drop(lock);
    config.known_hosts[0].fingerprint = "SHA256:changed".into();
    config.save(&path).unwrap();
    let calls = server.state.auth_calls.load(Ordering::SeqCst);
    assert!(
        Session::connect(
            server.profile.clone(),
            Zeroizing::new("test-only-secret".into()),
            path
        )
        .await
        .is_err()
    );
    assert_eq!(server.state.auth_calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn sftp_roundtrip_no_overwrite_and_private_key_without_shell() {
    let server = server::start().await;
    let dir = support::tempdir();
    let key_path = dir.path().join("key");
    let key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    key.write_openssh_file(&key_path, Default::default())
        .unwrap();
    let mut profile = server.profile.clone();
    profile.auth = AuthMethod::PrivateKey;
    profile.key_path = key_path.to_string_lossy().into_owned();
    let session = Session::connect(
        profile,
        Zeroizing::new(String::new()),
        dir.path().join("config.json"),
    )
    .await
    .unwrap();
    let payload: Vec<u8> = (0..300_000).map(|i| (i % 251) as u8).collect();
    let source = dir.path().join("source 空格.bin");
    std::fs::write(&source, &payload).unwrap();
    let remote = "/remote 空格.bin".to_string();
    session
        .sftp(Request::Upload {
            local: source.clone(),
            remote: remote.clone(),
        })
        .await
        .unwrap();
    let target = dir.path().join("download.bin");
    session
        .sftp(Request::Download {
            local: target.clone(),
            remote: remote.clone(),
        })
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), payload);
    let Outcome::Listed { entries, .. } = session.sftp(Request::List("/".into())).await.unwrap()
    else {
        panic!("missing listing");
    };
    assert!(
        entries
            .iter()
            .any(|entry| entry.name == "remote 空格.bin" && entry.size == 300_000)
    );
    std::fs::write(&source, b"must not overwrite").unwrap();
    assert!(
        session
            .sftp(Request::Upload {
                local: source,
                remote: remote.clone()
            })
            .await
            .is_err()
    );
    assert_eq!(server.state.files.lock().unwrap()[&remote], payload);
    assert!(
        session
            .sftp(Request::Download {
                local: target.clone(),
                remote
            })
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(target).unwrap(), payload);
    assert!(
        session
            .sftp(Request::Upload {
                local: dir.path().to_path_buf(),
                remote: "/dir".into()
            })
            .await
            .is_err()
    );
    assert!(server.state.commands.lock().unwrap().is_empty());
    session.disconnect().await;
}

#[tokio::test]
async fn encrypted_private_key_accepts_only_correct_passphrase() {
    let server = server::start().await;
    let dir = support::tempdir();
    let key_path = dir.path().join("encrypted-key");
    let key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let encrypted = key
        .encrypt(&mut rand::rng(), "test-only-passphrase")
        .unwrap();
    encrypted
        .write_openssh_file(&key_path, Default::default())
        .unwrap();
    let mut profile = server.profile.clone();
    profile.auth = AuthMethod::PrivateKey;
    profile.key_path = key_path.to_string_lossy().into_owned();
    let path = dir.path().join("config.json");
    assert!(
        Session::connect(
            profile.clone(),
            Zeroizing::new("wrong".into()),
            path.clone()
        )
        .await
        .is_err()
    );
    assert_eq!(server.state.auth_calls.load(Ordering::SeqCst), 0);
    Session::connect(profile, Zeroizing::new("test-only-passphrase".into()), path)
        .await
        .unwrap()
        .disconnect()
        .await;
}
