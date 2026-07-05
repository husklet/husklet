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
        if let Some(img) = inner
            .images
            .iter()
            .find(|i| i.name == c.image || format!("{}:latest", i.name) == c.image)
        {
            c.arch = Some(img.arch);
            c.rootfs = img.rootfs.clone();
        } else {
            c.arch = Some(Guest::LinuxAarch64);
        }
        inner.containers.insert(c.id.clone(), c);
    }
    inner.volumes = p.volumes;
    inner.networks = p.networks;
}
