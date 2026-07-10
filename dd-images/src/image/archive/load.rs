//! `docker load`: [`Store::load_archive`] — extract a dd save archive into a new image directory.

use super::*;
use crate::Error;
use std::path::PathBuf;

impl Store {
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
        // Prefer the arch the manifest recorded (round-trips an ELF-less linux/amd64 exactly); else sniff
        // the rootfs, else native arm64. Without this, a scratch/distroless amd64 image loaded back as arm64.
        let arch = if darwin {
            Arch::DarwinAarch64
        } else {
            manifest
                .arch
                .as_deref()
                .and_then(|a| Arch::detect("linux", a))
                .or_else(|| detect_arch(&rootfs))
                .unwrap_or(Arch::LinuxAarch64)
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
            labels: manifest.labels.clone(),
            stop_signal,
            img_volumes: manifest.img_volumes.clone(),
            healthcheck: manifest.healthcheck.clone(),
        };
        self.write_sidecar(&target, &loaded, darwin);
        Ok(loaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::archive::testutil::{tar_members, unique_dir, write_file};

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

    // Save/load must round-trip a linux/amd64 image's ARCH and LABELS even when the rootfs has NO
    // sniffable ELF (scratch/distroless) — previously it fell back to arm64 and dropped labels.
    #[test]
    fn load_restores_arch_and_labels_from_manifest_without_elf() {
        let src = unique_dir("arch-src");
        let rootfs = src.join("rootfs");
        write_file(&rootfs.join("etc/hostname"), b"scratch\n"); // no ELF anywhere
        let mut labels = std::collections::HashMap::new();
        labels.insert("org.example".to_string(), "yes".to_string());
        let manifest = Manifest {
            name: "scratch:amd64".to_string(),
            arch: Some("x86_64".to_string()),
            labels: labels.clone(),
            ..Default::default()
        };
        let bytes = Store::new("/unused").save_archive(&rootfs, &manifest).unwrap();
        let store_dir = unique_dir("arch-store");
        let loaded = Store::new(store_dir.to_str().unwrap()).load_archive(&bytes).unwrap();
        assert_eq!(loaded.arch, Arch::LinuxX86_64, "manifest arch restores amd64 (not the arm64 fallback)");
        assert_eq!(loaded.labels.get("org.example").map(String::as_str), Some("yes"), "labels round-trip");
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
