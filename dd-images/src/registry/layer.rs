//! Rootfs / whiteout / tar / gzip / sha256 tools used while unpacking and building layers.

use super::*;
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
        let target = if d.is_empty() {
            rootfs.to_path_buf()
        } else {
            rootfs.join(d)
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

/// Remove a path whether it's a file, a symlink, or a directory subtree; missing is success.
fn remove_path(p: &Path) {
    let _ = match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(_) => Ok(()),
    };
}

/// `tar | gzip` a rootfs into `out`; returns (compressed digest, compressed size).
pub(super) fn tar_gzip(rootfs: &Path, out: &Path) -> Result<(String, u64), Error> {
    let cmd = format!(
        "tar cf - -C '{}' . | gzip -n > '{}'",
        rootfs.display(),
        out.display()
    );
    run("sh", &["-c", &cmd])?;
    let size = std::fs::metadata(out)
        .map_err(|e| Error::Archive(e.to_string()))?
        .len();
    Ok((crate::image::digest::sha256_file(out)?, size))
}
