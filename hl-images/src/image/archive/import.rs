//! `docker import`: [`Store::import_rootfs`] — extract a bare rootfs tar into a new named image.

use super::*;
use crate::Error;
use serde_json::json;

impl Store {
    /// `docker import`: extract a bare rootfs tar (no manifest) into a new image named `name` (already a
    /// `repository` or `repository:tag`) and return the materialized [`LoadedImage`]. The arch is probed
    /// from the rootfs and the command defaults to the image's shell; a minimal `dd-image.json` sidecar is
    /// written so the image survives a daemon restart.
    pub fn import_rootfs(&self, name: &str, tar_bytes: &[u8]) -> Result<LoadedImage, Error> {
        let target = self.dir_for(name);
        let rootfs = target.join("rootfs");
        // Remove any pre-existing target and extract into a FRESH rootfs so tar can't follow a stale
        // symlink out of the store; on ANY failure we remove the target again (finding 1) so a failed
        // import never leaves a half-populated image behind.
        let _ = std::fs::remove_dir_all(&target);
        std::fs::create_dir_all(&rootfs).map_err(|e| Error::Archive(e.to_string()))?;
        let tmp = std::env::temp_dir().join(format!("dd-import-{}.tar", uniq()));
        if let Err(e) = std::fs::write(&tmp, tar_bytes) {
            let _ = std::fs::remove_dir_all(&target);
            return Err(Error::Archive(e.to_string()));
        }
        // run_extract requests owner/perm/xattr preservation (findings 3/11) and tolerates unprivileged
        // device-node mknod failures (finding 12) while still failing a genuinely broken archive.
        let extract = run_extract(&tmp, &rootfs);
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = extract {
            let _ = std::fs::remove_dir_all(&target);
            return Err(e);
        }
        let arch = detect_arch(&rootfs).unwrap_or(Arch::LinuxAarch64);
        let cmd = default_shell(&rootfs);
        let _ = std::fs::write(
            target.join("hl-image.json"),
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
            labels: std::collections::HashMap::new(),
            stop_signal: String::new(),
            img_volumes: Vec::new(),
            healthcheck: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::archive::testutil::{
        fake_elf, tar_members, tar_with_char_device, unique_dir, write_file,
    };
    use crate::image::archive::EXTRACT_FLAGS;

    // Finding 1 — a failed import (broken tar) leaves NO partial target dir behind.
    #[test]
    fn import_failure_removes_partial_target() {
        let store_dir = unique_dir("imp-fail-store");
        let store = Store::new(store_dir.to_str().unwrap());
        // Not a tar at all -> tar fails fatally -> import must Err AND clean up the target.
        let err = store
            .import_rootfs("brokenimg", b"this is definitely not a tar archive\n")
            .expect_err("broken tar must fail import");
        assert!(!err.to_string().is_empty());
        // The target directory the import would have created must not survive.
        assert!(
            !store_dir.join("brokenimg").exists(),
            "failed import left a partial target dir behind"
        );
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Finding 12 — a tar carrying a char-device entry plus a regular file imports successfully with the
    // regular file present (the unprivileged mknod failure must not abort the whole extract).
    #[test]
    fn import_tolerates_device_nodes() {
        let bytes = tar_with_char_device("regular.txt", b"i am regular\n", "dev/fakenull");
        let store_dir = unique_dir("imp-dev-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let loaded = store
            .import_rootfs("devimg", &bytes)
            .expect("device-node tar must still import the regular file");
        assert_eq!(
            std::fs::read_to_string(loaded.rootfs.join("regular.txt")).unwrap(),
            "i am regular\n"
        );
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Finding 11 — extraction requests numeric-owner/permission preservation (real chown needs root; we
    // assert the flags the extract command uses).
    #[test]
    fn extract_flags_preserve_owner_and_perms() {
        assert!(
            EXTRACT_FLAGS.contains(&"--numeric-owner"),
            "numeric owner (finding 11)"
        );
        assert!(
            EXTRACT_FLAGS.contains(&"--same-owner"),
            "same owner (finding 11)"
        );
        assert!(
            EXTRACT_FLAGS.contains(&"-p"),
            "preserve permissions (finding 11)"
        );
        assert!(
            EXTRACT_FLAGS.contains(&"--xattrs"),
            "xattrs round-trip (finding 3)"
        );
    }

    // C08 — the dd-images extraction boundary (run_extract_args) must REJECT an archive whose member
    // escapes the destination via a `..` component (path traversal) BEFORE writing any file, so a
    // `docker import` of a hostile tar can't land files outside the store.
    #[test]
    fn import_rejects_path_traversal_member() {
        let src = unique_dir("trav-src");
        std::fs::create_dir_all(&src).unwrap();
        write_file(&src.join("x"), b"pwn\n");
        // GNU tar --transform prepends `../` -> member "../x". Skip gracefully on bsdtar (no --transform).
        let evil = src.join("evil.tar");
        let made = std::process::Command::new("tar")
            .arg("cf")
            .arg(&evil)
            .arg("-C")
            .arg(&src)
            .arg("--transform")
            .arg("s,^,../,")
            .arg("x")
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            let _ = std::fs::remove_dir_all(&src);
            return; // no GNU tar --transform available; the guard wiring is still exercised elsewhere
        }
        // Prove the archive really carries a `..` member (else the test asserts nothing).
        let listed = std::process::Command::new("tar")
            .arg("tf")
            .arg(&evil)
            .output()
            .unwrap();
        if !String::from_utf8_lossy(&listed.stdout).contains("..") {
            let _ = std::fs::remove_dir_all(&src);
            return;
        }
        let bytes = std::fs::read(&evil).unwrap();

        let store_dir = unique_dir("trav-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let err = store
            .import_rootfs("evilimg", &bytes)
            .expect_err("a `..`-escaping member must be rejected at the extraction boundary");
        assert!(err.to_string().contains("path traversal"), "err: {err}");
        // The rejected import leaves no image dir (and never wrote the escaping file).
        assert!(
            !store_dir.join("evilimg").exists(),
            "rejected import must leave no image dir"
        );
        assert!(!src.join("x.escaped").exists());

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
        assert_eq!(
            loaded.arch,
            Arch::LinuxX86_64,
            "arch probed from the imported rootfs"
        );
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
}
