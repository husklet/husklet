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
    use crate::image::archive::testutil::{fake_elf, tar_members, unique_dir, write_file};

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
}
