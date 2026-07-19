//! `docker diff` — the container's copy-on-write upper layer diffed against the image rootfs, plus the
//! layer-reclaim used by `docker rm`/prune.
use super::super::*;

/// Reclaim a container's private writable upper layer (its copy-on-write files + whiteouts). hl gives
/// each container an UPPER over the read-only image rootfs, so `docker rm`/prune drops it just as docker
/// drops the container's writable layer — the shared image (the lower) is never touched. Removes the whole
/// `<hl_home>/containers/<id>` tree (the `upper` dir's parent). A no-op for darwin/flat-rootfs containers
/// (empty `upper`).
/// Reclaim a container's private writable upper layer. Returns the I/O result so `docker rm` can fail
/// (keeping state for retry) rather than silently orphaning the layer while reporting success. An empty
/// upper (darwin / legacy containers) or an already-absent dir is `Ok(())` (nothing to do).
pub(crate) struct Overlay<'a> {
    pub(crate) upper: &'a str,
    pub(crate) rootfs: &'a str,
}

impl Overlay<'_> {
    pub(crate) fn discard(&self) -> std::io::Result<()> {
        if self.upper.is_empty() {
            return Ok(());
        }
        let dir = std::path::Path::new(self.upper)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(self.upper));
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// Diff a container's copy-on-write upper layer against the image rootfs (the lower), producing the
    /// Docker `diff` kinds keyed by container-absolute path: 0=Modified, 1=Added, 2=Deleted. A file/symlink
    /// present in the upper is Modified if it also exists in the lower, else Added; a `.wh.NAME` whiteout
    /// marks NAME Deleted; a directory present only in the upper is Added (a copied-up dir that also exists
    /// in the lower is merely a parent and is surfaced via ancestor marking). Every ancestor directory of a
    /// change is then marked Modified, matching docker (`C /etc` for `A /etc/foo`).
    pub(crate) fn changes(&self) -> HashMap<String, u8> {
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
                    if std::fs::symlink_metadata(format!("{rootfs}{path}")).is_err() {
                        out.insert(path.clone(), 1);
                    }
                    walk(&e.path(), &path, rootfs, out);
                } else {
                    let kind = if std::fs::symlink_metadata(format!("{rootfs}{path}")).is_ok() {
                        0
                    } else {
                        1
                    };
                    out.insert(path, kind);
                }
            }
        }
        let mut out = HashMap::new();
        walk(std::path::Path::new(self.upper), "", self.rootfs, &mut out);
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
}

/// `GET /containers/{id}/changes` — `docker diff`. hl gives each container a copy-on-write UPPER over the
/// read-only image rootfs, so the changes are exactly that upper layer diffed against the image (see
/// `overlay_changes`). Reports the Docker shape: an array of `{Path, Kind}` (0=modified, 1=added,
/// 2=deleted), with each changed entry's ancestor directories also reported as modified, as docker does.
/// A darwin/flat-rootfs container (no upper) reports none.
impl Containers {
    pub(crate) async fn changes(State(a): State<App>, Path(id): Path<String>) -> Response {
        let (upper, rootfs) = {
            let g = a.inner.lock().await;
            let Some((_, c)) = ContainerId::get(&g, &id) else {
                return ErrorMessage::no_such(&id);
            };
            (c.upper.clone(), c.rootfs.clone())
        };
        if upper.is_empty() {
            return Json(Vec::<crate::api::ContainerChange>::new()).into_response();
        }
        let kinds = tokio::task::spawn_blocking(move || {
            Overlay {
                upper: &upper,
                rootfs: &rootfs,
            }
            .changes()
        })
        .await
        .unwrap_or_default();
        let mut out: Vec<crate::api::ContainerChange> = kinds
            .into_iter()
            .map(|(p, k)| crate::api::ContainerChange { path: p, kind: k })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Json(out).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A unique scratch dir (with `upper`/`lower` children) so parallel test runs don't collide;
    // removed on drop. Same std-temp_dir + RAII idiom as util::paths' tests (no `tempfile` dep).
    struct Tmp {
        root: PathBuf,
        upper: PathBuf,
        lower: PathBuf,
    }
    impl Tmp {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "hl_diff_test_{}_{}_{}",
                tag,
                std::process::id(),
                n
            ));
            let upper = root.join("upper");
            let lower = root.join("lower");
            std::fs::create_dir_all(&upper).unwrap();
            std::fs::create_dir_all(&lower).unwrap();
            Tmp { root, upper, lower }
        }
        fn upper(&self) -> &str {
            self.upper.to_str().unwrap()
        }
        fn lower(&self) -> &str {
            self.lower.to_str().unwrap()
        }
        // Create a file (with parent dirs) at `rel` under the given base.
        fn write(base: &std::path::Path, rel: &str, body: &[u8]) {
            let p = base.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        fn mkdir(base: &std::path::Path, rel: &str) {
            std::fs::create_dir_all(base.join(rel)).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn expect(pairs: &[(&str, u8)]) -> HashMap<String, u8> {
        pairs.iter().map(|(p, k)| (p.to_string(), *k)).collect()
    }

    // (a) A file present in the upper but not the lower is Added (1). Its only ancestor, the
    // root, is surfaced as Modified (0) — docker always reports the chain up to `/`.
    #[test]
    fn added_file_is_kind_1_with_root_ancestor() {
        let t = Tmp::new("added");
        Tmp::write(&t.upper, "newfile", b"hi");
        let got = Overlay {
            upper: t.upper(),
            rootfs: t.lower(),
        }
        .changes();
        assert_eq!(got, expect(&[("/newfile", 1), ("/", 0)]));
    }

    // (b) A file present in BOTH upper and lower is Modified (0).
    #[test]
    fn file_in_both_is_kind_0_modified() {
        let t = Tmp::new("modified");
        Tmp::write(&t.upper, "existing", b"new-contents");
        Tmp::write(&t.lower, "existing", b"old-contents");
        let got = Overlay {
            upper: t.upper(),
            rootfs: t.lower(),
        }
        .changes();
        assert_eq!(got, expect(&[("/existing", 0), ("/", 0)]));
    }

    // (c) A `.wh.<name>` whiteout marks <name> Deleted (2), regardless of the lower. The stripped
    // name (not the `.wh.` file) is what's reported.
    #[test]
    fn whiteout_marks_path_deleted_kind_2() {
        let t = Tmp::new("whiteout");
        Tmp::write(&t.upper, ".wh.gone", b""); // whiteout marker file
        Tmp::write(&t.lower, "gone", b"was-here");
        let got = Overlay {
            upper: t.upper(),
            rootfs: t.lower(),
        }
        .changes();
        assert_eq!(got, expect(&[("/gone", 2), ("/", 0)]));
    }

    // (d) A nested added file whose ancestor dir ALSO exists in the lower: the ancestor is a
    // copied-up dir, so it is NOT emitted as Added by the walk (it's in the lower) but IS surfaced
    // as Modified (0) via ancestor marking — docker's `C /etc` for `A /etc/foo`.
    #[test]
    fn nested_add_marks_existing_ancestor_dir_modified() {
        let t = Tmp::new("nested_existing");
        Tmp::mkdir(&t.upper, "etc");
        Tmp::write(&t.upper, "etc/newfile", b"x");
        Tmp::mkdir(&t.lower, "etc"); // ancestor exists in lower => modified, not added
        let got = Overlay {
            upper: t.upper(),
            rootfs: t.lower(),
        }
        .changes();
        assert_eq!(got, expect(&[("/etc/newfile", 1), ("/etc", 0), ("/", 0)]));
    }

    // (d') A nested added file whose ancestor dirs are absent from the lower: each new dir is
    // itself Added (1), and every ancestor up to root appears. (or_insert never downgrades an
    // Added dir to Modified.)
    #[test]
    fn nested_add_new_dirs_are_each_added() {
        let t = Tmp::new("nested_new");
        Tmp::write(&t.upper, "a/b/c", b"deep");
        let got = Overlay {
            upper: t.upper(),
            rootfs: t.lower(),
        }
        .changes();
        assert_eq!(
            got,
            expect(&[("/a", 1), ("/a/b", 1), ("/a/b/c", 1), ("/", 0)])
        );
    }

    // A combined tree exercising add + modify + whiteout together, confirming the full
    // {path -> kind} set (including all ancestor Modified marks) is exactly as expected.
    #[test]
    fn combined_tree_exact_set() {
        let t = Tmp::new("combined");
        // added file under a new dir
        Tmp::write(&t.upper, "opt/added", b"a");
        // modified file (dir copied up, present in lower)
        Tmp::mkdir(&t.upper, "etc");
        Tmp::write(&t.upper, "etc/conf", b"changed");
        Tmp::mkdir(&t.lower, "etc");
        Tmp::write(&t.lower, "etc/conf", b"orig");
        // whiteout deleting /etc/old
        Tmp::write(&t.upper, "etc/.wh.old", b"");
        Tmp::write(&t.lower, "etc/old", b"gone");
        let got = Overlay {
            upper: t.upper(),
            rootfs: t.lower(),
        }
        .changes();
        assert_eq!(
            got,
            expect(&[
                ("/opt", 1),       // new dir, added
                ("/opt/added", 1), // added file
                ("/etc", 0),       // copied-up dir -> modified via ancestor marking
                ("/etc/conf", 0),  // present in both -> modified
                ("/etc/old", 2),   // whiteout -> deleted
                ("/", 0),          // root ancestor
            ])
        );
    }

    // An empty upper (no changes) yields no entries at all — not even the root.
    #[test]
    fn empty_upper_is_empty_map() {
        let t = Tmp::new("empty");
        let got = Overlay {
            upper: t.upper(),
            rootfs: t.lower(),
        }
        .changes();
        assert!(got.is_empty());
    }
}
