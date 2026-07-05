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
