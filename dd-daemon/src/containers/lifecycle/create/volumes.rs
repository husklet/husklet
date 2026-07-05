//! Anonymous-volume seeding for `POST /containers/create` (Moby's populateVolumes):
//! normalize a mount target (`norm_dir`), recursively copy image content into a fresh
//! volume (`copy_dir_into`), and materialize an anonymous local volume seeded from the
//! image at a target path (`anon_volume`). Stateless helpers.
use super::super::super::*;

/// Normalize a container mount target for dedup: strip a trailing slash (except root). `/data/` and
/// `/data` name the same mount point, so an image VOLUME at a `-v`-covered path isn't duplicated.
pub(super) fn norm_dir(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        "/".into()
    } else {
        t.to_string()
    }
}

/// Recursively copy the contents of `src` INTO `dst` (files, dirs, symlinks) — Moby's `populateVolumes`:
/// a freshly-created anonymous volume is seeded with the image's existing content at the mount point, so
/// a `VOLUME /var/lib/postgresql/data` over a populated image dir keeps those files instead of hiding
/// them behind an empty mount. Best-effort (never fails the create on an I/O error).
fn copy_dir_into(src: &std::path::Path, dst: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(src) else {
        return;
    };
    for e in rd.flatten() {
        let from = e.path();
        let to = dst.join(e.file_name());
        match e.file_type() {
            Ok(ft) if ft.is_dir() => {
                let _ = std::fs::create_dir_all(&to);
                copy_dir_into(&from, &to);
            }
            Ok(ft) if ft.is_symlink() => {
                if let Ok(t) = std::fs::read_link(&from) {
                    let _ = std::os::unix::fs::symlink(t, &to);
                }
            }
            _ => {
                let _ = std::fs::copy(&from, &to);
            }
        }
    }
}

/// Create an ANONYMOUS local volume (64-hex name, docker-style) backing container path `target`, seeding
/// it from the image's content at that path (populateVolumes). Returns the [`Vol`]; the caller registers
/// it + records the name in the container's `anon_volumes` (so `rm -v`/prune can reclaim it).
pub(super) fn anon_volume(volumes_dir: &str, image_rootfs: &str, target: &str, cid: &str) -> Vol {
    let name = fake_id(&format!("anon:{cid}:{target}:{}", now_nanos()));
    let mountpoint = PathBuf::from(volumes_dir).join(&name);
    let _ = std::fs::create_dir_all(&mountpoint);
    let src = PathBuf::from(image_rootfs).join(target.trim_start_matches('/'));
    if src.is_dir() {
        copy_dir_into(&src, &mountpoint);
    }
    Vol {
        name,
        mountpoint: mountpoint.to_string_lossy().into_owned(),
        created_at: now_secs(),
        driver: "local".into(),
        options: HashMap::new(),
        labels: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- norm_dir: strip a trailing slash for mount-target dedup (root stays "/") ----
    #[test]
    fn norm_dir_plain_path_unchanged() {
        assert_eq!(norm_dir("/data"), "/data");
        assert_eq!(norm_dir("/var/lib/postgresql/data"), "/var/lib/postgresql/data");
    }
    #[test]
    fn norm_dir_strips_single_trailing_slash() {
        assert_eq!(norm_dir("/data/"), "/data");
    }
    #[test]
    fn norm_dir_strips_multiple_trailing_slashes() {
        // trim_end_matches removes ALL trailing slashes.
        assert_eq!(norm_dir("/data///"), "/data");
    }
    #[test]
    fn norm_dir_root_stays_root() {
        assert_eq!(norm_dir("/"), "/");
        assert_eq!(norm_dir("///"), "/");
    }
    #[test]
    fn norm_dir_empty_becomes_root() {
        // An all-slash-or-empty target collapses to "/" (never the empty string).
        assert_eq!(norm_dir(""), "/");
    }
    #[test]
    fn norm_dir_relative_and_no_leading_slash() {
        // Only the TRAILING slash is touched; a relative target keeps its shape.
        assert_eq!(norm_dir("data/"), "data");
        assert_eq!(norm_dir("data"), "data");
    }

    // ---- copy_dir_into: recursive seed of a fresh volume from image content ----
    // A unique scratch dir removed on drop (the established temp_dir + RAII idiom; no tempfile dep).
    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "dd_createvol_test_{}_{}_{}",
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
    fn copy_dir_into_copies_files_dirs_and_symlinks() {
        let root = Tmp::new("copy");
        let src = root.0.join("src");
        let dst = root.0.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        // a top-level file, a nested subdir + file, and a relative symlink to the top-level file.
        std::fs::write(src.join("a"), b"hello").unwrap();
        std::fs::create_dir_all(src.join("d")).unwrap();
        std::fs::write(src.join("d").join("b"), b"world").unwrap();
        std::os::unix::fs::symlink("a", src.join("l")).unwrap();

        copy_dir_into(&src, &dst);

        // regular file copied with its content
        assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"hello");
        // nested dir recursed into
        assert_eq!(std::fs::read(dst.join("d").join("b")).unwrap(), b"world");
        // symlink recreated AS a symlink (not dereferenced) pointing at the same relative target
        let meta = std::fs::symlink_metadata(dst.join("l")).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(std::fs::read_link(dst.join("l")).unwrap(), std::path::Path::new("a"));
    }

    #[test]
    fn copy_dir_into_missing_src_is_noop() {
        let root = Tmp::new("nosrc");
        let dst = root.0.join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        // A non-existent source directory must not error and must leave dst empty.
        copy_dir_into(&root.0.join("does-not-exist"), &dst);
        assert_eq!(std::fs::read_dir(&dst).unwrap().count(), 0);
    }
}
