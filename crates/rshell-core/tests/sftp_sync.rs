#[path = "support/headless.rs"]
mod server;
mod support;
use rshell_core::{
    headless::Session,
    sftp_sync::{Kind, Request},
};
use std::{path::Path, sync::atomic::Ordering, time::Duration};
use zeroize::Zeroizing;

async fn connect(server: &server::Server, dir: &Path) -> Session {
    Session::connect(
        server.profile.clone(),
        Zeroizing::new("test-only-secret".into()),
        dir.join("config.json"),
    )
    .await
    .unwrap()
}
fn request(local: &Path, remote: &str, upload: bool, dry_run: bool, delete: bool) -> Request {
    Request {
        local: local.to_path_buf(),
        remote: remote.into(),
        upload,
        dry_run,
        delete,
    }
}
fn seed(server: &server::Server, path: &str, bytes: &[u8]) {
    server
        .state
        .files
        .lock()
        .unwrap()
        .insert(path.into(), bytes.to_vec());
}

#[tokio::test]
async fn upload_dry_run_content_delta_empty_directories_and_explicit_mirror() {
    let server = server::start().await;
    let dir = support::tempdir();
    let local = dir.path().join("源目录");
    std::fs::create_dir_all(local.join("子目录/空目录")).unwrap();
    std::fs::write(local.join("子目录/数据.bin"), vec![0xA5; 100_001]).unwrap();
    std::fs::write(local.join("changed"), b"NEW").unwrap();
    std::fs::write(local.join("same"), b"same").unwrap();
    for (path, data) in [
        ("/target/", &b""[..]),
        ("/target/changed", b"OLD"),
        ("/target/same", b"same"),
        ("/target/extra/", b""),
        ("/target/extra/keep", b"keep"),
        ("/outside", b"safe"),
    ] {
        seed(&server, path, data);
    }
    let session = connect(&server, dir.path()).await;
    let before = server.state.files.lock().unwrap().clone();
    let plan = session
        .sync_directory(request(&local, "/target", true, true, true))
        .await
        .unwrap();
    assert_eq!(*server.state.files.lock().unwrap(), before);
    assert!(
        plan.operations
            .iter()
            .any(|op| op.path == "changed" && op.kind == Kind::Update)
    );
    assert!(
        plan.operations
            .iter()
            .any(|op| op.path == "same" && op.kind == Kind::Skip)
    );
    assert!(
        plan.operations
            .iter()
            .any(|op| op.path == "extra/keep" && op.kind == Kind::DeleteFile)
    );
    session
        .sync_directory(request(&local, "/target", true, false, false))
        .await
        .unwrap();
    {
        let files = server.state.files.lock().unwrap();
        assert_eq!(files["/target/changed"], b"NEW");
        assert_eq!(files["/target/子目录/数据.bin"], vec![0xA5; 100_001]);
        assert!(files.contains_key("/target/子目录/空目录/"));
        assert_eq!(files["/target/extra/keep"], b"keep");
    }
    let unchanged = session
        .sync_directory(request(&local, "/target", true, false, false))
        .await
        .unwrap();
    assert!(unchanged.operations.iter().all(|op| op.kind == Kind::Skip));
    session
        .sync_directory(request(&local, "/target", true, false, true))
        .await
        .unwrap();
    let files = server.state.files.lock().unwrap();
    assert!(!files.keys().any(|p| p.starts_with("/target/extra")));
    assert!(files.contains_key("/target/"));
    assert_eq!(files["/outside"], b"safe");
}

#[tokio::test]
async fn download_creates_root_updates_same_size_and_retains_extras_unless_requested() {
    let server = server::start().await;
    for (path, data) in [
        ("/source/", &b""[..]),
        ("/source/文件夹/", b""),
        ("/source/文件夹/空/", b""),
        ("/source/文件夹/data", b"NEW"),
    ] {
        seed(&server, path, data);
    }
    let dir = support::tempdir();
    let local = dir.path().join("missing");
    let session = connect(&server, dir.path()).await;
    session
        .sync_directory(request(&local, "/source", false, true, true))
        .await
        .unwrap();
    assert!(!local.exists());
    session
        .sync_directory(request(&local, "/source", false, false, false))
        .await
        .unwrap();
    assert!(local.join("文件夹/空").is_dir());
    std::fs::write(local.join("文件夹/data"), b"OLD").unwrap();
    std::fs::create_dir_all(local.join("extra/sub")).unwrap();
    std::fs::write(local.join("extra/sub/keep"), b"keep").unwrap();
    std::fs::write(dir.path().join("outside"), b"safe").unwrap();
    session
        .sync_directory(request(&local, "/source", false, false, false))
        .await
        .unwrap();
    assert_eq!(std::fs::read(local.join("文件夹/data")).unwrap(), b"NEW");
    assert!(local.join("extra/sub/keep").exists());
    session
        .sync_directory(request(&local, "/source", false, false, true))
        .await
        .unwrap();
    assert!(!local.join("extra").exists());
    assert_eq!(std::fs::read(dir.path().join("outside")).unwrap(), b"safe");
}

#[tokio::test]
async fn unsupported_atomic_replace_and_write_failure_preserve_target_and_extras() {
    let server = server::start().await;
    for (path, data) in [
        ("/target/", &b""[..]),
        ("/target/changed", b"OLD"),
        ("/target/extra", b"keep"),
    ] {
        seed(&server, path, data);
    }
    let dir = support::tempdir();
    let local = dir.path().join("source");
    std::fs::create_dir(&local).unwrap();
    std::fs::write(local.join("changed"), b"NEW").unwrap();
    let session = connect(&server, dir.path()).await;
    server
        .state
        .disable_posix_rename
        .store(true, Ordering::SeqCst);
    session
        .sync_directory(request(&local, "/target", true, true, true))
        .await
        .unwrap();
    let before = server.state.files.lock().unwrap().clone();
    assert!(
        session
            .sync_directory(request(&local, "/target", true, false, true))
            .await
            .is_err()
    );
    assert_eq!(*server.state.files.lock().unwrap(), before);
    server
        .state
        .disable_posix_rename
        .store(false, Ordering::SeqCst);
    server.state.fail_writes.store(true, Ordering::SeqCst);
    assert!(
        session
            .sync_directory(request(&local, "/target", true, false, true))
            .await
            .is_err()
    );
    let files = server.state.files.lock().unwrap();
    assert_eq!(files["/target/changed"], b"OLD");
    assert_eq!(files["/target/extra"], b"keep");
}

#[tokio::test]
async fn preflight_rejects_type_conflicts_case_collisions_links_unsafe_names_and_depth() {
    let server = server::start().await;
    let dir = support::tempdir();
    let local = dir.path().join("source");
    std::fs::create_dir(&local).unwrap();
    std::fs::write(local.join("clash"), b"file").unwrap();
    seed(&server, "/target/", b"");
    seed(&server, "/target/clash/", b"");
    let session = connect(&server, dir.path()).await;
    let before = server.state.files.lock().unwrap().clone();
    assert!(
        session
            .sync_directory(request(&local, "/target", true, false, true))
            .await
            .is_err()
    );
    assert_eq!(*server.state.files.lock().unwrap(), before);
    for names in [vec!["bad\\name"], vec!["CON"], vec!["Foo", "foo"]] {
        server
            .state
            .files
            .lock()
            .unwrap()
            .retain(|p, _| !p.starts_with("/bad/"));
        seed(&server, "/bad/", b"");
        for name in names {
            seed(&server, &format!("/bad/{name}"), b"");
        }
        assert!(
            session
                .sync_directory(request(&local, "/bad", false, true, false))
                .await
                .is_err()
        );
    }
    server.state.attributes.lock().unwrap().insert(
        "/target".into(),
        russh_sftp::protocol::FileAttributes {
            permissions: Some(0o120777),
            size: Some(0),
            ..Default::default()
        },
    );
    assert!(
        session
            .sync_directory(request(&local, "/target", true, true, false))
            .await
            .is_err()
    );
    assert!(
        session
            .sync_directory(request(&local, "/", true, true, true))
            .await
            .is_err()
    );
    assert!(
        session
            .sync_directory(request(&local, "/target/../escape", true, false, true))
            .await
            .is_err()
    );
    assert!(
        session
            .sync_directory(request(
                &local.join("../source"),
                "/new",
                true,
                false,
                false
            ))
            .await
            .is_err()
    );
    let deep = local.join(std::iter::repeat_n("d", 33).collect::<Vec<_>>().join("/"));
    std::fs::create_dir_all(deep).unwrap();
    assert!(
        session
            .sync_directory(request(&local, "/new", true, false, false))
            .await
            .is_err()
    );
    assert!(!server.state.files.lock().unwrap().contains_key("/new/"));
}

#[tokio::test]
async fn source_change_during_upload_stops_publication_and_mirror_deletion() {
    let server = server::start().await;
    seed(&server, "/slow-sync/", b"");
    seed(&server, "/slow-sync/data", b"old");
    seed(&server, "/slow-sync/extra", b"keep");
    let dir = support::tempdir();
    let local = dir.path().join("source");
    std::fs::create_dir(&local).unwrap();
    std::fs::write(local.join("data"), vec![1; 262144]).unwrap();
    let session = connect(&server, dir.path()).await;
    let req = request(&local, "/slow-sync", true, false, true);
    let task = tokio::spawn(async move { session.sync_directory(req).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if server
                .state
                .files
                .lock()
                .unwrap()
                .keys()
                .any(|p| p.contains(".rshell-sync-"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(local.join("data"), vec![2; 262144]).unwrap();
    assert!(task.await.unwrap().is_err());
    let files = server.state.files.lock().unwrap();
    assert_eq!(files["/slow-sync/data"], b"old");
    assert_eq!(files["/slow-sync/extra"], b"keep");
}

#[tokio::test]
async fn cancelled_sync_never_replaces_old_file_or_starts_mirror_deletion() {
    let server = server::start().await;
    seed(&server, "/slow-cancel/", b"");
    seed(&server, "/slow-cancel/data", b"old");
    seed(&server, "/slow-cancel/extra", b"keep");
    let dir = support::tempdir();
    let local = dir.path().join("source");
    std::fs::create_dir(&local).unwrap();
    std::fs::write(local.join("data"), vec![1; 262144]).unwrap();
    let session = connect(&server, dir.path()).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            session.sync_directory(request(&local, "/slow-cancel", true, false, true))
        )
        .await
        .is_err()
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let files = server.state.files.lock().unwrap();
    assert_eq!(files["/slow-cancel/data"], b"old");
    assert_eq!(files["/slow-cancel/extra"], b"keep");
}
