//! `docker save`: [`Store::save_archive`] — tar the image's `rootfs/` plus a `hl-manifest.json` sidecar.

use super::*;
use crate::Error;
use std::path::Path;

impl Store {
    /// `docker save`: tar the image's `rootfs/` directory plus a `hl-manifest.json` sidecar and return the
    /// archive bytes. `rootfs` is the image's on-disk `.../rootfs` path; the manifest records its identity +
    /// run config so a later [`load_archive`](Self::load_archive) restores name/cmd/env exactly. The
    /// on-disk image directory is left untouched (the manifest is staged in a temp dir and tarred via a
    /// second `-C`).
    pub fn save_archive(&self, rootfs: &Path, manifest: &Manifest) -> Result<Vec<u8>, Error> {
        let parent = rootfs
            .parent()
            .ok_or_else(|| Error::Archive("image has no rootfs directory".to_string()))?;
        let staging = std::env::temp_dir().join(format!("hl-save-{}", uniq()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| Error::Archive(e.to_string()))?;
        let manifest_json = serde_json::to_string(manifest).map_err(|e| Error::Archive(e.to_string()))?;
        let _ = std::fs::write(staging.join("hl-manifest.json"), manifest_json);
        // SAVE_FLAGS: `--format=posix` (nanosecond mtimes), `--xattrs` (extended attributes round-trip),
        // `--sparse` (holes don't expand). `-c` creates, `-f -` writes the archive to stdout.
        let out = std::process::Command::new("tar")
            .args(SAVE_FLAGS)
            .arg("-cf")
            .arg("-")
            .arg("-C")
            .arg(parent)
            .arg("rootfs")
            .arg("-C")
            .arg(&staging)
            .arg("hl-manifest.json")
            .output();
        let _ = std::fs::remove_dir_all(&staging);
        match out {
            Ok(o) if o.status.success() => Ok(o.stdout),
            Ok(o) => Err(Error::Archive(String::from_utf8_lossy(&o.stderr).into_owned())),
            Err(e) => Err(Error::Archive(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::archive::testutil::{fake_elf, unique_dir, write_file};

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

    // Findings 3/4/13 — the save `tar` requests xattr, sparse and pax(posix) preservation. A true
    // ns-mtime / hole / xattr test is environment-sensitive (needs specific fs + privilege), so we assert
    // the flags are present in the exact constant the save command shells out with.
    #[test]
    fn save_flags_request_xattrs_sparse_and_posix() {
        assert!(SAVE_FLAGS.contains(&"--xattrs"), "xattrs round-trip (finding 3)");
        assert!(SAVE_FLAGS.contains(&"--sparse"), "sparse files not expanded (finding 4)");
        assert!(SAVE_FLAGS.contains(&"--format=posix"), "pax/ns mtimes (finding 13)");
    }

    // ---- archive round-trip integration tests (shell out to the system `tar`) ----

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
            arch: None,
            labels: std::collections::HashMap::new(),
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
}
