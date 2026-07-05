//! The archive side of the store: `docker save` / `docker load` / `docker import`.
//!
//! dd's archive format is intentionally simple (not full OCI): a tar whose top level is the image's
//! `rootfs/` directory plus a [`Manifest`] sidecar (`dd-manifest.json`). [`Store::save_archive`] produces
//! it, [`Store::load_archive`] consumes it; [`Store::import_rootfs`] instead takes a bare rootfs tar (no
//! manifest) whose files land directly in a new image's rootfs. All tar work shells out to the system
//! `tar` (no crate dependency, no runtime) so the on-disk layout matches Docker/dd exactly.

use super::*;
use crate::Error;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// The store directory for an image `name`, flattening `/` and `:` to `_`. This is the RAW-name layout
    /// the load/import paths use (distinct from [`safe_name`]'s canonicalized-reference layout used by pull).
    fn dir_for(&self, name: &str) -> PathBuf {
        PathBuf::from(format!("{}/{}", self.dir, name.replace(['/', ':'], "_")))
    }

    /// `docker save`: tar the image's `rootfs/` directory plus a `dd-manifest.json` sidecar and return the
    /// archive bytes. `rootfs` is the image's on-disk `.../rootfs` path; the manifest records its identity +
    /// run config so a later [`load_archive`](Self::load_archive) restores name/cmd/env exactly. The
    /// on-disk image directory is left untouched (the manifest is staged in a temp dir and tarred via a
    /// second `-C`).
    pub fn save_archive(&self, rootfs: &Path, manifest: &Manifest) -> Result<Vec<u8>, Error> {
        let parent = rootfs
            .parent()
            .ok_or_else(|| Error::Archive("image has no rootfs directory".to_string()))?;
        let staging = std::env::temp_dir().join(format!("dd-save-{}", uniq()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| Error::Archive(e.to_string()))?;
        let manifest_json = serde_json::to_string(manifest).map_err(|e| Error::Archive(e.to_string()))?;
        let _ = std::fs::write(staging.join("dd-manifest.json"), manifest_json);
        let out = std::process::Command::new("tar")
            .arg("cf")
            .arg("-")
            .arg("-C")
            .arg(parent)
            .arg("rootfs")
            .arg("-C")
            .arg(&staging)
            .arg("dd-manifest.json")
            .output();
        let _ = std::fs::remove_dir_all(&staging);
        match out {
            Ok(o) if o.status.success() => Ok(o.stdout),
            Ok(o) => Err(Error::Archive(String::from_utf8_lossy(&o.stderr).into_owned())),
            Err(e) => Err(Error::Archive(e.to_string())),
        }
    }

    /// `docker load`: extract a dd save archive (`rootfs/` + optional `dd-manifest.json`) into a new image
    /// directory under the store and return the materialized [`LoadedImage`]. Tolerates a rootfs-only
    /// archive (no manifest) by falling back to a generic name + probed arch. Writes a `dd-image.json`
    /// sidecar so the image round-trips through discovery after a daemon restart.
    pub fn load_archive(&self, tar_bytes: &[u8]) -> Result<LoadedImage, Error> {
        let tmp = std::env::temp_dir().join(format!("dd-load-{}.tar", uniq()));
        std::fs::write(&tmp, tar_bytes).map_err(|e| Error::Archive(e.to_string()))?;
        let staging = PathBuf::from(format!("{}/.load-{}", self.dir, uniq()));
        let _ = std::fs::remove_dir_all(&staging);
        if let Err(e) = std::fs::create_dir_all(&staging) {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Archive(e.to_string()));
        }
        let out = std::process::Command::new("tar")
            .arg("xf")
            .arg(&tmp)
            .arg("-C")
            .arg(&staging)
            .output();
        let _ = std::fs::remove_file(&tmp);
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(Error::Archive(String::from_utf8_lossy(&o.stderr).into_owned()));
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(Error::Archive(e.to_string()));
            }
        }
        if !staging.join("rootfs").is_dir() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::Archive(
                "archive is not a dd image (no rootfs/ at top level)".to_string(),
            ));
        }
        // dd-manifest.json (written by `save`) carries the image identity; tolerate a rootfs-only archive
        // by falling back to a generic name via the default manifest.
        let manifest: Manifest = std::fs::read_to_string(staging.join("dd-manifest.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Manifest>(&s).ok())
            .unwrap_or_default();
        let name = if manifest.name.is_empty() {
            "loaded".to_string()
        } else {
            manifest.name.clone()
        };
        let darwin = manifest.is_darwin();
        let target = self.dir_for(&name);
        let _ = std::fs::remove_dir_all(&target);
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::Archive(e.to_string()));
        }
        let rootfs = target.join("rootfs");
        let arch = if darwin {
            Arch::DarwinAarch64
        } else {
            detect_arch(&rootfs).unwrap_or(Arch::LinuxAarch64)
        };
        let mut cmd = manifest.cmd.clone();
        if cmd.is_empty() {
            cmd = if darwin { vec!["bash".into()] } else { default_shell(&rootfs) };
        }
        let stop_signal = manifest.stop_signal.clone().unwrap_or_default();
        let loaded = LoadedImage {
            name,
            rootfs,
            arch,
            cmd,
            env: manifest.env.clone(),
            entrypoint: manifest.entrypoint.clone(),
            workdir: manifest.workdir.clone(),
            user: manifest.user.clone(),
            exposed_ports: manifest.exposed_ports.clone(),
            stop_signal,
            img_volumes: manifest.img_volumes.clone(),
            healthcheck: manifest.healthcheck.clone(),
        };
        self.write_sidecar(&target, &loaded, darwin);
        Ok(loaded)
    }

    /// `docker import`: extract a bare rootfs tar (no manifest) into a new image named `name` (already a
    /// `repository` or `repository:tag`) and return the materialized [`LoadedImage`]. The arch is probed
    /// from the rootfs and the command defaults to the image's shell; a minimal `dd-image.json` sidecar is
    /// written so the image survives a daemon restart.
    pub fn import_rootfs(&self, name: &str, tar_bytes: &[u8]) -> Result<LoadedImage, Error> {
        let target = self.dir_for(name);
        let rootfs = target.join("rootfs");
        let _ = std::fs::remove_dir_all(&target);
        std::fs::create_dir_all(&rootfs).map_err(|e| Error::Archive(e.to_string()))?;
        let tmp = std::env::temp_dir().join(format!("dd-import-{}.tar", uniq()));
        std::fs::write(&tmp, tar_bytes).map_err(|e| Error::Archive(e.to_string()))?;
        let out = std::process::Command::new("tar")
            .arg("xf")
            .arg(&tmp)
            .arg("-C")
            .arg(&rootfs)
            .output();
        let _ = std::fs::remove_file(&tmp);
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => return Err(Error::Archive(String::from_utf8_lossy(&o.stderr).into_owned())),
            Err(e) => return Err(Error::Archive(e.to_string())),
        }
        let arch = detect_arch(&rootfs).unwrap_or(Arch::LinuxAarch64);
        let cmd = default_shell(&rootfs);
        let _ = std::fs::write(
            target.join("dd-image.json"),
            json!({ "name": name, "cmd": cmd }).to_string(),
        );
        Ok(LoadedImage {
            name: name.to_string(),
            rootfs,
            arch,
            cmd,
            env: Vec::new(),
            entrypoint: Vec::new(),
            workdir: String::new(),
            user: String::new(),
            exposed_ports: Vec::new(),
            stop_signal: String::new(),
            img_volumes: Vec::new(),
            healthcheck: None,
        })
    }

    /// Remove an image's on-disk directory (`<store>/<safe>/`, the parent of its `rootfs/`). Guarded to the
    /// writable store: a rootfs under a read-only bundled starter dir (or anywhere outside the store root)
    /// is left untouched so removing a discovered alias can't wipe shipped images.
    pub fn remove_image_dir(&self, rootfs: &str) {
        let Some(dir) = Path::new(rootfs).parent() else {
            return;
        };
        let base = Path::new(&self.dir);
        if dir != base && dir.starts_with(base) {
            let _ = std::fs::remove_dir_all(dir);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    // A unique scratch dir under the system temp dir.
    fn unique_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("dd-images-test-{}-{}-{}", label, std::process::id(), nanos))
    }

    #[test]
    fn remove_image_dir_deletes_dir_under_store_root() {
        let root = unique_dir("under");
        let img_dir = root.join("img");
        let rootfs = img_dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();

        let store = Store::new(root.to_str().unwrap());
        store.remove_image_dir(rootfs.to_str().unwrap());

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
        store.remove_image_dir(rootfs.to_str().unwrap());

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
        store.remove_image_dir(rootfs.to_str().unwrap());

        // Guard: parent does not start with the store root -> left untouched.
        assert!(img_dir.exists(), "rootfs outside the store root must be left untouched");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    // A real error site round-trips its EXACT former String through the typed Error: `save_archive` on a
    // rootfs with no parent directory hits the `ok_or` guard, and its Display must equal the old text
    // byte-for-byte (this is the string the daemon surfaces as the HTTP `{"message": …}` body).
    #[test]
    fn save_archive_no_parent_preserves_exact_message() {
        let store = Store::new("/unused/for/this/test");
        let err = store
            .save_archive(Path::new("/"), &Manifest::default())
            .unwrap_err();
        assert_eq!(err.to_string(), "image has no rootfs directory");
    }
}
