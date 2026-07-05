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

#[cfg(test)]
mod tests {
    use super::*;

    // A unique scratch dir so parallel test runs don't collide; removed on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "dd_paths_test_{}_{}_{}",
                tag,
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dir_size_empty_is_zero() {
        let t = Tmp::new("empty");
        assert_eq!(dir_size(&t.0), 0);
    }

    #[test]
    fn dir_size_missing_path_is_zero() {
        // An unreadable/absent path is 0, not an error.
        assert_eq!(dir_size(std::path::Path::new("/dd/no/such/path/xyzzy")), 0);
    }

    #[test]
    fn dir_size_sums_regular_files_recursively() {
        let t = Tmp::new("recurse");
        std::fs::write(t.0.join("a"), b"hello").unwrap(); // 5 bytes
        let sub = t.0.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("b"), b"abc").unwrap(); // 3 bytes
        assert_eq!(dir_size(&t.0), 8);
    }

    #[test]
    fn dir_size_skips_symlinks() {
        let t = Tmp::new("symlink");
        std::fs::write(t.0.join("real"), b"1234").unwrap(); // 4 bytes
        // A symlink is neither followed nor counted (its own len is excluded).
        std::os::unix::fs::symlink(t.0.join("real"), t.0.join("link")).unwrap();
        assert_eq!(dir_size(&t.0), 4);
    }
}
