//! Deterministic executable image fixtures shared by workflow tests.

use hl_container::Guest;
use hl_images::Platform;
use sha2::{Digest, Sha256};
use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

type Error = Box<dyn std::error::Error>;

pub(crate) const IMAGE: &str = "workflow/alpine:test";

/// The guest architecture the pinned minirootfs actually contains.
///
/// Every workflow here executes the archive named by `HL_ALPINE_ARCHIVE`, and the flake
/// pins a different one per host: `alpine-minirootfs-*-aarch64` in the Darwin dev shell,
/// `*-x86_64` on `x86_64` Linux. Nothing downstream infers it -- `Guest::default()` is
/// `Aarch64` and `Daemon::new` starts at `Platform::linux_arm64()` -- so the fixture that
/// chooses the rootfs is the only place that knows, and it must say so to both.
///
/// # Panics
/// Panics when `HL_ALPINE_ARCHIVE` is unset or does not name a recognised architecture.
/// Guessing is what hid this: the previous form fell back to `arm64` for every name that
/// did not spell `x86_64`, which agreed with both defaults on an arm64 Mac and agreed with
/// neither once the host became `x86_64` Linux.
pub(crate) fn platform() -> Platform {
    let source = env::var_os("HL_ALPINE_ARCHIVE")
        .map(PathBuf::from)
        .expect("HL_ALPINE_ARCHIVE must name the pinned Alpine minirootfs");
    let name = source.to_string_lossy().into_owned();
    if name.contains("x86_64") || name.contains("amd64") {
        Platform::linux_amd64()
    } else if name.contains("aarch64") || name.contains("arm64") {
        Platform::linux_arm64()
    } else {
        panic!("HL_ALPINE_ARCHIVE {name:?} does not name an architecture this workflow can execute");
    }
}

/// The engine guest ISA that matches [`platform`], for the headless workflows that build a
/// [`hl_container::ContainerSpec`] directly and therefore never pass through image resolution.
pub(crate) fn guest() -> Guest {
    Guest::for_platform(&platform()).expect("the pinned minirootfs names a supported guest ISA")
}

/// Wrap the pinned Alpine minirootfs as a Docker save archive accepted by the
/// daemon's real image-load endpoint. The minirootfs itself is the OCI layer.
pub(crate) fn alpine(work: &Path) -> Result<PathBuf, Error> {
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
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": platform().architecture,
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
pub(crate) fn rootfs(work: &Path, name: &str) -> Result<PathBuf, Error> {
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
