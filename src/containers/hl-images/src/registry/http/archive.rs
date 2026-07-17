//! Subprocess helpers that shell out to `tar`/other tools: unpacking a gzipped-tar layer blob into a
//! flattened rootfs (with the macOS-specific device-node / read-only-dir recovery) and a small generic
//! `run` used across the registry module.

use crate::Error;
use std::path::Path;
use std::process::Command;

/// Unpack a gzipped-tar layer blob from `src` into `rootfs` (`tar xzf`), unprivileged, on macOS.
///
/// hl flattens every OCI layer into one shared `rootfs` with sequential `tar` runs (no overlayfs), so
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
    // CONTAINMENT: hl flattens every layer into ONE shared rootfs with sequential `tar` runs, so a
    // pre-existing symlink left by an earlier layer (e.g. `rootfs/linkout -> ../outside`) plus a later
    // entry that writes THROUGH it (`linkout/file.txt`) would make tar follow the symlink and write
    // `outside/file.txt` — outside the rootfs. Before extracting, remove any existing symlink sitting at
    // a directory-component position of an entry in THIS layer; tar then recreates a real directory
    // there (layers legitimately replace lower entries), and the write stays inside the rootfs.
    scrub_symlink_prefixes(src, rootfs);
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
    crate::registry::layer::make_dirs_writable(rootfs);
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

/// Remove any pre-existing SYMLINK that sits at a path-component position of an entry in the layer at
/// `src`, so `tar` can't be tricked into writing THROUGH it to outside the rootfs. Purely defensive and
/// contained: it walks each entry's components lexically (rejecting `..`/absolute escapes so the walk
/// itself never leaves the rootfs) and `unlink`s a symlink found at any prefix. A symlink the layer
/// itself ships is recreated by the subsequent extraction, so this never loses layer content.
fn scrub_symlink_prefixes(src: &Path, rootfs: &Path) {
    let out = match Command::new("tar").arg("tzf").arg(src).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return,
    };
    for line in String::from_utf8_lossy(&out).lines() {
        let rel = line
            .trim_end_matches('/')
            .trim_start_matches("./")
            .trim_start_matches('/');
        if rel.is_empty() {
            continue;
        }
        let mut cur = rootfs.to_path_buf();
        for comp in rel.split('/') {
            match comp {
                "" | "." => continue,
                ".." => break, // never walk above the rootfs for a malformed entry
                c => cur.push(c),
            }
            match std::fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => {
                    let _ = std::fs::remove_file(&cur);
                    // path no longer exists; deeper components can't be pre-existing symlinks.
                    break;
                }
                Ok(_) => {}      // a real dir/file — keep descending
                Err(_) => break, // nothing here (or below) yet
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "hl_archive_test_{}_{}_{}",
                tag,
                std::process::id(),
                n
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Finding 5: a pre-existing `rootfs/linkout -> ../outside` symlink plus a later layer entry
    // `linkout/file.txt` must NOT write outside the rootfs. Extraction must be contained: the file lands
    // under rootfs/linkout/ (a real dir), and nothing appears in the sibling `outside/`.
    #[test]
    fn extract_does_not_write_through_existing_symlink() {
        let t = Tmp::new("symthrough");
        let rootfs = t.0.join("rootfs");
        let outside = t.0.join("outside");
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // a base-layer symlink that escapes the rootfs.
        symlink("../outside", rootfs.join("linkout")).unwrap();

        // the next layer writes THROUGH linkout/.
        let layerdir = t.0.join("layer");
        std::fs::create_dir_all(layerdir.join("linkout")).unwrap();
        std::fs::write(layerdir.join("linkout").join("file.txt"), b"pwn").unwrap();
        let tar = t.0.join("layer.tar.gz");
        let st = Command::new("tar")
            .arg("-czf")
            .arg(&tar)
            .arg("-C")
            .arg(&layerdir)
            .arg(".")
            .status()
            .unwrap();
        assert!(st.success(), "build layer tar");

        extract_targz(&tar, &rootfs).unwrap();

        assert!(
            !outside.join("file.txt").exists(),
            "extraction must NOT write through the escaping symlink to outside the rootfs"
        );
        assert!(
            rootfs.join("linkout").join("file.txt").exists(),
            "the layer's file must land inside the rootfs (symlink replaced by a real dir)"
        );
        assert!(
            !rootfs
                .join("linkout")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "the escaping symlink must have been replaced by a real directory"
        );
    }
}
