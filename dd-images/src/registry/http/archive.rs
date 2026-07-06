//! Subprocess helpers that shell out to `tar`/other tools: unpacking a gzipped-tar layer blob into a
//! flattened rootfs (with the macOS-specific device-node / read-only-dir recovery) and a small generic
//! `run` used across the registry module.

use crate::Error;
use std::path::Path;
use std::process::Command;

/// Unpack a gzipped-tar layer blob from `src` into `rootfs` (`tar xzf`), unprivileged, on macOS.
///
/// dd flattens every OCI layer into one shared `rootfs` with sequential `tar` runs (no overlayfs), so
/// two macOS-specific hazards break `docker pull` for images that pull fine everywhere else:
///
///  1. Device nodes. Base layers (mysql:8.4, amazonlinux:2023, oraclelinux, …) ship char/fifo specials
///     under `dev/` (dev/console, dev/null, dev/ptmx, …). Unprivileged `mknod` fails "Operation not
///     permitted" and tar exits non-zero. We `--exclude 'dev/*'` so tar never tries — containers get a
///     fresh /dev synthesized by the engine at runtime, so the static nodes are never used (this is
///     what Docker's own userspace unpackers do).
///  2. Read-only directories a *previous* layer left behind. e.g. mysql's oraclelinux base ships
///     `etc/pki/ca-trust/extracted/pem/directory-hash/` as `dr-xr-xr-x` full of symlinks; libarchive
///     defers dir-mode restore so the layer that *creates* it extracts fine, but a later layer that
///     overwrites a symlink inside it (or a re-pull) can't `unlink` in the now-write-less dir →
///     "Can't unlink already-existing object: Permission denied" → the whole layer aborted. We recover
///     by re-adding owner-write to every dir in the rootfs and extracting the layer again (libarchive
///     re-restores the archive's own dir modes for dirs this layer contains; a dir it doesn't touch
///     just keeps owner-write, harmless for a rootfs whose processes run as root).
///
/// Real corruption (truncated/damaged gzip, "Unexpected EOF", "not in gzip format", "No space left")
/// is never swallowed — those still fail the pull.
pub(in crate::registry) fn extract_targz(src: &Path, rootfs: &Path) -> Result<(), Error> {
    let attempt = || {
        Command::new("tar")
            .args(["--exclude", "dev/*", "--exclude", "./dev/*", "-xzf"])
            .arg(src)
            .arg("-C")
            .arg(rootfs)
            .output()
            .map_err(|e| Error::Archive(format!("tar: {e}")))
    };
    // Split tar's stderr into (needs a writable-dir retry?, fatal lines). Benign = unprivileged
    // mknod/ownership refusal or tar's trailing summary; retryable = a "Permission denied" overwrite
    // into a read-only dir; everything else is fatal.
    fn classify(stderr: &str) -> (bool, Vec<String>) {
        let (mut retry, mut fatal) = (false, Vec::new());
        for line in stderr.lines() {
            let l = line.trim();
            if l.is_empty()
                || l.contains("Operation not permitted")
                || l.contains("Cannot mknod")
                || l.contains("Error exit delayed from previous errors")
            {
                continue;
            }
            if l.contains("Permission denied") {
                retry = true;
                continue;
            }
            fatal.push(l.to_string());
        }
        (retry, fatal)
    }
    let out = attempt()?;
    if out.status.success() {
        return Ok(());
    }
    let (retry, fatal) = classify(&String::from_utf8_lossy(&out.stderr));
    if !fatal.is_empty() {
        return Err(Error::Archive(format!(
            "tar extract failed: {}",
            fatal.join("; ")
        )));
    }
    if !retry {
        return Ok(());
    } // only device-node noise — the layer's real content extracted fine
      // A read-only dir from an earlier layer is blocking this layer's overwrites: make every dir in the
      // rootfs owner-writable and extract the layer again.
    let _ = Command::new("find")
        .arg(rootfs)
        .args(["-type", "d", "-exec", "chmod", "u+w", "{}", "+"])
        .output();
    let out2 = attempt()?;
    if out2.status.success() {
        return Ok(());
    }
    let (_, fatal2) = classify(&String::from_utf8_lossy(&out2.stderr));
    if fatal2.is_empty() {
        Ok(())
    } else {
        Err(Error::Archive(format!(
            "tar extract failed after making dirs writable: {}",
            fatal2.join("; ")
        )))
    }
}

pub(in crate::registry) fn run(prog: &str, args: &[&str]) -> Result<String, Error> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| Error::Other(format!("{prog}: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = if stderr.trim().is_empty() {
            format!("exited with {}", out.status)
        } else {
            stderr.trim().to_string()
        };
        return Err(Error::Other(format!("{prog} {args:?} failed: {detail}")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
