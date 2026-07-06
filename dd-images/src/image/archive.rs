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

    // ---- archive round-trip integration tests (shell out to the system `tar`) ----

    /// Write `bytes` to `path`, creating parent dirs. Used to build fake rootfs trees on disk.
    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// A >=20-byte fake ELF header whose `e_machine` (offset 18, LE u16) is `machine`, so `detect_arch`
    /// classifies a rootfs containing it as the corresponding Linux target without a real binary.
    fn fake_elf(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(b"\x7fELF");
        let m = machine.to_le_bytes();
        b[18] = m[0];
        b[19] = m[1];
        b
    }

    /// `tar cf - -C <dir> <members...>` -> archive bytes (mirrors what `save_archive` shells out to).
    fn tar_members(dir: &Path, members: &[&str]) -> Vec<u8> {
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

    // Flow 1 — save -> load round-trips identity, run config, arch, AND file contents byte-faithfully.
    // Invariant: `save_archive(rootfs, manifest)` then `load_archive(bytes)` yields a LoadedImage whose
    // every config field equals what went in, whose arch is re-detected from the packed binary, and whose
    // unpacked rootfs contains the original files with intact contents.
    #[test]
    fn save_then_load_roundtrips_config_and_files() {
        let src = unique_dir("rt-src");
        let rootfs = src.join("rootfs");
        write_file(&rootfs.join("hello.txt"), b"hello dd\n");
        write_file(&rootfs.join("etc/motd"), b"welcome\n");
        // A fake x86_64 ELF at a probe path so the re-detected arch is the meaningful LinuxX86_64
        // (NOT the LinuxAarch64 fallback) — proves arch is genuinely probed on load.
        write_file(&rootfs.join("bin/sh"), &fake_elf(0x3E));

        let manifest = Manifest {
            name: "myrepo/app:v1".to_string(),
            cmd: vec!["/bin/sh".to_string(), "-c".to_string(), "echo hi".to_string()],
            env: vec!["PATH=/usr/bin".to_string(), "FOO=bar".to_string()],
            entrypoint: vec!["/entry".to_string()],
            workdir: "/work".to_string(),
            user: "1000".to_string(),
            exposed_ports: vec!["8080/tcp".to_string()],
            os: None,
            stop_signal: Some("SIGINT".to_string()),
            img_volumes: vec!["/data".to_string()],
            healthcheck: Some(serde_json::json!({"Test": ["CMD", "true"]})),
        };

        // save_archive doesn't touch self.dir; any Store produces the bytes.
        let bytes = Store::new("/unused")
            .save_archive(&rootfs, &manifest)
            .expect("save_archive");
        assert!(!bytes.is_empty(), "archive bytes are non-empty");

        let store_dir = unique_dir("rt-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let loaded = store.load_archive(&bytes).expect("load_archive");

        // Identity + run config round-trip exactly (name keeps its raw `/` and `:`).
        assert_eq!(loaded.name, "myrepo/app:v1");
        assert_eq!(loaded.cmd, vec!["/bin/sh", "-c", "echo hi"]);
        assert_eq!(loaded.env, vec!["PATH=/usr/bin", "FOO=bar"]);
        assert_eq!(loaded.entrypoint, vec!["/entry"]);
        assert_eq!(loaded.workdir, "/work");
        assert_eq!(loaded.user, "1000");
        assert_eq!(loaded.exposed_ports, vec!["8080/tcp"]);
        assert_eq!(loaded.stop_signal, "SIGINT");
        assert_eq!(loaded.img_volumes, vec!["/data"]);
        assert_eq!(loaded.healthcheck, Some(serde_json::json!({"Test": ["CMD", "true"]})));
        // Arch re-detected from the packed ELF.
        assert_eq!(loaded.arch, Arch::LinuxX86_64);

        // Files land under the unpacked rootfs WITH their contents intact.
        assert_eq!(
            std::fs::read_to_string(loaded.rootfs.join("hello.txt")).unwrap(),
            "hello dd\n"
        );
        assert_eq!(
            std::fs::read_to_string(loaded.rootfs.join("etc/motd")).unwrap(),
            "welcome\n"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Flow 2 — a rootfs-only archive (no dd-manifest.json) loads via the fallback path.
    // Invariant: with no manifest, name falls back to "loaded", arch falls back to LinuxAarch64 (no
    // binaries to sniff), and the rootfs still unpacks.
    #[test]
    fn load_rootfs_only_archive_falls_back() {
        let src = unique_dir("ro-src");
        let rootfs = src.join("rootfs");
        write_file(&rootfs.join("greeting"), b"no manifest here\n");
        // Archive containing ONLY `rootfs/...` (no dd-manifest.json sidecar).
        let bytes = tar_members(&src, &["rootfs"]);

        let store_dir = unique_dir("ro-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let loaded = store.load_archive(&bytes).expect("load_archive tolerates no manifest");

        assert_eq!(loaded.name, "loaded", "generic fallback name");
        assert_eq!(loaded.arch, Arch::LinuxAarch64, "arch fallback with no sniffable binary");
        assert_eq!(
            std::fs::read_to_string(loaded.rootfs.join("greeting")).unwrap(),
            "no manifest here\n"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Flow 3 — a NON-dd archive (no top-level rootfs/) is rejected and leaves nothing behind.
    // Invariant: load_archive returns Err and its staging dir is cleaned up, so the store root holds no
    // partial image or `.load-*` directory.
    #[test]
    fn load_non_dd_archive_errors_and_leaves_no_leftover() {
        let src = unique_dir("nd-src");
        // Files at the top level, NO `rootfs/` wrapper.
        write_file(&src.join("loose.txt"), b"not a dd image\n");
        write_file(&src.join("data/inner"), b"x\n");
        let bytes = tar_members(&src, &["loose.txt", "data"]);

        let store_dir = unique_dir("nd-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let err = store.load_archive(&bytes).expect_err("non-dd archive must be rejected");
        assert!(
            err.to_string().contains("not a dd image"),
            "err mentions the not-a-dd-image reason: {err}"
        );

        // Staging was cleaned: the store root has no leftover entries at all.
        let leftovers: Vec<_> = std::fs::read_dir(&store_dir)
            .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "no partial image / staging dir left behind, found: {leftovers:?}"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Flow 4 — import_rootfs unpacks a bare rootfs tar (files at top level, no wrapper/manifest).
    // Invariant: the given name is kept verbatim, the arch is probed from the rootfs, and every file lands
    // directly under the new image's rootfs with intact contents.
    #[test]
    fn import_rootfs_unpacks_bare_tar() {
        let src = unique_dir("imp-src");
        write_file(&src.join("app.conf"), b"key=value\n");
        write_file(&src.join("usr/local/note"), b"imported\n");
        // Fake x86_64 ELF at a probe path so the probed arch is the distinguishable LinuxX86_64.
        write_file(&src.join("bin/busybox"), &fake_elf(0x3E));
        // Bare rootfs: members are the top-level entries themselves (no `rootfs/` dir).
        let bytes = tar_members(&src, &["app.conf", "usr", "bin"]);

        let store_dir = unique_dir("imp-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let loaded = store.import_rootfs("myimg", &bytes).expect("import_rootfs");

        assert_eq!(loaded.name, "myimg");
        assert_eq!(loaded.arch, Arch::LinuxX86_64, "arch probed from the imported rootfs");
        assert_eq!(
            std::fs::read_to_string(loaded.rootfs.join("app.conf")).unwrap(),
            "key=value\n"
        );
        assert_eq!(
            std::fs::read_to_string(loaded.rootfs.join("usr/local/note")).unwrap(),
            "imported\n"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Flow 6 — concurrent loads into ONE store don't collide (the uniq() per-process SEQ fix).
    // Invariant: two threads loading distinct images into the same store root both succeed and produce
    // independent, correct rootfs trees (exercises the per-request `.load-<pid>-<seq>` staging uniqueness;
    // two shared-`<pid>` staging paths would otherwise clobber each other mid-extract).
    #[test]
    fn concurrent_loads_into_one_store_do_not_collide() {
        // Build two independent save archives with distinct names + marker files.
        let mk = |label: &str, marker: &[u8]| -> Vec<u8> {
            let src = unique_dir(label);
            let rootfs = src.join("rootfs");
            write_file(&rootfs.join("marker"), marker);
            let manifest = Manifest {
                name: format!("conc/{label}:1"),
                ..Default::default()
            };
            let bytes = Store::new("/unused").save_archive(&rootfs, &manifest).unwrap();
            let _ = std::fs::remove_dir_all(&src);
            bytes
        };
        let bytes_a = mk("conc-a", b"AAAA\n");
        let bytes_b = mk("conc-b", b"BBBB\n");

        let store_dir = unique_dir("conc-store");
        let store = Store::new(store_dir.to_str().unwrap());

        let sa = store.clone();
        let sb = store.clone();
        let ta = std::thread::spawn(move || sa.load_archive(&bytes_a));
        let tb = std::thread::spawn(move || sb.load_archive(&bytes_b));
        let la = ta.join().unwrap().expect("thread a load");
        let lb = tb.join().unwrap().expect("thread b load");

        // Both loaded independently, each with its own name + marker contents.
        assert_eq!(la.name, "conc/conc-a:1");
        assert_eq!(lb.name, "conc/conc-b:1");
        assert_eq!(std::fs::read_to_string(la.rootfs.join("marker")).unwrap(), "AAAA\n");
        assert_eq!(std::fs::read_to_string(lb.rootfs.join("marker")).unwrap(), "BBBB\n");

        let _ = std::fs::remove_dir_all(&store_dir);
    }
}
