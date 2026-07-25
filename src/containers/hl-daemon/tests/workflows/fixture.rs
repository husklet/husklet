//! Deterministic executable image fixtures shared by workflow tests.

use sha2::{Digest, Sha256};
use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

type Error = Box<dyn std::error::Error>;

pub(super) const IMAGE: &str = "workflow/alpine:test";

/// Wrap the pinned Alpine minirootfs as a Docker save archive accepted by the
/// daemon's real image-load endpoint. The minirootfs itself is the OCI layer.
pub(super) fn alpine(work: &Path) -> Result<PathBuf, Error> {
    let source = env::var_os("HL_ALPINE_ARCHIVE")
        .map(PathBuf::from)
        .ok_or("HL_ALPINE_ARCHIVE must name the pinned Alpine minirootfs")?;
    let source_bytes = fs::read(&source)?;
    let mut layer = Vec::new();
    flate2::read::GzDecoder::new(&source_bytes[..]).read_to_end(&mut layer)?;
    let mut digest = String::from("sha256:");
    for byte in Sha256::digest(&layer) {
        write!(digest, "{byte:02x}")?;
    }
    let architecture = if source.to_string_lossy().contains("x86_64") {
        "amd64"
    } else {
        "arm64"
    };
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": architecture,
        "os": "linux",
        "config": {"Cmd": ["/bin/sh"], "WorkingDir": "/"},
        "rootfs": {"type": "layers", "diff_ids": [digest]}
    }))?;
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json",
        "RepoTags": [IMAGE],
        "Layers": ["layer.tar"]
    }]))?;
    let path = work.join("alpine-docker.tar");
    let mut archive = tar::Builder::new(fs::File::create(&path)?);
    append(&mut archive, "config.json", &config)?;
    append(&mut archive, "layer.tar", &layer)?;
    append(&mut archive, "manifest.json", &manifest)?;
    archive.finish()?;
    Ok(path)
}

/// Materialize the same pinned layer as an executable directory fixture for
/// headless container workflows that exercise the domain API directly.
pub(super) fn rootfs(work: &Path, name: &str) -> Result<PathBuf, Error> {
    let source = env::var_os("HL_ALPINE_ARCHIVE")
        .map(PathBuf::from)
        .ok_or("HL_ALPINE_ARCHIVE must name the pinned Alpine minirootfs")?;
    let root = work.join(name);
    let file = fs::File::open(source)?;
    let mut layer = hl_images::layer::Layer::new(flate2::read::GzDecoder::new(file));
    layer.apply(&root)?;
    Ok(root)
}

fn append<W: io::Write>(archive: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    archive.append_data(&mut header, name, bytes)
}
