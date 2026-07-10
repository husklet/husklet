//! The archive side of the store: `docker save` / `docker load` / `docker import`.
//!
//! dd's archive format is intentionally simple (not full OCI): a tar whose top level is the image's
//! `rootfs/` directory plus a [`Manifest`] sidecar (`dd-manifest.json`). [`Store::save_archive`] produces
//! it, [`Store::load_archive`] consumes it; [`Store::import_rootfs`] instead takes a bare rootfs tar (no
//! manifest) whose files land directly in a new image's rootfs. All tar work shells out to the system
//! `tar` (no crate dependency, no runtime) so the on-disk layout matches Docker/dd exactly.
//!
//! Split by operation across sibling files: [`Store::save_archive`] (`save.rs`),
//! [`Store::load_archive`] (`load.rs`), [`Store::import_rootfs`] (`import.rs`). This module holds the
//! shared [`LoadedImage`], the `uniq()` temp-suffix helper, and the `dir_for` / `remove_image_dir` /
//! `write_sidecar` store helpers those operations share.

use super::*;
use crate::Error;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// GNU-tar flags for EXTRACTING a load/import archive: `--xattrs` round-trips extended attributes,
/// `--numeric-owner`/`--same-owner`/`-p` preserve the numeric uid/gid + permission bits (effective only
/// when the daemon runs privileged; a warning otherwise). Device-node / ownership refusals are tolerated
/// by [`run_extract`], so an unprivileged extract still lands every regular file.
pub(super) const EXTRACT_FLAGS: &[&str] = &["--xattrs", "--numeric-owner", "--same-owner", "-p"];

/// GNU-tar flags for CREATING a save archive: `--format=posix` (pax) keeps nanosecond mtimes,
/// `--xattrs` round-trips extended attributes, `--sparse` keeps holes from expanding on disk/wire.
pub(super) const SAVE_FLAGS: &[&str] = &["--format=posix", "--xattrs", "--sparse"];

/// True for a stderr line from `tar` that is a benign, non-fatal warning during an UNPRIVILEGED extract:
/// device-node `mknod`, ownership/permission/xattr restoration refusals, and tar's trailing
/// "Error exit delayed" summary. Real corruption ("not a tar archive", "Unexpected EOF", "No space
/// left", …) is never benign, so it still fails the extract.
fn benign_extract_line(l: &str) -> bool {
    l.is_empty()
        || l.contains("Cannot mknod")
        || l.contains("Cannot create symlink")
        || l.contains("Operation not permitted")
        || l.contains("Cannot change ownership")
        || l.contains("Cannot change mode")
        || l.contains("Cannot set ")
        || l.contains("Cannot utime")
        || l.contains("Cannot restore")
        || l.contains("Error exit delayed")
        // tar's trailing summary; only benign because every REAL error also prints its own fatal line.
        || l.contains("Exiting with failure status")
}

/// Extract `archive` into `dest` with [`EXTRACT_FLAGS`], tolerating unprivileged device-node / ownership
/// noise (a valid device-node tar must not abort the whole extract). Returns `Err` only on a genuinely
/// fatal tar failure (bad archive, I/O error), so every regular file still lands.
pub(super) fn run_extract(archive: &Path, dest: &Path) -> Result<(), Error> {
    run_extract_args(archive, dest, &[])
}

/// Like [`run_extract`] but with `extra` tar arguments inserted before `-xf` (e.g. `--exclude` patterns
/// used to drop AUFS/OCI whiteout markers when flattening docker-save layers).
pub(super) fn run_extract_args(archive: &Path, dest: &Path, extra: &[&str]) -> Result<(), Error> {
    let out = Command::new("tar")
        .args(EXTRACT_FLAGS)
        .args(extra)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| Error::Archive(e.to_string()))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let fatal: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !benign_extract_line(l))
        .collect();
    if fatal.is_empty() {
        Ok(())
    } else {
        Err(Error::Archive(fatal.join("; ")))
    }
}

// Each op file adds `impl Store { … }` methods (`save_archive` / `load_archive` / `import_rootfs`)
// reached via the `Store` type, so declaring the modules is all that's needed to compile them in —
// there are no free items to re-export.
mod import;
mod load;
mod save;

// Per-process monotonic suffix so concurrent save/load/import requests in ONE daemon don't collide on
// the same `<pid>` temp path (two in-flight loads sharing `dd-load-<pid>.tar` / `.load-<pid>` would
// overwrite each other or `remove_dir_all` the dir mid-extract). Mirrors registry/http.rs's SEQ.
static SEQ: AtomicU64 = AtomicU64::new(0);
fn uniq() -> String {
    format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed))
}

/// An image materialized into the store by [`Store::load_archive`] / [`Store::import_rootfs`]: its unpacked
/// `rootfs`, detected [`Arch`], and run config recovered from the archive's manifest. Plain data — the
/// caller maps it onto its own image model. `healthcheck` is the raw OCI/docker JSON (or `None`).
#[derive(Clone, Debug)]
pub struct LoadedImage {
    /// The image reference (`repository:tag`).
    pub name: String,
    /// The unpacked root filesystem placed under the store.
    pub rootfs: PathBuf,
    /// The target detected from the manifest / rootfs.
    pub arch: Arch,
    /// The default command (never empty — falls back to `bash`/`/bin/sh`).
    pub cmd: Vec<String>,
    /// The environment (`K=V` lines).
    pub env: Vec<String>,
    /// The entrypoint (prepended to the command).
    pub entrypoint: Vec<String>,
    /// The working directory (empty if unset).
    pub workdir: String,
    /// The default run user (empty if unset).
    pub user: String,
    /// The exposed-port keys (e.g. `"5432/tcp"`).
    pub exposed_ports: Vec<String>,
    /// The `docker stop` signal (`Config.StopSignal`); empty ⇒ SIGTERM.
    pub stop_signal: String,
    /// The dirs that get an anonymous volume at run (`Config.Volumes` keys).
    pub img_volumes: Vec<String>,
    /// The container healthcheck probe as raw JSON (`None` ⇒ no probe recorded).
    pub healthcheck: Option<Value>,
}

impl Store {
    /// The store directory for an image `name`. The name is reduced to a SINGLE, injective, path-safe
    /// component via [`encode_store_component`](crate::image::config::encode_store_component) — `/` and `:`
    /// become `%2F`/`%3A` (so distinct refs like `owner/app:1_2` and `owner/app_1:2` no longer collide),
    /// and because the encoded component contains no `/` and is never `.`/`..`, a hostile manifest name
    /// like `../../evil` can NOT escape the store (finding: load's remove-before-rename must stay contained).
    /// This is the RAW-name layout the load/import paths use (distinct from [`safe_name`]'s
    /// canonicalized-reference layout used by pull — but both now share the same reversible encoding).
    fn dir_for(&self, name: &str) -> PathBuf {
        PathBuf::from(format!("{}/{}", self.dir, crate::image::config::encode_store_component(name)))
    }

    /// Install a fully-staged image directory `staged` at `target`, swapping any existing image aside so
    /// an interrupted/failed install leaves the OLD image intact (containers may still be using its rootfs).
    /// The steps are: if `target` exists, `rename` it to a sibling `.old-<uniq>`; `rename(staged, target)`;
    /// only on success remove the aside copy. If the final rename fails, the aside is renamed back so the
    /// previous image survives. `staged` and `target` must live on the same filesystem (both under the
    /// store), so each `rename` is atomic. Never removes the existing target before the new content is in
    /// place (finding: same-name load must not destroy a rootfs older containers still use).
    fn install_dir(&self, staged: &Path, target: &Path) -> Result<(), Error> {
        if target.exists() {
            let aside = PathBuf::from(format!("{}/.old-{}", self.dir, uniq()));
            std::fs::rename(target, &aside).map_err(|e| Error::Archive(e.to_string()))?;
            match std::fs::rename(staged, target) {
                Ok(()) => {
                    let _ = std::fs::remove_dir_all(&aside);
                    Ok(())
                }
                Err(e) => {
                    // Restore the previous image; it was never destroyed.
                    let _ = std::fs::rename(&aside, target);
                    Err(Error::Archive(e.to_string()))
                }
            }
        } else {
            std::fs::rename(staged, target).map_err(|e| Error::Archive(e.to_string()))
        }
    }

    /// Remove an image's on-disk directory (`<store>/<safe>/`, the parent of its `rootfs/`). Guarded to the
    /// writable store: a rootfs under a read-only bundled starter dir (or anywhere outside the store root)
    /// is left untouched so removing a discovered alias can't wipe shipped images.
    /// Returns `Ok(())` when the dir was removed OR is guarded/absent (a no-op); `Err` only when an actual
    /// removal of an in-store dir failed, so the caller can keep image state (retryable) and report an error.
    pub fn remove_image_dir(&self, rootfs: &str) -> std::io::Result<()> {
        let Some(dir) = Path::new(rootfs).parent() else {
            return Ok(());
        };
        let base = Path::new(&self.dir);
        if dir != base && dir.starts_with(base) && dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// Write the `dd-image.json` sidecar for a freshly loaded image so discovery restores its run config
    /// after a daemon restart (mirrors the fields the pull path records).
    fn write_sidecar(&self, target: &Path, img: &LoadedImage, darwin: bool) {
        let mut dd = json!({
            "name": img.name, "cmd": img.cmd, "env": img.env, "entrypoint": img.entrypoint,
            "workdir": img.workdir, "user": img.user, "exposed_ports": img.exposed_ports,
            "stop_signal": img.stop_signal, "img_volumes": img.img_volumes,
            "healthcheck": img.healthcheck,
        });
        if darwin {
            dd["os"] = json!("darwin");
        }
        let _ = std::fs::write(target.join("dd-image.json"), dd.to_string());
    }
}

/// Shared scratch/build helpers for the archive round-trip tests, imported by the per-operation test
/// modules in `save.rs` / `load.rs` / `import.rs` (they need a common temp-dir + fake-rootfs toolkit).
#[cfg(test)]
pub(super) mod testutil {
    use std::path::{Path, PathBuf};

    /// A unique scratch dir under the system temp dir.
    pub(crate) fn unique_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("dd-images-test-{}-{}-{}", label, std::process::id(), nanos))
    }

    /// Write `bytes` to `path`, creating parent dirs. Used to build fake rootfs trees on disk.
    pub(crate) fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// A >=20-byte fake ELF header whose `e_machine` (offset 18, LE u16) is `machine`, so `detect_arch`
    /// classifies a rootfs containing it as the corresponding Linux target without a real binary.
    pub(crate) fn fake_elf(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(b"\x7fELF");
        let m = machine.to_le_bytes();
        b[18] = m[0];
        b[19] = m[1];
        b
    }

    /// A hand-built ustar tar containing a regular file `regular` (with `contents`) and a CHAR-device
    /// entry `devpath` (major/minor 1/3). Building a device tar with the system `tar` needs a real device
    /// node (and privilege); crafting the 512-byte ustar headers directly lets the device-node tolerance
    /// test run unprivileged. On extract, an unprivileged `mknod` of the device fails benignly while the
    /// regular file still lands.
    pub(crate) fn tar_with_char_device(regular: &str, contents: &[u8], devpath: &str) -> Vec<u8> {
        fn header(name: &str, size: usize, typeflag: u8, devmaj: u32, devmin: u32) -> [u8; 512] {
            let mut h = [0u8; 512];
            let put = |h: &mut [u8; 512], off: usize, len: usize, s: &str| {
                let b = s.as_bytes();
                let n = b.len().min(len);
                h[off..off + n].copy_from_slice(&b[..n]);
            };
            let put_oct = |h: &mut [u8; 512], off: usize, len: usize, val: u64| {
                // `len-1` octal digits, space-padded to a trailing NUL (ustar convention).
                let s = format!("{:0width$o}", val, width = len - 1);
                let b = s.as_bytes();
                h[off..off + b.len()].copy_from_slice(b);
            };
            put(&mut h, 0, 100, name);
            put_oct(&mut h, 100, 8, 0o644); // mode
            put_oct(&mut h, 108, 8, 0); // uid
            put_oct(&mut h, 116, 8, 0); // gid
            put_oct(&mut h, 124, 12, size as u64); // size
            put_oct(&mut h, 136, 12, 0); // mtime
            h[156] = typeflag;
            put(&mut h, 257, 6, "ustar"); // magic (NUL-terminated)
            h[263] = b'0';
            h[264] = b'0'; // version "00"
            put_oct(&mut h, 329, 8, devmaj as u64);
            put_oct(&mut h, 337, 8, devmin as u64);
            // checksum: sum of all bytes with the checksum field treated as spaces.
            for b in h[148..156].iter_mut() {
                *b = b' ';
            }
            let sum: u32 = h.iter().map(|&b| b as u32).sum();
            put_oct(&mut h, 148, 8, sum as u64);
            h[155] = b' ';
            h
        }
        let mut out = Vec::new();
        // regular file entry + its (padded) data
        out.extend_from_slice(&header(regular, contents.len(), b'0', 0, 0));
        out.extend_from_slice(contents);
        let pad = (512 - contents.len() % 512) % 512;
        out.extend(std::iter::repeat(0u8).take(pad));
        // char-device entry (typeflag '3', no data)
        out.extend_from_slice(&header(devpath, 0, b'3', 1, 3));
        // two zero blocks = end of archive
        out.extend(std::iter::repeat(0u8).take(1024));
        out
    }

    /// `tar cf - -C <dir> <members...>` -> archive bytes (mirrors what `save_archive` shells out to).
    pub(crate) fn tar_members(dir: &Path, members: &[&str]) -> Vec<u8> {
        let mut c = std::process::Command::new("tar");
        c.arg("cf").arg("-").arg("-C").arg(dir);
        for m in members {
            c.arg(m);
        }
        let out = c.output().expect("spawn tar");
        assert!(
            out.status.success(),
            "tar failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::archive::testutil::unique_dir;

    #[test]
    fn remove_image_dir_deletes_dir_under_store_root() {
        let root = unique_dir("under");
        let img_dir = root.join("img");
        let rootfs = img_dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        let store = Store::new(root.to_str().unwrap());
        store.remove_image_dir(rootfs.to_str().unwrap()).unwrap();

        // The image dir (parent of rootfs) is strictly under the store root -> removed.
        assert!(!img_dir.exists(), "image dir under store root should be removed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_image_dir_leaves_dir_whose_parent_is_store_root() {
        let root = unique_dir("isroot");
        // rootfs sits directly under the store root, so its parent IS the store root.
        let rootfs = root.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        let store = Store::new(root.to_str().unwrap());
        store.remove_image_dir(rootfs.to_str().unwrap()).unwrap();

        // Guard: parent == store root -> left untouched (won't wipe the store itself).
        assert!(rootfs.exists(), "rootfs whose parent is the store root must be left untouched");

        let _ = std::fs::remove_dir_all(&root);
    }

    // Finding 5 — a manifest name with path traversal must resolve to a contained path strictly under the
    // store root (load's remove-before-rename would otherwise delete outside the store).
    #[test]
    fn dir_for_contains_path_traversal_names() {
        let store = Store::new("/var/lib/dd/images");
        let base = Path::new("/var/lib/dd/images");
        for name in ["../../evil", "..", ".", "a/../../b", "", "/etc/passwd"] {
            let d = store.dir_for(name);
            assert!(d.starts_with(base), "{name:?} -> {d:?} escaped the store");
            // Exactly one component below the store root (no `/` survived the encoding).
            let rel = d.strip_prefix(base).unwrap();
            assert_eq!(rel.components().count(), 1, "{name:?} -> {d:?} is not a single component");
            let comp = rel.components().next().unwrap().as_os_str();
            assert!(comp != ".." && comp != ".", "{name:?} -> {d:?} is a traversal component");
        }
    }

    // Finding 7 — the raw-name -> dir encoding is injective: two DISTINCT refs that both merely mixed
    // `/`, `:` and `_` (and collided under the old flatten-to-`_` scheme) now map to DIFFERENT dirs.
    #[test]
    fn dir_for_is_injective_for_colliding_refs() {
        let store = Store::new("/store");
        assert_ne!(store.dir_for("owner/app:1_2"), store.dir_for("owner/app_1:2"));
        assert_ne!(store.dir_for("a:b/c"), store.dir_for("a/b:c"));
        // ordinary names remain stable + readable (no separators to escape)
        assert_eq!(store.dir_for("busybox_latest"), PathBuf::from("/store/busybox_latest"));
    }

    // Finding 10 — install_dir never destroys the existing image before the new content is in place, and
    // restores the old image if the final rename fails.
    #[test]
    fn install_dir_success_swaps_and_failure_preserves_old() {
        let root = unique_dir("swap-store");
        let store = Store::new(root.to_str().unwrap());

        // A pre-existing image with an old sentinel.
        let target = root.join("repo_app");
        std::fs::create_dir_all(target.join("rootfs")).unwrap();
        std::fs::write(target.join("rootfs/old-sentinel"), b"old").unwrap();

        // Failure path: staged does not exist -> install fails, OLD image + sentinel survive intact.
        let missing = root.join(".staged-missing");
        let err = store.install_dir(&missing, &target).expect_err("missing staged must fail");
        assert!(err.to_string().contains("No such file") || !err.to_string().is_empty());
        assert!(target.join("rootfs/old-sentinel").exists(), "old image destroyed on failed install");
        // No `.old-*` aside left dangling.
        let asides: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".old-"))
            .collect();
        assert!(asides.is_empty(), "aside copy left behind after restore: {asides:?}");

        // Success path: a fully staged new image replaces it, old aside is cleaned up.
        let staged = root.join(".staged-new");
        std::fs::create_dir_all(staged.join("rootfs")).unwrap();
        std::fs::write(staged.join("rootfs/new-sentinel"), b"new").unwrap();
        store.install_dir(&staged, &target).expect("install succeeds");
        assert!(target.join("rootfs/new-sentinel").exists(), "new content not installed");
        assert!(!target.join("rootfs/old-sentinel").exists(), "old content should be replaced");
        let asides2: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".old-"))
            .collect();
        assert!(asides2.is_empty(), "aside copy not cleaned after success: {asides2:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_image_dir_leaves_dir_outside_store_root() {
        let root = unique_dir("store");
        std::fs::create_dir_all(&root).unwrap();
        // A rootfs living entirely outside the store root (e.g. a read-only bundled starter dir).
        let outside = unique_dir("outside");
        let img_dir = outside.join("img");
        let rootfs = img_dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        let store = Store::new(root.to_str().unwrap());
        store.remove_image_dir(rootfs.to_str().unwrap()).unwrap();

        // Guard: parent does not start with the store root -> left untouched.
        assert!(img_dir.exists(), "rootfs outside the store root must be left untouched");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
