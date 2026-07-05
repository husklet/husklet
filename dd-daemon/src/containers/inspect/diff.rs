//! `docker diff` — the container's copy-on-write upper layer diffed against the image rootfs, plus the
//! layer-reclaim used by `docker rm`/prune.
use super::super::*;

/// Reclaim a container's private writable upper layer (its copy-on-write files + whiteouts). dd gives
/// each container an UPPER over the read-only image rootfs, so `docker rm`/prune drops it just as docker
/// drops the container's writable layer — the shared image (the lower) is never touched. Removes the whole
/// `<dd_home>/containers/<id>` tree (the `upper` dir's parent). A no-op for darwin/flat-rootfs containers
/// (empty `upper`).
pub(crate) fn discard_container_layer(upper: &str) {
    if upper.is_empty() {
        return;
    }
    let dir = std::path::Path::new(upper)
        .parent()
        .unwrap_or_else(|| std::path::Path::new(upper));
    let _ = std::fs::remove_dir_all(dir);
}

/// Diff a container's copy-on-write upper layer against the image rootfs (the lower), producing the
/// Docker `diff` kinds keyed by container-absolute path: 0=Modified, 1=Added, 2=Deleted. A file/symlink
/// present in the upper is Modified if it also exists in the lower, else Added; a `.wh.NAME` whiteout
/// marks NAME Deleted; a directory present only in the upper is Added (a copied-up dir that also exists
/// in the lower is merely a parent and is surfaced via ancestor marking). Every ancestor directory of a
/// change is then marked Modified, matching docker (`C /etc` for `A /etc/foo`).
pub(crate) fn overlay_changes(upper: &str, rootfs: &str) -> HashMap<String, u8> {
    fn in_lower(rootfs: &str, path: &str) -> bool {
        std::fs::symlink_metadata(format!("{rootfs}{path}")).is_ok()
    }
    fn walk(dir: &std::path::Path, prefix: &str, rootfs: &str, out: &mut HashMap<String, u8>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if let Some(stripped) = name.strip_prefix(".wh.") {
                out.insert(format!("{prefix}/{stripped}"), 2); // whiteout -> deleted
                continue;
            }
            let Ok(md) = e.path().symlink_metadata() else {
                continue;
            };
            let path = format!("{prefix}/{name}");
            if md.file_type().is_dir() {
                if !in_lower(rootfs, &path) {
                    out.insert(path.clone(), 1);
                }
                walk(&e.path(), &path, rootfs, out);
            } else {
                let kind = if in_lower(rootfs, &path) { 0 } else { 1 };
                out.insert(path, kind);
            }
        }
    }
    let mut out = HashMap::new();
    walk(std::path::Path::new(upper), "", rootfs, &mut out);
    // Mark every ancestor directory of a change as modified (docker reports `C /etc` for `A /etc/foo`),
    // without overriding a more specific Added/Deleted on that ancestor itself.
    let leaves: Vec<String> = out.keys().cloned().collect();
    for path in leaves {
        let mut p = path.as_str();
        while let Some(idx) = p.rfind('/') {
            let parent = if idx == 0 { "/" } else { &p[..idx] };
            out.entry(parent.to_string()).or_insert(0);
            if idx == 0 {
                break;
            }
            p = &p[..idx];
        }
    }
    out
}

/// `GET /containers/{id}/changes` — `docker diff`. dd gives each container a copy-on-write UPPER over the
/// read-only image rootfs, so the changes are exactly that upper layer diffed against the image (see
/// `overlay_changes`). Reports the Docker shape: an array of `{Path, Kind}` (0=modified, 1=added,
/// 2=deleted), with each changed entry's ancestor directories also reported as modified, as docker does.
/// A darwin/flat-rootfs container (no upper) reports none.
pub(crate) async fn containers_changes(State(a): State<App>, Path(id): Path<String>) -> Response {
    let (upper, rootfs) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, &id) else {
            return no_such(&id);
        };
        let Some(c) = g.containers.get(&full) else {
            return no_such(&id);
        };
        (c.upper.clone(), c.rootfs.clone())
    };
    if upper.is_empty() {
        return Json(Vec::<crate::api::ContainerChange>::new()).into_response();
    }
    let kinds = tokio::task::spawn_blocking(move || overlay_changes(&upper, &rootfs))
        .await
        .unwrap_or_default();
    let mut out: Vec<crate::api::ContainerChange> = kinds
        .into_iter()
        .map(|(p, k)| crate::api::ContainerChange { path: p, kind: k })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Json(out).into_response()
}
