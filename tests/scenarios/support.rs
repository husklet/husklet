//! Process and image plumbing owned by the repository end-to-end runner.

use hl_container::{Config, Containers};
use sha2::{Digest, Sha256};
use std::{fmt::Write as _, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::{sleep, timeout},
};

const TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) async fn containers_for(root: &Path) -> Result<Containers, hl_container::Error> {
    Containers::builder(Config::new(root.join("state"))).build().await
}

pub(crate) async fn wait_for_path(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        while !socket.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "embedded daemon socket startup timed out".into())
}

pub(crate) async fn raw_http(
    socket: &Path,
    request: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
    timeout(TIMEOUT, async {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(request).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        if response.len() > 1024 * 1024 {
            return Err("HTTP response exceeded one MiB".into());
        }
        String::from_utf8(response).map_err(Into::into)
    })
    .await
    .map_err(|_| "raw HTTP exchange timed out")?
}

pub(crate) fn write_image_archive(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut layer = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut layer);
        append_archive_member(&mut tar, "fixture.txt", b"scenario fixture\n")?;
        tar.finish()?;
    }
    let mut diff_id = String::from("sha256:");
    for byte in Sha256::digest(&layer) {
        write!(diff_id, "{byte:02x}")?;
    }
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64", "os": "linux",
        "config": {"Cmd": ["/bin/true"], "Env": ["SCENARIO=fixture"], "WorkingDir": "/"},
        "rootfs": {"type": "layers", "diff_ids": [diff_id]}
    }))?;
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json", "RepoTags": ["scenario/fixture:test"], "Layers": ["layer.tar"]
    }]))?;
    let mut outer = tar::Builder::new(std::fs::File::create(path)?);
    append_archive_member(&mut outer, "config.json", &config)?;
    append_archive_member(&mut outer, "layer.tar", &layer)?;
    append_archive_member(&mut outer, "manifest.json", &manifest)?;
    outer.finish()?;
    Ok(())
}

fn append_archive_member<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    archive.append_data(&mut header, name, bytes)
}

pub(crate) async fn unpack(
    archive: std::path::PathBuf,
    destination: std::path::PathBuf,
) -> Result<(), std::io::Error> {
    tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
        std::fs::create_dir(&destination)?;
        let file = std::fs::File::open(archive)?;
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(destination)?;
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}
