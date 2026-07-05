//! Filesystem-path helpers rooted at the daemon state dir, plus recursive size accounting.
use super::*;

/// `~/.dd` (or `./.dd` if `$HOME` is unset) — the default state/volumes root.
pub(crate) fn dd_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dd")
}

/// `~/.dd/buildcache` — the `docker build` layer cache root (one dir per cached step under `layers/`).
/// Distinct from `~/.dd/pcache` (the JIT translated-code cache surfaced as `system df` BuilderSize).
pub(crate) fn buildcache_dir() -> PathBuf {
    dd_home().join("buildcache")
}

/// On-disk size of an image's rootfs, cached per rootfs path (computed once; rootfs rarely changes).
/// The host-fs `macos` image is skipped (walking `/` would be catastrophic).
pub(crate) fn image_size(rootfs: &str, name: &str) -> i64 {
    if name == "macos" {
        return 0;
    }
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(s) = cache.lock().unwrap().get(rootfs) {
        return *s;
    }
    let s = dir_size(std::path::Path::new(rootfs));
    cache.lock().unwrap().insert(rootfs.to_string(), s);
    s
}

/// Recursively sum the size of regular files under `p` (symlinks are not followed).
pub(crate) fn dir_size(p: &std::path::Path) -> i64 {
    let mut total = 0i64;
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    for e in rd.flatten() {
        let Ok(md) = e.path().symlink_metadata() else {
            continue;
        };
        let ft = md.file_type();
        if ft.is_symlink() {
            continue;
        } else if ft.is_dir() {
            total += dir_size(&e.path());
        } else {
            total += md.len() as i64;
        }
    }
    total
}
