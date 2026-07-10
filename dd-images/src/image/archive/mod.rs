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
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// The image labels (`LABEL` / `--label`), preserved across save/load.
    pub labels: std::collections::HashMap<String, String>,
    /// The `docker stop` signal (`Config.StopSignal`); empty ⇒ SIGTERM.
    pub stop_signal: String,
    /// The dirs that get an anonymous volume at run (`Config.Volumes` keys).
    pub img_volumes: Vec<String>,
    /// The container healthcheck probe as raw JSON (`None` ⇒ no probe recorded).
    pub healthcheck: Option<Value>,
}

impl Store {
    /// The store directory for an image `name`, flattening `/` and `:` to `_`. This is the RAW-name layout
    /// the load/import paths use (distinct from [`safe_name`]'s canonicalized-reference layout used by pull).
    fn dir_for(&self, name: &str) -> PathBuf {
        PathBuf::from(format!("{}/{}", self.dir, name.replace(['/', ':'], "_")))
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
            "healthcheck": img.healthcheck, "labels": img.labels,
            // Record os + instruction set so discovery restores the arch even for an ELF-less rootfs.
            "os": img.arch.os(), "arch": img.arch.isa(),
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
