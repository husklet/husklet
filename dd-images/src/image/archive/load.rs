//! `docker load`: [`Store::load_archive`] — extract a dd save archive (or a standard `docker save`
//! archive) into a new image directory under the store.

use super::*;
use crate::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

impl Store {
    /// `docker load`: materialize an archive into a new image directory under the store and return the
    /// [`LoadedImage`]. Two archive shapes are accepted:
    ///
    ///  * **dd format** — a top-level `rootfs/` dir plus an optional `dd-manifest.json` sidecar (what
    ///    [`save_archive`](Self::save_archive) writes). A rootfs-only archive (no manifest) still loads via
    ///    a generic name + probed arch; a PRESENT-but-malformed `dd-manifest.json` is an error.
    ///  * **docker save format** — a top-level `manifest.json` (a JSON array of `{Config, RepoTags,
    ///    Layers}`), a config blob, and per-layer tar members (`<hash>/layer.tar`, `<hash>.tar`, or
    ///    `blobs/sha256/<hash>`, gzipped or not). The layers are flattened IN ORDER into one rootfs honoring
    ///    OCI/AUFS whiteouts (`.wh.<name>` deletes `<name>`; `.wh..wh..opq` makes a dir opaque), and the
    ///    image identity/run config come from `RepoTags[0]` + the config blob's `config`/`Config` section.
    ///
    /// Extraction preserves owners/perms/xattrs and tolerates unprivileged device-node mknod failures. The
    /// staged image is swapped into place so a same-name load never destroys the previous rootfs until the
    /// new one is fully built. Writes a `dd-image.json` sidecar so the image survives a daemon restart.
    pub fn load_archive(&self, tar_bytes: &[u8]) -> Result<LoadedImage, Error> {
        let tmp = std::env::temp_dir().join(format!("dd-load-{}.tar", uniq()));
        std::fs::write(&tmp, tar_bytes).map_err(|e| Error::Archive(e.to_string()))?;
        let staging = PathBuf::from(format!("{}/.load-{}", self.dir, uniq()));
        let _ = std::fs::remove_dir_all(&staging);
        if let Err(e) = std::fs::create_dir_all(&staging) {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Archive(e.to_string()));
        }
        // Extract the OUTER archive tolerantly (xattrs/owners preserved; device nodes don't abort it).
        let extract = run_extract(&tmp, &staging);
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = extract {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }

        let result = if staging.join("rootfs").is_dir() {
            self.load_dd_format(&staging)
        } else if staging.join("manifest.json").is_file() {
            self.load_docker_format(&staging)
        } else {
            Err(Error::Archive(
                "archive is not a dd image (no rootfs/ at top level)".to_string(),
            ))
        };
        // The dd path renames `staging` into place; the docker path builds a separate dir and leaves
        // `staging`. Either way, remove whatever remains (a no-op if already renamed away).
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    /// Materialize a dd-format staging dir (`rootfs/` + optional `dd-manifest.json`) into a new image.
    fn load_dd_format(&self, staging: &Path) -> Result<LoadedImage, Error> {
        // dd-manifest.json carries the image identity. ABSENT is fine (rootfs-only fallback); PRESENT but
        // malformed is an ERROR rather than being silently swallowed to defaults.
        let manifest_path = staging.join("dd-manifest.json");
        let manifest: Manifest = if manifest_path.exists() {
            let s = std::fs::read_to_string(&manifest_path).map_err(|e| Error::Archive(e.to_string()))?;
            serde_json::from_str::<Manifest>(&s)
                .map_err(|e| Error::Manifest(format!("malformed dd-manifest.json: {e}")))?
        } else {
            Manifest::default()
        };
        if !manifest.os_is_supported() {
            return Err(Error::Manifest(format!(
                "unsupported image os: {}",
                manifest.os.as_deref().unwrap_or_default()
            )));
        }
        let name = if manifest.name.is_empty() {
            "loaded".to_string()
        } else {
            manifest.name.clone()
        };
        let darwin = manifest.is_darwin();
        let target = self.dir_for(&name);
        self.install_dir(staging, &target)?;
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
            stop_signal: manifest.stop_signal.clone().unwrap_or_default(),
            img_volumes: manifest.img_volumes.clone(),
            healthcheck: manifest.healthcheck.clone(),
        };
        self.write_sidecar(&target, &loaded, darwin);
        Ok(loaded)
    }

    /// Materialize a `docker save` staging dir into a new image: read `manifest.json`, flatten its layers
    /// in order into one rootfs (honoring whiteouts), and recover identity + run config from the config blob.
    fn load_docker_format(&self, staging: &Path) -> Result<LoadedImage, Error> {
        let mtext = std::fs::read_to_string(staging.join("manifest.json"))
            .map_err(|e| Error::Archive(e.to_string()))?;
        let mval: Value = serde_json::from_str(&mtext)
            .map_err(|e| Error::Manifest(format!("malformed docker manifest.json: {e}")))?;
        let entry = mval
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| Error::Manifest("docker manifest.json is not a non-empty array".to_string()))?;
        let layers: Vec<String> = entry["Layers"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if layers.is_empty() {
            return Err(Error::Manifest("docker manifest has no layers".to_string()));
        }
        let name = entry["RepoTags"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("loaded")
            .to_string();
        // Config blob (image config). Missing/unreadable -> Null; run config then falls back to defaults.
        let blob: Value = entry["Config"]
            .as_str()
            .and_then(|p| std::fs::read_to_string(staging.join(p)).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        // Reject a present-but-unsupported OS rather than importing it as Linux.
        if let Some(os) = blob["os"].as_str() {
            if !os.is_empty() && os != "linux" && os != "darwin" {
                return Err(Error::Manifest(format!("unsupported image os: {os}")));
            }
        }
        let darwin = blob["os"].as_str() == Some("darwin");

        // Flatten the layers IN ORDER into a fresh build dir's rootfs, honoring whiteouts.
        let build = PathBuf::from(format!("{}/.merge-{}", self.dir, uniq()));
        let _ = std::fs::remove_dir_all(&build);
        let merged = build.join("rootfs");
        std::fs::create_dir_all(&merged).map_err(|e| Error::Archive(e.to_string()))?;
        for layer in &layers {
            if let Err(e) = apply_layer(&staging.join(layer), &merged) {
                let _ = std::fs::remove_dir_all(&build);
                return Err(e);
            }
        }

        let target = self.dir_for(&name);
        if let Err(e) = self.install_dir(&build, &target) {
            let _ = std::fs::remove_dir_all(&build);
            return Err(e);
        }
        let rootfs = target.join("rootfs");
        let arch = arch_from_config(&blob)
            .or_else(|| detect_arch(&rootfs))
            .unwrap_or(if darwin { Arch::DarwinAarch64 } else { Arch::LinuxAarch64 });

        // The run config lives under `config` (OCI image config) or `Config` (docker container config).
        let section = if blob.get("config").map(Value::is_object).unwrap_or(false) {
            &blob["config"]
        } else {
            &blob["Config"]
        };
        let strs = |key: &str| -> Vec<String> {
            section[key]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let sorted_keys = |key: &str| -> Vec<String> {
            let mut v: Vec<String> = section[key]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            v.sort();
            v
        };
        let mut cmd = strs("Cmd");
        if cmd.is_empty() {
            cmd = if darwin { vec!["bash".into()] } else { default_shell(&rootfs) };
        }
        let healthcheck = match &section["Healthcheck"] {
            Value::Null => None,
            hc => Some(hc.clone()),
        };
        let loaded = LoadedImage {
            name,
            rootfs,
            arch,
            cmd,
            env: strs("Env"),
            entrypoint: strs("Entrypoint"),
            workdir: section["WorkingDir"].as_str().unwrap_or_default().to_string(),
            user: section["User"].as_str().unwrap_or_default().to_string(),
            exposed_ports: sorted_keys("ExposedPorts"),
            stop_signal: section["StopSignal"].as_str().unwrap_or_default().to_string(),
            img_volumes: sorted_keys("Volumes"),
            healthcheck,
        };
        self.write_sidecar(&target, &loaded, darwin);
        Ok(loaded)
    }
}

/// Apply one docker-save layer tar onto the merged `rootfs`: honor OCI/AUFS whiteouts, then extract the
/// layer's real content over the merged tree (dropping the `.wh.*` markers themselves). Whiteouts:
/// `<dir>/.wh.<name>` deletes `<dir>/<name>`; `<dir>/.wh..wh..opq` makes `<dir>` opaque (its lower
/// contents are cleared before this layer's content is applied).
fn apply_layer(layer: &Path, merged: &Path) -> Result<(), Error> {
    // List the layer's members to discover whiteout markers (GNU tar auto-detects gzip on read).
    let listing = Command::new("tar")
        .arg("tf")
        .arg(layer)
        .output()
        .map_err(|e| Error::Archive(e.to_string()))?;
    if !listing.status.success() {
        return Err(Error::Archive(format!(
            "layer {}: {}",
            layer.display(),
            String::from_utf8_lossy(&listing.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&listing.stdout);
    let mut opaque_dirs: Vec<&str> = Vec::new();
    let mut whiteouts: Vec<(&str, &str)> = Vec::new(); // (parent dir, deleted name)
    for raw in text.lines() {
        let e = raw.strip_prefix("./").unwrap_or(raw).trim_end_matches('/');
        let (dir, base) = match e.rsplit_once('/') {
            Some((d, b)) => (d, b),
            None => ("", e),
        };
        if base == ".wh..wh..opq" {
            opaque_dirs.push(dir);
        } else if let Some(name) = base.strip_prefix(".wh.") {
            whiteouts.push((dir, name));
        }
    }
    // Opaque: clear the merged dir's existing (lower) contents before applying this layer.
    for dir in opaque_dirs {
        let d = if dir.is_empty() { merged.to_path_buf() } else { merged.join(dir) };
        if let Ok(rd) = std::fs::read_dir(&d) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_dir() && !p.is_symlink() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    // Regular whiteouts: delete the named entry from the merged tree.
    for (dir, name) in whiteouts {
        let victim = if dir.is_empty() { merged.join(name) } else { merged.join(dir).join(name) };
        if victim.is_dir() && !victim.is_symlink() {
            let _ = std::fs::remove_dir_all(&victim);
        } else {
            let _ = std::fs::remove_file(&victim);
        }
    }
    // Extract the layer's real content over the merged tree, dropping the whiteout markers.
    run_extract_args(layer, merged, &["--exclude", ".wh.*", "--exclude", "*/.wh.*"])
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

    // Finding 2 — a standard `docker save` archive (manifest.json + config blob + ordered layer tars)
    // loads: layers flatten in order, whiteouts + opaque dirs are honored, and identity/run config come
    // from RepoTags[0] + the config blob.
    #[test]
    fn load_docker_save_archive_flattens_layers_and_whiteouts() {
        let work = unique_dir("dl-work");

        // layer1: a.txt + keep/inside.txt
        let l1 = work.join("l1");
        write_file(&l1.join("a.txt"), b"from-l1\n");
        write_file(&l1.join("keep/inside.txt"), b"lower\n");
        let layer1 = tar_members(&l1, &["a.txt", "keep"]);

        // layer2: adds c.txt, whiteouts a.txt, and makes keep/ opaque while adding keep/new.txt
        let l2 = work.join("l2");
        write_file(&l2.join("c.txt"), b"from-l2\n");
        write_file(&l2.join(".wh.a.txt"), b"");
        write_file(&l2.join("keep/.wh..wh..opq"), b"");
        write_file(&l2.join("keep/new.txt"), b"upper\n");
        let layer2 = tar_members(&l2, &["c.txt", ".wh.a.txt", "keep"]);

        let arc = work.join("arc");
        write_file(&arc.join("layer1.tar"), &layer1);
        write_file(&arc.join("layer2.tar"), &layer2);
        let config = r#"{"architecture":"amd64","os":"linux","config":{
            "Cmd":["/bin/sh","-c","run"],"Env":["PATH=/bin","X=1"],"Entrypoint":["/entry"],
            "WorkingDir":"/w","User":"app","ExposedPorts":{"7000/tcp":{}},
            "StopSignal":"SIGINT","Volumes":{"/vol":{}}}}"#;
        write_file(&arc.join("config.json"), config.as_bytes());
        let manifest = r#"[{"Config":"config.json","RepoTags":["myrepo/app:v9"],
            "Layers":["layer1.tar","layer2.tar"]}]"#;
        write_file(&arc.join("manifest.json"), manifest.as_bytes());
        let outer = tar_members(&arc, &["manifest.json", "config.json", "layer1.tar", "layer2.tar"]);

        let store_dir = unique_dir("dl-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let loaded = store.load_archive(&outer).expect("docker save archive loads");

        // Identity from RepoTags[0]; arch from the config blob.
        assert_eq!(loaded.name, "myrepo/app:v9");
        assert_eq!(loaded.arch, Arch::LinuxX86_64);
        // Run config recovered from the config blob's `config` section.
        assert_eq!(loaded.cmd, vec!["/bin/sh", "-c", "run"]);
        assert_eq!(loaded.env, vec!["PATH=/bin", "X=1"]);
        assert_eq!(loaded.entrypoint, vec!["/entry"]);
        assert_eq!(loaded.workdir, "/w");
        assert_eq!(loaded.user, "app");
        assert_eq!(loaded.exposed_ports, vec!["7000/tcp"]);
        assert_eq!(loaded.stop_signal, "SIGINT");
        assert_eq!(loaded.img_volumes, vec!["/vol"]);

        // Merged rootfs: c.txt added; a.txt whiteouted away; keep/ opaque cleared its lower file but kept
        // the upper one; no whiteout markers leaked.
        let rootfs = &loaded.rootfs;
        assert_eq!(std::fs::read_to_string(rootfs.join("c.txt")).unwrap(), "from-l2\n");
        assert!(!rootfs.join("a.txt").exists(), "a.txt should be whiteouted away");
        assert!(!rootfs.join("keep/inside.txt").exists(), "opaque should clear the lower file");
        assert_eq!(std::fs::read_to_string(rootfs.join("keep/new.txt")).unwrap(), "upper\n");
        assert!(!rootfs.join(".wh.a.txt").exists(), "whiteout marker must not leak");
        assert!(!rootfs.join("keep/.wh..wh..opq").exists(), "opaque marker must not leak");

        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Finding 6 — a PRESENT but malformed dd-manifest.json is an ERROR (not swallowed to rootfs-only).
    #[test]
    fn load_malformed_dd_manifest_errors() {
        let src = unique_dir("mm-src");
        write_file(&src.join("rootfs/f"), b"x\n");
        write_file(&src.join("dd-manifest.json"), b"{ this is : not valid json ]");
        let bytes = tar_members(&src, &["rootfs", "dd-manifest.json"]);

        let store_dir = unique_dir("mm-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let err = store.load_archive(&bytes).expect_err("malformed manifest must error");
        assert!(err.to_string().contains("malformed dd-manifest.json"), "err: {err}");

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Finding 8 — a dd manifest with an unsupported os (windows) must be rejected, not imported as Linux.
    #[test]
    fn load_unsupported_os_manifest_errors() {
        let src = unique_dir("os-src");
        write_file(&src.join("rootfs/f"), b"x\n");
        write_file(&src.join("dd-manifest.json"), br#"{"name":"win:1","os":"windows"}"#);
        let bytes = tar_members(&src, &["rootfs", "dd-manifest.json"]);

        let store_dir = unique_dir("os-store");
        let store = Store::new(store_dir.to_str().unwrap());
        let err = store.load_archive(&bytes).expect_err("windows os must error");
        assert!(err.to_string().contains("unsupported image os"), "err: {err}");

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    // Finding 10 — loading an archive whose name matches an existing image swaps the rootfs into place
    // (old replaced by new) and leaves no staging/aside dirs behind.
    #[test]
    fn load_same_name_swaps_without_leftovers() {
        let src = unique_dir("sn-src");
        let rootfs = src.join("rootfs");
        write_file(&rootfs.join("new-sentinel"), b"new\n");
        let manifest = Manifest { name: "same/app:1".to_string(), ..Default::default() };
        let bytes = Store::new("/unused").save_archive(&rootfs, &manifest).unwrap();

        let store_dir = unique_dir("sn-store");
        let store = Store::new(store_dir.to_str().unwrap());
        // Pre-existing image at the same name with an OLD sentinel.
        let target = store.dir_for("same/app:1");
        std::fs::create_dir_all(target.join("rootfs")).unwrap();
        std::fs::write(target.join("rootfs/old-sentinel"), b"old\n").unwrap();

        let loaded = store.load_archive(&bytes).expect("same-name load");
        assert!(loaded.rootfs.join("new-sentinel").exists(), "new content installed");
        assert!(!loaded.rootfs.join("old-sentinel").exists(), "old content replaced");

        // No `.load-*` / `.merge-*` / `.old-*` scratch dirs remain in the store root.
        let leftovers: Vec<_> = std::fs::read_dir(&store_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "scratch dirs left behind: {leftovers:?}");

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
