//! `state.json` persistence: atomic save + arch-re-resolving load of containers/volumes/networks.
use super::*;

/// Write containers/volumes/networks to `path` atomically (temp file + rename). Best-effort:
/// persistence failures are logged but never abort a request. Durability-sensitive handlers that must
/// fail/roll back on a persistence error call [`save_state_checked`] instead.
pub(crate) fn save_state(inner: &Inner, path: &str) {
    if let Err(e) = save_state_checked(inner, path) {
        eprintln!("[hl-daemon] state save failed: {e}");
    }
}

/// Like [`save_state`] but returns the I/O error so a caller can fail the request or roll back the
/// in-memory mutation — a successful `201`/`204` must not describe state that will vanish on restart.
pub(crate) fn save_state_checked(inner: &Inner, path: &str) -> std::io::Result<()> {
    let p = Persisted {
        containers: inner.containers.values().cloned().collect(),
        volumes: inner.volumes.clone(),
        networks: inner.networks.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&p)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

/// Load persisted state into `inner`, re-resolving each container's arch/rootfs from the
/// freshly discovered images.
pub(crate) fn load_state(inner: &mut Inner, path: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(p) = serde_json::from_slice::<Persisted>(&bytes) else {
        eprintln!("[hl-daemon] ignoring unreadable state file {path}");
        return;
    };
    for mut c in p.containers {
        // Re-resolve arch/rootfs from the freshly discovered images. Match on the persisted rootfs first
        // (exact, so a re-tagged image still resolves), then fall back to the SAME lenient resolver that
        // `containers_create` uses (repository + tag aware). The prior exact/`{name}:latest` string match
        // missed the common case — a container created from a BARE ref (`docker run nginx`, stored
        // `c.image = "nginx"`) never matched its pulled image named `nginx:latest`, so EVERY pulled-image
        // container fell through to the arm64 default. Harmless for arm64, but it silently reset an amd64
        // container's arch to arm64 across a daemon restart (it would then run x86 code on the arm64 engine).
        let resolved = inner
            .images
            .iter()
            .find(|i| !c.rootfs.is_empty() && i.rootfs == c.rootfs)
            .or_else(|| crate::util::find_image(&inner.images, &c.image));
        match resolved {
            Some(img) => {
                c.arch = Some(img.arch);
                c.rootfs = img.rootfs.clone();
            }
            // No image resolves: keep the PERSISTED arch (deserialized from state.json) rather than
            // forcing arm64 — an amd64 container whose image was removed must not silently switch engines
            // on restart. Only fall back to the arm64 default when nothing was persisted either.
            None => c.arch = c.arch.or(Some(Guest::LinuxAarch64)),
        }
        // A daemon restart loses every live process. A container persisted as running/paused/restarting has
        // no backing process after reload, so normalize it to a terminal state — otherwise inspect reports
        // Running=true with Pid=0 and `POST /start` becomes a 304 no-op instead of (re)starting it, and a
        // `restarting` container stays stuck forever with no supervisor to advance it. Docker uses exit code
        // 255 for containers still running at daemon shutdown.
        if matches!(c.status.as_str(), "running" | "paused" | "restarting") {
            c.status = "exited".into();
            if c.exit_code == 0 {
                c.exit_code = 255;
            }
            if c.finished_at == 0 {
                c.finished_at = crate::util::now_secs();
            }
        }
        // Restore the exited container's persisted logs (stdout/stderr are `#[serde(skip)]`, so the state
        // file carries none) so `docker logs` still returns output after a daemon restart.
        let logdir = crate::util::hl_home()
            .join("containers")
            .join(&c.id)
            .join("logs");
        if let Ok(b) = std::fs::read(logdir.join("stdout")) {
            c.stdout = b;
        }
        if let Ok(b) = std::fs::read(logdir.join("stderr")) {
            c.stderr = b;
        }
        inner.containers.insert(c.id.clone(), c);
    }
    inner.volumes = p.volumes;
    inner.networks = p.networks;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("hl-daemon-state-{}-{}.json", std::process::id(), tag))
            .to_string_lossy()
            .into_owned()
    }

    fn img(name: &str, rootfs: &str, arch: Guest) -> Image {
        Image { name: name.into(), rootfs: rootfs.into(), arch, ..Default::default() }
    }

    // REGRESSION: a container created from a BARE ref (`docker run --platform linux/amd64 nginx`) is stored
    // with `c.image = "nginx"`, while the pulled image is named `nginx:latest`. On a daemon restart the
    // arch/rootfs must re-resolve from that image. The old exact/`{name}:latest` string match missed this
    // and defaulted the container to arm64 — so an amd64 container would run x86 code on the arm64 engine.
    #[test]
    fn load_state_reresolves_amd64_arch_from_bare_ref() {
        let path = tmp("amd64");
        let _ = std::fs::remove_file(&path);
        let mut src = Inner::default();
        src.containers.insert(
            "c1".into(),
            Container { id: "c1".into(), image: "nginx".into(), rootfs: "/img/nginx/rootfs".into(), ..Default::default() },
        );
        save_state(&src, &path);

        let mut dst = Inner::default();
        dst.images = vec![img("nginx:latest", "/img/nginx/rootfs", Guest::LinuxX86_64)];
        load_state(&mut dst, &path);
        let got = dst.containers.get("c1").expect("container reloaded");
        assert_eq!(got.arch, Some(Guest::LinuxX86_64), "amd64 arch must survive a daemon restart");
        assert_eq!(got.rootfs, "/img/nginx/rootfs");
        let _ = std::fs::remove_file(&path);
    }

    // The rootfs match wins even when the image was re-tagged (name no longer matches the container's ref).
    #[test]
    fn load_state_matches_by_rootfs_when_name_diverged() {
        let path = tmp("rootfs");
        let _ = std::fs::remove_file(&path);
        let mut src = Inner::default();
        src.containers.insert(
            "c2".into(),
            Container { id: "c2".into(), image: "oldname".into(), rootfs: "/img/x/rootfs".into(), ..Default::default() },
        );
        save_state(&src, &path);
        let mut dst = Inner::default();
        dst.images = vec![img("renamed:latest", "/img/x/rootfs", Guest::LinuxX86_64)];
        load_state(&mut dst, &path);
        assert_eq!(dst.containers.get("c2").unwrap().arch, Some(Guest::LinuxX86_64));
        let _ = std::fs::remove_file(&path);
    }

    // "Restart State Load Overwrites Persisted Container Arch": when the image is gone the PERSISTED arch
    // must survive (an amd64 container must not silently become arm64 and run x86 on the wrong engine).
    #[test]
    fn load_state_preserves_persisted_arch_when_image_absent() {
        let path = tmp("persistarch");
        let _ = std::fs::remove_file(&path);
        let mut src = Inner::default();
        src.containers.insert(
            "c4".into(),
            Container { id: "c4".into(), image: "gone".into(), arch: Some(Guest::LinuxX86_64), ..Default::default() },
        );
        save_state(&src, &path);
        let mut dst = Inner::default(); // no images -> nothing resolves
        load_state(&mut dst, &path);
        assert_eq!(
            dst.containers.get("c4").unwrap().arch,
            Some(Guest::LinuxX86_64),
            "persisted amd64 arch must survive a restart with the image absent"
        );
        let _ = std::fs::remove_file(&path);
    }

    // "Daemon Restart Reloads Running Containers Without Live Process" / "Restarting Containers Can Stay
    // Stuck": a running/paused/restarting container has no process after reload, so it normalizes to exited.
    #[test]
    fn load_state_normalizes_orphaned_running_and_restarting_to_exited() {
        let path = tmp("orphanrun");
        let _ = std::fs::remove_file(&path);
        let mut src = Inner::default();
        for (id, st) in [("run1", "running"), ("pau1", "paused"), ("res1", "restarting")] {
            src.containers.insert(
                id.into(),
                Container { id: id.into(), image: "x".into(), status: st.into(), ..Default::default() },
            );
        }
        save_state(&src, &path);
        let mut dst = Inner::default();
        load_state(&mut dst, &path);
        for id in ["run1", "pau1", "res1"] {
            let c = dst.containers.get(id).unwrap();
            assert_eq!(c.status, "exited", "{id} must normalize to exited (no live process after restart)");
            assert_eq!(c.exit_code, 255, "{id} takes docker's shutdown exit code");
        }
        let _ = std::fs::remove_file(&path);
    }

    // "Retained Container Logs Are Lost Across Daemon Restart": an exited container's logs are persisted
    // to disk on exit and restored on reload, so `docker logs` still returns them after a restart.
    #[test]
    fn load_state_restores_persisted_logs_for_exited_container() {
        let cid = format!("logtest{}", std::process::id());
        let logdir = crate::util::hl_home().join("containers").join(&cid).join("logs");
        std::fs::create_dir_all(&logdir).unwrap();
        std::fs::write(logdir.join("stdout"), b"hello after restart\n").unwrap();
        std::fs::write(logdir.join("stderr"), b"warn line\n").unwrap();

        let path = tmp("logs");
        let _ = std::fs::remove_file(&path);
        let mut src = Inner::default();
        src.containers.insert(
            cid.clone(),
            Container { id: cid.clone(), image: "x".into(), status: "exited".into(), ..Default::default() },
        );
        save_state(&src, &path);
        let mut dst = Inner::default();
        load_state(&mut dst, &path);
        let c = dst.containers.get(&cid).unwrap();
        assert_eq!(c.stdout, b"hello after restart\n", "stdout restored from disk after restart");
        assert_eq!(c.stderr, b"warn line\n", "stderr restored from disk after restart");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(crate::util::hl_home().join("containers").join(&cid));
    }

    // No matching image at all AND no persisted arch -> the safe arm64 default (unchanged for orphans).
    #[test]
    fn load_state_defaults_arm64_when_image_absent() {
        let path = tmp("orphan");
        let _ = std::fs::remove_file(&path);
        let mut src = Inner::default();
        src.containers.insert(
            "c3".into(),
            Container { id: "c3".into(), image: "gone".into(), ..Default::default() },
        );
        save_state(&src, &path);
        let mut dst = Inner::default();
        load_state(&mut dst, &path);
        assert_eq!(dst.containers.get("c3").unwrap().arch, Some(Guest::LinuxAarch64));
        let _ = std::fs::remove_file(&path);
    }
}
