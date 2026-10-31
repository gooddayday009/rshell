//! Pinned, on-demand Agent component. All work is called from the Agent background task.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::AsyncWriteExt;

const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_UNPACKED: u64 = 768 * 1024 * 1024;
const EXE: &str = "agent-sdk/rshell-agent-sdk.exe";
static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Deserialize)]
struct Component {
    version: String,
    url: String,
    sha256: String,
    size: u64,
}
impl Component {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.version.is_empty()
                && !self.version.starts_with('.')
                && !self.version.ends_with('.')
                && self.version.len() <= 64
                && self
                    .version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
            "Agent 组件版本无效"
        );
        ensure!(
            self.sha256.len() == 64 && self.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "Agent 组件校验配置无效"
        );
        ensure!(
            self.size > 0 && self.size <= MAX_ARCHIVE,
            "Agent 组件大小配置无效"
        );
        let url = reqwest::Url::parse(&self.url)?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("github.com")
                && url.username().is_empty()
                && url.password().is_none(),
            "Agent 组件须来自配置的 GitHub HTTPS Release"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct ComponentInfo {
    pub version: String,
    pub size: u64,
}
pub fn component_info() -> Result<ComponentInfo> {
    let spec: Component = serde_json::from_str(include_str!("../../../agent-sdk/component.json"))?;
    spec.validate()?;
    Ok(ComponentInfo {
        version: spec.version,
        size: spec.size,
    })
}

/// Read-only lookup. Does not create directories, download or execute anything.
pub async fn find_component(bundled: &Path) -> Result<Option<PathBuf>> {
    if tokio::fs::metadata(bundled)
        .await
        .is_ok_and(|m| m.is_file())
    {
        return Ok(Some(bundled.to_owned()));
    }
    let spec: Component = serde_json::from_str(include_str!("../../../agent-sdk/component.json"))?;
    spec.validate()?;
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Ok(None);
    };
    let cache = PathBuf::from(local)
        .join("RShell/components")
        .join(&spec.version);
    tokio::task::spawn_blocking(move || {
        Ok(if installed(&cache, &spec) {
            Some(cache.join(EXE))
        } else {
            None
        })
    })
    .await?
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn installed(path: &Path, spec: &Component) -> bool {
    let marker = path.join("archive.sha256");
    path.join(EXE).is_file()
        && std::fs::metadata(&marker).is_ok_and(|m| m.len() == 64)
        && std::fs::read_to_string(marker).is_ok_and(|s| s == spec.sha256)
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// An existing offline component remains usable. Missing components are installed per user.
pub async fn ensure_component(
    bundled: PathBuf,
    progress: impl Fn(String) + Send + Sync + 'static,
) -> Result<PathBuf> {
    if tokio::fs::metadata(&bundled)
        .await
        .is_ok_and(|m| m.is_file())
    {
        return Ok(bundled);
    }
    let spec: Component = serde_json::from_str(include_str!("../../../agent-sdk/component.json"))?;
    spec.validate()?;
    let root =
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("无法定位当前用户的组件目录")?)
            .join("RShell/components");
    let _lock = INSTALL.lock().await;
    let cache = root.join(&spec.version);
    let check = cache.clone();
    let description = spec.clone();
    if tokio::task::spawn_blocking(move || installed(&check, &description)).await? {
        return Ok(cache.join(EXE));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("RShell/", env!("CARGO_PKG_VERSION")))
        .build()?;
    install(&client, &spec, &root, progress)
        .await
        .context("Agent 组件下载/安装失败；可重试，或在 Agent 设置中指定离线组件路径")
}

async fn install(
    client: &reqwest::Client,
    spec: &Component,
    root: &Path,
    progress: impl Fn(String) + Send + Sync + 'static,
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(root).await?;
    let root_copy = root.to_owned();
    let stage = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix(".download-")
            .tempdir_in(root_copy)
    })
    .await??;
    progress(format!(
        "首次使用 Agent，正在下载组件（约 {:.0} MiB，可停止后重试）…",
        spec.size as f64 / 1048576.0
    ));
    let mut response = client.get(&spec.url).send().await?.error_for_status()?;
    if let Some(length) = response.content_length() {
        ensure!(length == spec.size, "组件下载大小与固定版本不符");
    }
    let archive = stage.path().join("component.zip");
    let mut output = tokio::fs::File::create(&archive).await?;
    let mut hash = Sha256::new();
    let mut received = 0_u64;
    let mut last_percent = 0;
    while let Some(bytes) = response.chunk().await? {
        received += bytes.len() as u64;
        ensure!(
            received <= spec.size && received <= MAX_ARCHIVE,
            "组件下载超过大小限制"
        );
        hash.update(&bytes);
        output.write_all(&bytes).await?;
        let percent = received * 100 / spec.size;
        if percent >= last_percent + 5 {
            progress(format!(
                "正在下载 Agent 组件：{percent}%（{:.1}/{:.1} MiB）",
                received as f64 / 1048576.0,
                spec.size as f64 / 1048576.0
            ));
            last_percent = percent;
        }
    }
    output.flush().await?;
    drop(output);
    ensure!(received == spec.size, "组件下载不完整，请重试");
    ensure!(
        hex(&hash.finalize()) == spec.sha256,
        "Agent 组件 SHA-256 校验失败，未安装或执行"
    );
    progress("Agent 组件校验通过，正在解压安装…".into());
    let cancel = CancelOnDrop(Arc::new(AtomicBool::new(false)));
    let signal = cancel.0.clone();
    let description = spec.clone();
    let root = root.to_owned();
    let executable = tokio::task::spawn_blocking(move || {
        let destination = stage.path().join("payload");
        extract(&archive, &destination, &signal)?;
        ensure!(!signal.load(Ordering::Relaxed), "Agent 组件安装已取消");
        std::fs::write(destination.join("archive.sha256"), &description.sha256)?;
        let final_path = root.join(&description.version);
        if let Err(error) = std::fs::rename(&destination, &final_path) {
            // Another process may have completed the same pinned download.
            if !installed(&final_path, &description) {
                return Err(error).context("组件目录已存在或不可写；请检查组件缓存目录");
            }
        }
        Ok::<_, anyhow::Error>(final_path.join(EXE))
    })
    .await??;
    progress("Agent 组件已就绪，正在启动…".into());
    Ok(executable)
}

fn relative_name(name: &str) -> Result<PathBuf> {
    let normalized = name.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    let parts: Vec<_> = normalized.split('/').collect();
    ensure!(
        parts.len() >= 2 && parts[0] == "agent-sdk",
        "组件 ZIP 路径超出 agent-sdk 目录"
    );
    for part in &parts {
        ensure!(!part.is_empty() && !matches!(*part, "." | "..") && !part.ends_with(['.', ' ']) && !part.chars().any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*')), "组件 ZIP 含不安全路径");
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit()),
            "组件 ZIP 含保留设备路径"
        );
    }
    Ok(parts.iter().collect())
}

fn extract(archive: &Path, destination: &Path, cancelled: &AtomicBool) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    ensure!(zip.len() <= 4096, "组件 ZIP 文件数超过限制");
    std::fs::create_dir(destination)?;
    let mut total = 0_u64;
    let mut hashes = BTreeMap::new();
    let mut names = std::collections::HashSet::new();
    for index in 0..zip.len() {
        ensure!(!cancelled.load(Ordering::Relaxed), "Agent 组件安装已取消");
        let mut entry = zip.by_index(index)?;
        ensure!(
            entry
                .unix_mode()
                .is_none_or(|mode| mode & 0o170000 != 0o120000),
            "组件 ZIP 不允许符号链接"
        );
        if entry.is_dir() && matches!(entry.name(), "agent-sdk/" | "agent-sdk\\") {
            continue;
        }
        let relative = relative_name(entry.name())?;
        let normalized = relative.to_string_lossy().replace('\\', "/");
        ensure!(
            names.insert(normalized.to_ascii_lowercase()),
            "组件 ZIP 含重复路径"
        );
        let path = destination.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        total = total
            .checked_add(entry.size())
            .context("组件解压大小溢出")?;
        ensure!(total <= MAX_UNPACKED, "组件解压大小超过限制");
        std::fs::create_dir_all(path.parent().context("无效的组件路径")?)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let declared = entry.size();
        let mut written = 0_u64;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            ensure!(!cancelled.load(Ordering::Relaxed), "Agent 组件安装已取消");
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            written += n as u64;
            ensure!(written <= declared, "组件解压文件超过声明大小");
            hash.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
        }
        ensure!(written == declared, "组件解压文件不完整");
        if normalized != "agent-sdk/manifest.json" {
            hashes.insert(
                normalized["agent-sdk/".len()..].to_owned(),
                hex(&hash.finalize()),
            );
        }
    }
    let manifest = destination.join("agent-sdk/manifest.json");
    ensure!(
        std::fs::metadata(&manifest)?.len() <= 1024 * 1024,
        "组件 manifest 超过大小限制"
    );
    #[derive(Deserialize)]
    struct Manifest {
        protocol_version: u32,
        sha256: BTreeMap<String, String>,
    }
    let manifest: Manifest = serde_json::from_reader(std::fs::File::open(manifest)?)?;
    ensure!(
        manifest.protocol_version == 1 && hashes == manifest.sha256,
        "Agent 组件文件清单或协议校验失败"
    );
    ensure!(destination.join(EXE).is_file(), "Agent 组件缺少执行文件");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn fixture(extra: Option<&str>, bad_manifest: bool) -> Vec<u8> {
        let exe = b"synthetic component fixture, never executed";
        let checksum = if bad_manifest {
            "0".repeat(64)
        } else {
            hex(&Sha256::digest(exe))
        };
        let manifest =
            serde_json::json!({"protocol_version":1,"sha256":{"rshell-agent-sdk.exe":checksum}})
                .to_string();
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file(EXE, options).unwrap();
        archive.write_all(exe).unwrap();
        archive
            .start_file("agent-sdk/manifest.json", options)
            .unwrap();
        archive.write_all(manifest.as_bytes()).unwrap();
        if let Some(name) = extra {
            archive.start_file(name, options).unwrap();
            archive.write_all(b"unexpected").unwrap();
        }
        archive.finish().unwrap().into_inner()
    }
    fn root() -> tempfile::TempDir {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/component-tests");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::tempdir_in(root).unwrap()
    }
    fn spec(bytes: &[u8], url: String) -> Component {
        Component {
            version: "test-1".into(),
            url,
            sha256: hex(&Sha256::digest(bytes)),
            size: bytes.len() as u64,
        }
    }
    async fn serve(bytes: Vec<u8>, declared: u64) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/component.zip", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await;
            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await;
            let _ = stream.write_all(&bytes).await;
        });
        (url, task)
    }
    #[test]
    fn pinned_release_descriptor_is_valid() {
        let spec: Component =
            serde_json::from_str(include_str!("../../../agent-sdk/component.json")).unwrap();
        spec.validate().unwrap();
        let mut invalid = spec.clone();
        invalid.version = "..".into();
        assert!(invalid.validate().is_err());
        invalid = spec;
        invalid.url = "http://github.com/not-https".into();
        assert!(invalid.validate().is_err());
    }
    #[tokio::test]
    async fn complete_download_is_validated_and_atomically_cached() {
        let root = root();
        let bytes = fixture(None, false);
        let (url, server) = serve(bytes.clone(), bytes.len() as u64).await;
        let spec = spec(&bytes, url);
        let path = install(&reqwest::Client::new(), &spec, root.path(), |_| {})
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(path, root.path().join("test-1").join(EXE));
        assert!(installed(&root.path().join("test-1"), &spec));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        let mut other = spec;
        other.sha256 = "0".repeat(64);
        assert!(!installed(&root.path().join("test-1"), &other));
    }
    #[tokio::test]
    async fn checksum_and_length_errors_never_install() {
        for wrong_length in [false, true] {
            let root = root();
            let bytes = fixture(None, false);
            let (url, server) =
                serve(bytes.clone(), bytes.len() as u64 + u64::from(wrong_length)).await;
            let mut spec = spec(&bytes, url);
            if !wrong_length {
                spec.sha256 = "0".repeat(64);
            }
            assert!(
                install(&reqwest::Client::new(), &spec, root.path(), |_| {})
                    .await
                    .is_err()
            );
            server.await.unwrap();
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }
    #[tokio::test]
    async fn interrupted_download_removes_staging_and_can_retry() {
        let root = root();
        let bytes = fixture(None, false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let spec = spec(
            &bytes,
            format!("http://{}/component.zip", listener.local_addr().unwrap()),
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        let length = bytes.len();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&bytes[..10]).await.unwrap();
            let _ = tx.send(());
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let target = root.path().to_owned();
        let task =
            tokio::spawn(
                async move { install(&reqwest::Client::new(), &spec, &target, |_| {}).await },
            );
        rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        task.abort();
        let _ = task.await;
        server.abort();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        let bytes = fixture(None, false);
        let (url, server) = serve(bytes.clone(), bytes.len() as u64).await;
        install(
            &reqwest::Client::new(),
            &super::tests::spec(&bytes, url),
            root.path(),
            |_| {},
        )
        .await
        .unwrap();
        server.await.unwrap();
    }
    #[test]
    fn extraction_rejects_traversal_devices_extra_files_and_bad_manifest() {
        for bad in [
            "../escape",
            "agent-sdk/../../escape",
            "agent-sdk/C:/escape",
            "agent-sdk/CON.txt",
            "agent-sdk/..\\escape",
            "agent-sdk/unlisted.txt",
            "agent-sdk/RSHELL-AGENT-SDK.EXE",
        ] {
            let root = root();
            let archive = root.path().join("test.zip");
            std::fs::write(&archive, fixture(Some(bad), false)).unwrap();
            assert!(
                extract(
                    &archive,
                    &root.path().join("payload"),
                    &AtomicBool::new(false)
                )
                .is_err(),
                "{bad}"
            );
        }
        let root = root();
        let archive = root.path().join("test.zip");
        std::fs::write(&archive, fixture(None, true)).unwrap();
        assert!(
            extract(
                &archive,
                &root.path().join("payload"),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn existing_offline_component_does_not_download() {
        let root = root();
        let executable = root.path().join("rshell-agent-sdk.exe");
        std::fs::write(&executable, b"fixture").unwrap();
        assert_eq!(
            ensure_component(executable.clone(), |_| panic!("must not download"))
                .await
                .unwrap(),
            executable
        );
    }
}
