//! OCI/Docker archive fixtures shared by daemon integration tests.

use sha2::{Digest, Sha256};
use std::{fmt::Write as _, path::Path};

pub(crate) fn write_image_archive(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut layer = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut layer);
        let payload = b"scenario fixture\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, "fixture.txt", &payload[..])?;
        tar.finish()?;
    }
    let mut diff_id = String::from("sha256:");
    for byte in Sha256::digest(&layer) {
        write!(diff_id, "{byte:02x}")?;
    }
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {"Cmd": ["/bin/true"], "Env": ["SCENARIO=fixture"], "WorkingDir": "/"},
        "rootfs": {"type": "layers", "diff_ids": [diff_id]}
    }))?;
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json",
        "RepoTags": ["scenario/fixture:test"],
        "Layers": ["layer.tar"]
    }]))?;
    let file = std::fs::File::create(path)?;
    let mut outer = tar::Builder::new(file);
    append_archive_member(&mut outer, "config.json", &config)?;
    append_archive_member(&mut outer, "layer.tar", &layer)?;
    append_archive_member(&mut outer, "manifest.json", &manifest)?;
    outer.finish()?;
    Ok(())
}

pub(crate) fn append_archive_member<W: std::io::Write>(
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
