//! Opt-in GitHub update verification; never launches downloaded packages.
use rshell_core::update;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let release = update::check_latest().await?;
    println!("Latest stable: {}", release.version);
    for package in &release.packages {
        println!("{} ({} bytes)", package.name, package.size);
    }
    if std::env::args().any(|arg| arg == "--download") {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/update-probe");
        for package in release.packages {
            let path = update::download(package, root.clone(), |_| {}).await?;
            println!("SHA-256 verified: {}", path.display());
        }
    }
    Ok(())
}
