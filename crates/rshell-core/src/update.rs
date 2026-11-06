//! Explicit, background-only release checks and verified package downloads.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

pub const RELEASES_URL: &str = "https://github.com/gooddayday009/rshell/releases";
const API: &str = "https://api.github.com/repos/gooddayday009/rshell/releases?per_page=20";
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub size: u64,
    pub installer: bool,
    url: String,
    digest: Option<String>,
    checksum_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub packages: Vec<Package>,
}

impl Release {
    pub fn is_newer_than(&self, current: &str) -> bool {
        version(&self.version) > version(current)
    }
}

fn version(text: &str) -> Option<[u64; 3]> {
    let parts: Vec<_> = text.strip_prefix('v').unwrap_or(text).split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some([
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ])
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(text: &str) -> Option<String> {
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

fn select_release(bytes: &[u8]) -> Result<Release> {
    let releases: Vec<ApiRelease> =
        serde_json::from_slice(bytes).context("GitHub 版本信息无法解析")?;
    let latest = releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease && version(&r.tag_name).is_some())
        .max_by_key(|r| version(&r.tag_name))
        .context("仓库暂未发布可识别的稳定版本")?;
    let number = latest.tag_name.trim_start_matches('v').to_owned();
    let base = format!("{RELEASES_URL}/download/{}/", latest.tag_name);
    let mut packages = Vec::new();
    for (suffix, installer) in package_suffixes() {
        let name = format!("RShell-{number}-{suffix}");
        let Some(asset) = latest.assets.iter().find(|a| a.name == name) else {
            continue;
        };
        ensure!(
            asset.browser_download_url == format!("{base}{name}"),
            "下载地址不属于指定仓库的版本资产"
        );
        ensure!(
            asset.size > 0 && asset.size <= MAX_PACKAGE,
            "版本包大小无效或超过 256 MiB"
        );
        let digest = asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .and_then(sha256);
        let checksum_url = latest
            .assets
            .iter()
            .find(|a| a.name == format!("{name}.sha256"))
            .map(|a| a.browser_download_url.clone());
        if let Some(url) = &checksum_url {
            ensure!(
                url == &format!("{base}{name}.sha256"),
                "校验文件地址不属于指定仓库"
            );
        }
        ensure!(
            digest.is_some() || checksum_url.is_some(),
            "版本包缺少 SHA-256 校验信息"
        );
        packages.push(Package {
            name,
            size: asset.size,
            installer: *installer,
            url: asset.browser_download_url.clone(),
            digest,
            checksum_url,
        });
    }
    ensure!(
        !packages.is_empty(),
        "最新版本未上传适用于当前系统和架构的安装包"
    );
    Ok(Release {
        version: number,
        packages,
    })
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("RShell/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}

async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    response = response
        .error_for_status()
        .context("GitHub 请求失败（请检查网络或 API 访问限额）")?;
    let mut result = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            result.len().saturating_add(chunk.len()) <= limit,
            "GitHub 响应超过大小限制"
        );
        result.extend_from_slice(&chunk);
    }
    Ok(result)
}

pub async fn check_latest() -> Result<Release> {
    let response = client()?
        .get(API)
        .timeout(Duration::from_secs(20))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    select_release(&bounded(response, 1024 * 1024).await?)
}

fn checksum(bytes: &[u8], name: &str) -> Result<String> {
    let text = std::str::from_utf8(bytes).context("校验文件不是 UTF-8 文本")?;
    let mut fields = text.split_whitespace();
    let hash = fields
        .next()
        .and_then(sha256)
        .context("SHA-256 校验文件格式错误")?;
    ensure!(
        fields.next().map(|s| s.trim_start_matches('*')) == Some(name) && fields.next().is_none(),
        "校验文件中的包名不匹配"
    );
    Ok(hash)
}

/// Returns only a complete, hash-verified file. Never launches an installer.
pub async fn download(
    package: Package,
    root: PathBuf,
    progress: impl Fn(u64) + Send,
) -> Result<PathBuf> {
    download_with_client(&client()?, package, &root, progress).await
}

async fn download_with_client(
    client: &reqwest::Client,
    package: Package,
    root: &Path,
    progress: impl Fn(u64) + Send,
) -> Result<PathBuf> {
    ensure!(
        Path::new(&package.name)
            .file_name()
            .and_then(|s| s.to_str())
            == Some(package.name.as_str()),
        "版本包文件名无效"
    );
    ensure!(
        package.size > 0 && package.size <= MAX_PACKAGE,
        "版本包大小无效"
    );
    let expected = match &package.checksum_url {
        Some(url) => {
            let response = client
                .get(url)
                .timeout(Duration::from_secs(20))
                .send()
                .await?;
            checksum(&bounded(response, 4096).await?, &package.name)?
        }
        None => package.digest.clone().context("缺少 SHA-256 校验信息")?,
    };
    if let Some(digest) = &package.digest {
        ensure!(&expected == digest, "GitHub 摘要与校验文件不一致");
    }
    tokio::fs::create_dir_all(root).await?;
    let root = root.to_owned();
    let stage = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new().prefix("package-").tempdir_in(root)
    })
    .await??;
    let path = stage.path().join(&package.name);
    let mut response = client.get(&package.url).send().await?.error_for_status()?;
    if let Some(size) = response.content_length() {
        ensure!(size == package.size, "下载包长度与版本信息不一致");
    }
    let mut file = tokio::fs::File::create(&path).await?;
    let mut received = 0_u64;
    let mut hash = Sha256::new();
    while let Some(chunk) = response.chunk().await? {
        received += chunk.len() as u64;
        ensure!(
            received <= package.size && received <= MAX_PACKAGE,
            "下载包超过大小限制"
        );
        hash.update(&chunk);
        file.write_all(&chunk).await?;
        progress(received);
    }
    file.flush().await?;
    drop(file);
    ensure!(received == package.size, "下载不完整，请重试");
    ensure!(
        hex(&hash.finalize()) == expected,
        "SHA-256 校验失败，未保留安装包"
    );
    // Keep the unique directory only after verification; partial downloads are cleaned up.
    tokio::fs::write(stage.path().join("verified.sha256"), &expected).await?;
    let _ = stage.keep();
    Ok(path)
}

#[cfg(any(windows, test))]
fn verify_installer(path: &Path, expected: &str) -> Result<()> {
    use std::io::Read;
    ensure!(
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("RShell-") && n.ends_with("-windows-x64-setup.exe")),
        "请选择已下载的 RShell 安装包"
    );
    ensure!(sha256(expected).is_some(), "安装包校验信息无效");
    let mut file = std::fs::File::open(path)?;
    ensure!(file.metadata()?.len() <= MAX_PACKAGE, "安装包超过大小限制");
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    ensure!(
        hex(&hash.finalize()) == expected,
        "安装包已改变，校验失败，请重新下载"
    );
    Ok(())
}

/// Background operation: start a separate helper that waits for this process to exit.
#[cfg(windows)]
pub fn launch_install(path: PathBuf) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let directory = path.parent().context("安装包目录无效")?;
    let expected = std::fs::read_to_string(directory.join("verified.sha256"))?;
    verify_installer(&path, &expected)?;
    let executable = std::env::current_exe()?;
    let helper = directory.join(format!("rshell-updater-{}.exe", uuid::Uuid::new_v4()));
    std::fs::copy(&executable, &helper)?;
    std::process::Command::new(helper)
        .arg("--apply-update")
        .arg(std::process::id().to_string())
        .arg(path)
        .arg(executable)
        .arg(expected)
        .creation_flags(0x08000000)
        .spawn()
        .context("无法启动更新助手")?;
    Ok(())
}

/// Runs only in the copied helper, outside the application/configuration lock.
#[cfg(windows)]
pub fn apply_update(args: &[std::ffi::OsString]) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    ensure!(args.len() == 4, "更新助手参数无效");
    let pid: u32 = args[0].to_str().context("进程号无效")?.parse()?;
    let package = PathBuf::from(&args[1]);
    let executable = PathBuf::from(&args[2]);
    let expected = args[3].to_str().context("校验信息无效")?;
    ensure!(executable.is_absolute(), "安装位置必须为绝对路径");
    let directory = executable.parent().context("安装位置无效")?;
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if !process.is_null() {
        let result = unsafe { WaitForSingleObject(process, 120_000) };
        unsafe {
            CloseHandle(process);
        }
        ensure!(result == WAIT_OBJECT_0, "等待 RShell 退出超时，未安装更新");
    } else {
        ensure!(
            std::io::Error::last_os_error().raw_os_error() == Some(87),
            "无法确认 RShell 已退出，未安装更新"
        );
    }
    verify_installer(&package, expected)?;
    // NSIS requires /D to be last and unquoted, including directories with spaces.
    let install_dir = directory.to_str().context("安装目录编码无效")?;
    ensure!(!install_dir.contains(['"', '\r', '\n']), "安装目录无效");
    let status = std::process::Command::new(package)
        .arg("/S")
        .raw_arg(format!("/D={install_dir}"))
        .status()?;
    ensure!(
        status.success(),
        "更新安装失败（{status}），请重新打开 RShell 后重试"
    );
    let restart = directory.join("rshell.exe");
    let mut command = std::process::Command::new(restart);
    if let Some(config) = std::env::var_os("RSHELL_CONFIG_DIR") {
        command.env("RSHELL_CONFIG_DIR", config);
    }
    command.spawn().context("安装完成，但重新启动失败")?;
    Ok(())
}

fn package_suffixes() -> &'static [(&'static str, bool)] {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            &[("macos-arm64.zip", false)]
        } else {
            &[("macos-x64.zip", false)]
        }
    } else {
        &[
            ("windows-x64-setup.exe", true),
            ("windows-x64-portable.zip", false),
        ]
    }
}
#[cfg(not(windows))]
pub fn launch_install(_path: PathBuf) -> Result<()> {
    anyhow::bail!("请退出 RShell 后手动替换应用程序")
}
#[cfg(not(windows))]
pub fn apply_update(_args: &[std::ffi::OsString]) -> Result<()> {
    anyhow::bail!("当前系统不使用 Windows 更新助手")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_or_non_installer_files_cannot_be_installed() {
        let root = tempfile::tempdir().unwrap();
        let installer = root.path().join("RShell-2.3.3-windows-x64-setup.exe");
        std::fs::write(&installer, b"verified fixture").unwrap();
        let digest = hex(&Sha256::digest(b"verified fixture"));
        verify_installer(&installer, &digest).unwrap();
        std::fs::write(&installer, b"changed fixture").unwrap();
        assert!(verify_installer(&installer, &digest).is_err());
        let archive = root.path().join("RShell-2.3.3-windows-x64-portable.zip");
        std::fs::write(&archive, b"verified fixture").unwrap();
        assert!(verify_installer(&archive, &digest).is_err());
    }

    fn release(tag: &str) -> serde_json::Value {
        let name = format!(
            "RShell-{}-{}",
            tag.trim_start_matches('v'),
            package_suffixes()[0].0
        );
        serde_json::json!({"tag_name":tag,"draft":false,"prerelease":false,"assets":[{
            "name":name,"size":3,"browser_download_url":format!("{RELEASES_URL}/download/{tag}/{name}"),
            "digest":format!("sha256:{}", "a".repeat(64))
        }]})
    }

    #[test]
    fn selects_numeric_stable_version_and_only_repository_assets() {
        let mut preview = release("9.0.0");
        preview["prerelease"] = true.into();
        let data = serde_json::to_vec(&vec![
            release("2.9.0"),
            release("v2.10.0"),
            preview,
            release("agent-sdk-1.0.0"),
        ])
        .unwrap();
        let latest = select_release(&data).unwrap();
        assert_eq!(latest.version, "2.10.0");
        assert!(latest.is_newer_than("2.9.0"));
        assert!(!latest.is_newer_than("2.10.0"));
        assert!(!latest.is_newer_than("3.0.0"));
        let mut invalid = release("2.3.3");
        invalid["assets"][0]["browser_download_url"] = "https://example.com/setup.exe".into();
        assert!(select_release(&serde_json::to_vec(&vec![invalid]).unwrap()).is_err());
        assert!(select_release(b"[]").is_err());
    }

    #[test]
    fn checksum_is_bound_to_exact_package_name() {
        let text = format!("{}  setup.exe\n", "A".repeat(64));
        assert_eq!(
            checksum(text.as_bytes(), "setup.exe").unwrap(),
            "a".repeat(64)
        );
        assert!(checksum(text.as_bytes(), "other.exe").is_err());
        assert!(checksum(b"bad setup.exe", "setup.exe").is_err());
    }

    #[tokio::test]
    async fn downloads_verified_bytes_and_removes_corrupt_packages() {
        use std::io::{Read, Write};
        for (corrupt, size) in [(false, 3), (true, 3), (false, 4)] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                assert!(stream.read(&mut request).unwrap() > 0);
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                    )
                    .unwrap();
            });
            let root = tempfile::tempdir().unwrap();
            let package = Package {
                name: "setup.exe".into(),
                size,
                installer: true,
                url: format!("http://{address}/setup.exe"),
                checksum_url: None,
                digest: Some(if corrupt {
                    "0".repeat(64)
                } else {
                    hex(&Sha256::digest(b"abc"))
                }),
            };
            let result =
                download_with_client(&reqwest::Client::new(), package, root.path(), |_| {}).await;
            server.join().unwrap();
            if corrupt || size != 3 {
                assert!(result.is_err());
                assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
            } else {
                assert_eq!(std::fs::read(result.unwrap()).unwrap(), b"abc");
            }
        }
    }

    #[tokio::test]
    async fn cancelled_download_does_not_leave_a_package() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\na")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let root = tempfile::tempdir().unwrap();
        let target = root.path().to_owned();
        let package = Package {
            name: "setup.exe".into(),
            size: 3,
            installer: true,
            url: format!("http://{address}/setup.exe"),
            checksum_url: None,
            digest: Some(hex(&Sha256::digest(b"abc"))),
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(async move {
            download_with_client(&reqwest::Client::new(), package, &target, move |_| {
                let _ = tx.try_send(());
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        server.abort();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
