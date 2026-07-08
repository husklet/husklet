//! `state.json` persistence: atomic save + arch-re-resolving load of containers/volumes/networks.
use super::*;

/// Write containers/volumes/networks to `path` atomically (temp file + rename). Best-effort:
/// persistence failures are logged but never abort a request.
pub(crate) fn save_state(inner: &Inner, path: &str) {
    let p = Persisted {
        containers: inner.containers.values().cloned().collect(),
        volumes: inner.volumes.clone(),
        networks: inner.networks.clone(),
    };
    let Ok(bytes) = serde_json::to_vec_pretty(&p) else {
        return;
    };
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!("[dd-daemon] state save failed: {e}");
        }
    }
}

/// Load persisted state into `inner`, re-resolving each container's arch/rootfs from the
/// freshly discovered images.
pub(crate) fn load_state(inner: &mut Inner, path: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(p) = serde_json::from_slice::<Persisted>(&bytes) else {
        eprintln!("[dd-daemon] ignoring unreadable state file {path}");
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
            None => c.arch = Some(Guest::LinuxAarch64),
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
            .join(format!("dd-daemon-state-{}-{}.json", std::process::id(), tag))
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

    // No matching image at all -> the safe arm64 default (behavior unchanged for orphaned containers).
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
