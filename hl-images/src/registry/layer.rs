//! Rootfs / whiteout / tar / gzip / sha256 tools used while unpacking and building layers.

use crate::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn reset_dir(p: &Path) -> Result<(), Error> {
    // A previous (possibly failed) extraction can leave read-only dirs (e.g. a base layer's
    // `dr-xr-xr-x` cert dir); `remove_dir_all` can't unlink entries inside a write-less dir, so re-add
    // owner-write to every dir first — otherwise stale content would survive the reset.
    if p.exists() {
        let _ = Command::new("find")
            .arg(p)
            .args(["-type", "d", "-exec", "chmod", "u+w", "{}", "+"])
            .output();
    }
    let _ = std::fs::remove_dir_all(p);
    std::fs::create_dir_all(p).map_err(|e| Error::Archive(format!("mkdir {}: {e}", p.display())))
}

const WH_PREFIX: &str = ".wh.";
const WH_OPAQUE: &str = ".wh..wh..opq";

/// Apply OCI whiteouts left by a just-extracted layer: a `.wh.<name>` marker deletes the sibling
/// `<name>`, and `.wh..wh..opq` clears the directory's lower contents (we just drop the marker — the
/// layers are already flattened). Done with a plain filesystem walk rather than a `find | while …
/// dirname/basename/rm` pipeline: a degenerate marker name can't make a shell utility error out
/// ("sh failed: …") nor, worse, delete the wrong path (a bare `.wh.` made the old script run
/// `rm -rf "$dir/"`, wiping the parent directory).
pub(super) fn apply_whiteouts(rootfs: &Path) -> Result<(), Error> {
    // Enumerate every marker first, then apply: a deletion can remove a whole subtree that itself
    // holds further markers, so we must not mutate the tree while still walking it.
    let mut markers = Vec::new();
    collect_whiteouts(rootfs, &mut markers);
    for marker in &markers {
        let name = marker
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The opaque marker has no sibling to delete; any other `.wh.<name>` hides the sibling `<name>`.
        // A marker that is *only* the `.wh.` prefix (empty target) is malformed — drop it without
        // deleting anything rather than removing its parent directory.
        if name != WH_OPAQUE {
            if let Some(target) = name.strip_prefix(WH_PREFIX).filter(|t| !t.is_empty()) {
                if let Some(parent) = marker.parent() {
                    remove_path(&parent.join(target));
                }
            }
        }
        let _ = std::fs::remove_file(marker);
    }
    Ok(())
}

/// Directories a layer marks OPAQUE via a `.wh..wh..opq` entry, as rootfs-relative paths (an empty string
/// means the rootfs root itself). Read straight from the layer tar (before extraction) so we can clear the
/// dir's flattened lower content first.
pub(super) fn opaque_dirs_in_tar(tar_gz: &Path) -> Vec<String> {
    let out = match Command::new("tar").arg("tzf").arg(tar_gz).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let p = l.trim_end_matches('/');
            let name = p.rsplit('/').next()?;
            if name != WH_OPAQUE {
                return None;
            }
            // the marker's parent dir, normalized (drop the leading "./" tar prefix and any leading '/').
            let parent = p[..p.len() - name.len()].trim_end_matches('/');
            Some(
                parent
                    .trim_start_matches("./")
                    .trim_start_matches('/')
                    .to_string(),
            )
        })
        .collect()
}

/// Clear the flattened lower-layer content of each opaque dir (remove the subtree, recreate it empty) so
/// that only the current layer's entries survive when it extracts on top.
pub(super) fn clear_opaque_dirs(rootfs: &Path, dirs: &[String]) {
    for d in dirs {
        // CONTAINMENT: a malicious layer can carry an opaque marker whose parent dir uses `..`
        // components (`../outside/.wh..wh..opq`). Joining that onto the rootfs and removing it would
        // delete files OUTSIDE the rootfs. Only clear a dir whose normalized rootfs-relative path stays
        // strictly under the rootfs; skip anything that escapes.
        let Some(rel) = contained_relative(d) else {
            continue;
        };
        let target = if rel.is_empty() {
            rootfs.to_path_buf()
        } else {
            rootfs.join(&rel)
        };
        // A base layer may have left the dir read-only (see reset_dir); re-add owner-write so it can be
        // cleared, then remove + recreate empty.
        if target.exists() {
            let _ = Command::new("find")
                .arg(&target)
                .args(["-type", "d", "-exec", "chmod", "u+w", "{}", "+"])
                .output();
        }
        let _ = std::fs::remove_dir_all(&target);
        let _ = std::fs::create_dir_all(&target);
    }
}

/// Collect every `.wh.*` marker under `dir`, recursing into real subdirectories only (symlinks are not
/// followed, so a layer can't redirect the walk outside the rootfs).
fn collect_whiteouts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(WH_PREFIX) {
            out.push(entry.path());
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_whiteouts(&entry.path(), out);
        }
    }
}

/// Normalize a rootfs-relative path from a layer, returning `Some(components-joined)` only if it stays
/// strictly under the rootfs. Returns `None` if it escapes (a leading/embedded `..` that pops above the
/// root) or is absolute. An empty/`.`-only path normalizes to `""` (the rootfs root itself). Purely
/// lexical (no fs access), which is exactly what we want for an untrusted archive-supplied path.
fn contained_relative(rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None; // absolute -> never rootfs-relative
    }
    let mut stack: Vec<&str> = Vec::new();
    for comp in rel.split('/') {
        match comp {
            "" | "." => continue,
            ".." => {
                // popping past the root escapes the rootfs -> reject the whole path
                stack.pop()?;
            }
            c => stack.push(c),
        }
    }
    Some(stack.join("/"))
}

/// Remove a path whether it's a file, a symlink, or a directory subtree; missing is success.
fn remove_path(p: &Path) {
    let _ = match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(_) => Ok(()),
    };
}

/// `tar | gzip` a rootfs into `out`; returns (compressed digest, compressed size).
///
/// The rootfs path is passed as an ARGV element to `tar` (not interpolated into a shell string), so a
/// path containing an apostrophe or other shell metacharacter packages correctly instead of failing with
/// "Unterminated quoted string". The pipe to `gzip` is wired via `Command` stdio rather than a shell
/// string. `--sparse` makes `tar` detect and compactly store holes so a sparse file in the rootfs is not
/// expanded to its full logical size on the way through the layer blob.
/// The `tar` argv that packs `rootfs` to stdout for the layer blob. `--sparse` so holes in a sparse file
/// are stored compactly (not expanded to full logical size); the rootfs path is an argv element (never a
/// shell string) so apostrophes/metacharacters in it are safe.
fn tar_pack_argv(rootfs: &Path) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    vec![
        OsString::from("--sparse"),
        OsString::from("-cf"),
        OsString::from("-"),
        OsString::from("-C"),
        rootfs.as_os_str().to_os_string(),
        OsString::from("."),
    ]
}

pub(super) fn tar_gzip(rootfs: &Path, out: &Path) -> Result<(String, u64), Error> {
    use std::process::Stdio;
    let outfile = std::fs::File::create(out).map_err(|e| Error::Archive(e.to_string()))?;
    let mut tar = Command::new("tar")
        .args(tar_pack_argv(rootfs))
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Archive(format!("tar: {e}")))?;
    let tar_stdout = tar
        .stdout
        .take()
        .ok_or_else(|| Error::Archive("tar stdout unavailable".to_string()))?;
    let gzip = Command::new("gzip")
        .arg("-n")
        .stdin(tar_stdout)
        .stdout(outfile)
        .spawn()
        .map_err(|e| Error::Archive(format!("gzip: {e}")))?;
    let tar_status = tar.wait().map_err(|e| Error::Archive(e.to_string()))?;
    let gzip_out = gzip
        .wait_with_output()
        .map_err(|e| Error::Archive(e.to_string()))?;
    if !tar_status.success() {
        return Err(Error::Archive(format!("tar exited with {tar_status}")));
    }
    if !gzip_out.status.success() {
        return Err(Error::Archive(format!(
            "gzip exited with {}: {}",
            gzip_out.status,
            String::from_utf8_lossy(&gzip_out.stderr).trim()
        )));
    }
    let size = std::fs::metadata(out)
        .map_err(|e| Error::Archive(e.to_string()))?
        .len();
    Ok((crate::image::digest::sha256_file(out)?, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A unique scratch dir removed on drop (temp_dir + RAII idiom; no tempfile dep).
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "hl_layer_wh_test_{}_{}_{}",
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
    fn whiteout_deletes_sibling_and_marker_keeps_others() {
        let t = Tmp::new("basic");
        let root = &t.0;
        std::fs::write(root.join("keep"), b"k").unwrap();
        std::fs::write(root.join("remove"), b"r").unwrap();
        std::fs::write(root.join(".wh.remove"), b"").unwrap();

        apply_whiteouts(root).unwrap();

        // the marked sibling AND the marker itself are gone; the unrelated file survives.
        assert!(root.join("keep").exists());
        assert!(!root.join("remove").exists());
        assert!(!root.join(".wh.remove").exists());
    }

    #[test]
    fn whiteout_of_directory_removes_whole_subtree() {
        let t = Tmp::new("dir");
        let root = &t.0;
        std::fs::create_dir_all(root.join("d").join("sub")).unwrap();
        std::fs::write(root.join("d").join("sub").join("f"), b"x").unwrap();
        std::fs::write(root.join(".wh.d"), b"").unwrap();

        apply_whiteouts(root).unwrap();

        assert!(!root.join("d").exists(), "directory subtree should be removed");
        assert!(!root.join(".wh.d").exists());
    }

    #[test]
    fn bare_wh_prefix_marker_does_not_delete_parent() {
        // Regression guard: a malformed marker that is ONLY the `.wh.` prefix (empty target) must be
        // dropped WITHOUT deleting its parent directory or any sibling (the old shell pipeline ran
        // `rm -rf "$dir/"` here, wiping the parent).
        let t = Tmp::new("baremarker");
        let root = &t.0;
        let dir = root.join("layerdir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("safe"), b"s").unwrap();
        std::fs::write(dir.join(".wh."), b"").unwrap();

        apply_whiteouts(root).unwrap();

        // the parent dir and its real content survive; only the malformed marker is removed.
        assert!(dir.exists(), "parent dir must NOT be wiped by a bare .wh. marker");
        assert!(dir.join("safe").exists(), "sibling must survive");
        assert!(!dir.join(".wh.").exists(), "the malformed marker is still cleaned up");
    }

    #[test]
    fn opaque_marker_is_dropped_without_deleting_siblings() {
        // `.wh..wh..opq` has no sibling to delete (layers are already flattened): the marker is removed
        // but adjacent files are left intact.
        let t = Tmp::new("opaque");
        let root = &t.0;
        std::fs::write(root.join("data"), b"d").unwrap();
        std::fs::write(root.join(".wh..wh..opq"), b"").unwrap();

        apply_whiteouts(root).unwrap();

        assert!(root.join("data").exists(), "opaque marker must not delete siblings");
        assert!(!root.join(".wh..wh..opq").exists(), "opaque marker itself is removed");
    }

    // Finding 3: the layer-packing tar argv must carry `--sparse` (so a sparse file is not expanded to
    // its full logical size through the layer blob).
    #[test]
    fn tar_pack_argv_has_sparse_flag() {
        let argv = tar_pack_argv(Path::new("/some/rootfs"));
        assert!(
            argv.iter().any(|a| a == "--sparse"),
            "layer tar must pass --sparse, got {argv:?}"
        );
        // rootfs is a single argv element (not embedded in a shell string), so metacharacters are safe.
        assert!(argv.iter().any(|a| a == std::ffi::OsStr::new("/some/rootfs")));
    }

    // Finding 2: a rootfs whose PATH contains an apostrophe must package without "Unterminated quoted
    // string" — paths go to tar as argv, not a single-quoted shell string.
    #[test]
    fn tar_gzip_handles_apostrophe_in_rootfs_path() {
        let t = Tmp::new("apos");
        let rootfs = t.0.join("O'Brien's rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::write(rootfs.join("file.txt"), b"hello\n").unwrap();
        let out = t.0.join("layer.tar.gz");
        let (digest, size) = tar_gzip(&rootfs, &out).expect("apostrophe rootfs must package");
        assert!(digest.starts_with("sha256:") && digest.len() == 71, "got {digest:?}");
        assert!(size > 0);
        // it is a real gzip that round-trips back to the file.
        let back = t.0.join("back");
        std::fs::create_dir_all(&back).unwrap();
        let st = Command::new("tar").arg("-xzf").arg(&out).arg("-C").arg(&back).status().unwrap();
        assert!(st.success());
        assert_eq!(std::fs::read(back.join("file.txt")).unwrap(), b"hello\n");
    }

    // Finding 4: an opaque marker whose parent path escapes the rootfs via `..` must NOT delete anything
    // outside the rootfs.
    #[test]
    fn opaque_dir_escaping_rootfs_is_not_cleared() {
        let t = Tmp::new("opq-escape");
        let rootfs = t.0.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        // a sibling of the rootfs, OUTSIDE it, that must survive.
        let outside = t.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"keep me").unwrap();

        // a malicious opaque dir pointing above the rootfs.
        clear_opaque_dirs(&rootfs, &["../outside".to_string()]);

        assert!(outside.join("secret").exists(), "opaque `..` escape must not delete outside the rootfs");
        assert!(outside.exists());
    }

    // contained_relative is the lexical containment guard: normal paths pass, escapes reject.
    #[test]
    fn contained_relative_normalizes_and_rejects_escapes() {
        assert_eq!(contained_relative("app"), Some("app".to_string()));
        assert_eq!(contained_relative("a/b/../c"), Some("a/c".to_string()));
        assert_eq!(contained_relative("./a/./b"), Some("a/b".to_string()));
        assert_eq!(contained_relative(""), Some(String::new()));
        assert_eq!(contained_relative("a/.."), Some(String::new()));
        // escapes reject
        assert_eq!(contained_relative("../outside"), None);
        assert_eq!(contained_relative("a/../.."), None);
        assert_eq!(contained_relative("/abs"), None);
    }

    #[test]
    fn whiteouts_apply_recursively_in_subdirectories() {
        // A marker nested in a subdirectory deletes the sibling in THAT directory (the walk recurses).
        let t = Tmp::new("recurse");
        let root = &t.0;
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("gone"), b"g").unwrap();
        std::fs::write(sub.join("stay"), b"s").unwrap();
        std::fs::write(sub.join(".wh.gone"), b"").unwrap();

        apply_whiteouts(root).unwrap();

        assert!(!sub.join("gone").exists());
        assert!(sub.join("stay").exists());
        assert!(!sub.join(".wh.gone").exists());
    }
}
