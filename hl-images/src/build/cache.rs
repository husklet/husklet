//! The `docker build` layer cache — a conservative, content-addressed reimplementation of Docker's
//! classic build cache, lifted out of the daemon so it is runtime-agnostic (it snapshots + restores
//! rootfs trees and stores step metadata; it never runs a build step). Each step gets a `cache id` =
//! sha256(parent step's cache id + a normalized descriptor of the instruction). For COPY/ADD the
//! descriptor folds in a content+metadata digest of the source files, so changed context invalidates;
//! for everything else it is the (ARG-substituted) instruction text. The rootfs produced AFTER a step is
//! snapshotted under `<buildcache>/layers/<cache-id>/rootfs` (filesystem-mutating steps only) alongside a
//! meta.json capturing the cumulative image config, so a future rebuild can REUSE the snapshot+config
//! instead of re-running. CORRECTNESS RULE: a hit replays the exact rootfs a prior run of the identical
//! (parent+instruction[+context]) step recorded — bit-identical to that run; anything we cannot prove
//! identical misses and re-runs.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A deterministic non-crypto fallback id (FNV-1a, widened) matching the daemon's, used only when the
/// `sha256sum` CLI is unavailable so cache ids still key deterministically (never empty/colliding).
fn fake_id(seed: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}{h:016x}{h:016x}{h:08x}")
}

/// Current unix time in whole seconds (0 before the epoch / on a clock error) — the `created` stamp on a
/// stored cache layer's metadata. Informational only; never folded into a cache key.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// sha256 (lowercase hex, no `sha256:` prefix) of arbitrary bytes. Thin re-export of the crate's single
/// digest helper (`crate::image::digest::sha256_hex`); kept here as the public `hl_images::build`
/// surface external callers (the daemon's build handler/prune) already import.
pub fn sha256_hex(data: &[u8]) -> String {
    crate::image::digest::sha256_hex(data)
}

/// A deterministic content digest of an assembled rootfs: hash of a sorted
/// (type,mode,nlink,size,symlink-target,path) listing combined with the sha256 of every regular file's
/// contents. Including the mode means a permission change (e.g. `0644 -> 0755`) changes the digest, so
/// image/cache identity tracks the executable bit and other mode/metadata; nlink/symlink-target fold in
/// hardlink count and symlink target so topology/target changes are not aliased. Same tree -> same hash,
/// independent of filesystem iteration order. Returns "" on failure.
///
/// Computed by a native in-process walk (see [`dir_digest`]) — the former `sh -c` find/sort/sha256sum
/// pipeline is gone, removing the crate's last shell-injection surface. The serialization is our own
/// canonical form and is NOT byte-compatible with the old GNU-tool output, so existing cache keys rotate
/// once on rollout (a one-time miss + re-run, allowed by the module's CORRECTNESS RULE); it is
/// self-consistent thereafter.
pub fn rootfs_digest(rootfs: &Path) -> String {
    dir_digest(rootfs).unwrap_or_default()
}

/// Deterministic content+metadata digest of a file or directory subtree at `p` (absolute host path):
/// type, mode, hardlink count, symlink target and size of every entry plus the sha256 of each regular
/// file's contents, sorted so it is independent of fs iteration order. A changed symlink target
/// (`link -> aa` vs `link -> bb`) changes the digest; folding in hardlink count distinguishes a
/// hardlinked tree from independent files with identical bytes, so a `COPY`/`ADD` cache key does not
/// alias those. Used to make COPY/ADD cache keys content-addressed. Returns "" on failure or a
/// missing/unreadable path (the caller then forces a miss rather than risk serving a stale layer).
///
/// Native in-process implementation (no `sh -c`/find/stat/sha256sum subprocess); the canonical
/// serialization differs from the old GNU-tool output, so keys rotate once on rollout — see
/// [`rootfs_digest`].
pub fn path_digest(p: &Path) -> String {
    let Ok(md) = std::fs::symlink_metadata(p) else {
        return String::new(); // missing/unreadable -> force a cache miss
    };
    let ft = md.file_type();
    if ft.is_dir() {
        return dir_digest(p).unwrap_or_default();
    }
    // A single file / symlink / special node: hash a small canonical descriptor of just this entry.
    let mut h = Sha256::new();
    if ft.is_symlink() {
        let tgt = std::fs::read_link(p)
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_default();
        h.update(format!("l {:o} {tgt}", md.mode() & 0o7777).as_bytes());
    } else if ft.is_file() {
        h.update(format!("f {:o} {} {}", md.mode() & 0o7777, md.nlink(), md.len()).as_bytes());
        if let Some(hexd) = file_sha256(p) {
            h.update(b" ");
            h.update(hexd.as_bytes());
        }
    } else {
        // char/block/fifo/socket special — fold in rdev so a different device node digests differently.
        h.update(
            format!(
                "s {:o} {} rdev={}",
                md.mode() & 0o7777,
                md.nlink(),
                md.rdev()
            )
            .as_bytes(),
        );
    }
    hex_lower(&h.finalize())
}

/// The shared directory-subtree digester behind [`rootfs_digest`] and [`path_digest`]'s dir branch: one
/// hand-rolled recursive walk that records a canonical metadata line per entry plus the sha256 of each
/// regular file's contents, sorts both listings (so the result is independent of fs iteration order), and
/// hashes the concatenation. Symlinks are recorded (type + target) but NOT followed, so a malicious
/// symlink in the tree can't redirect the walk elsewhere. Returns `None` only if `root` itself can't be
/// read as a directory (caller maps that to "" -> forced cache miss).
fn dir_digest(root: &Path) -> Option<String> {
    // `root` must be a readable directory; otherwise there is nothing to digest deterministically.
    if !std::fs::symlink_metadata(root).ok()?.is_dir() {
        return None;
    }
    let mut meta_lines: Vec<String> = Vec::new();
    let mut file_lines: Vec<String> = Vec::new();
    record_entry(root, root, &mut meta_lines, &mut file_lines);
    meta_lines.sort();
    file_lines.sort();
    let mut h = Sha256::new();
    for l in &meta_lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    h.update(b"--\n"); // separate the metadata section from the content-hash section unambiguously
    for l in &file_lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    Some(hex_lower(&h.finalize()))
}

/// Record `path`'s canonical metadata line (and, for a regular file, its content sha256), recursing into
/// real subdirectories only. `root` anchors the rootfs-relative path so the listing is location-stable.
fn record_entry(root: &Path, path: &Path, meta: &mut Vec<String>, files: &mut Vec<String>) {
    let Ok(md) = std::fs::symlink_metadata(path) else {
        return;
    };
    let ft = md.file_type();
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    let mode = md.mode() & 0o7777;
    if ft.is_dir() {
        meta.push(format!("d {mode:o} {} {} {rel}", md.nlink(), md.len()));
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                record_entry(root, &e.path(), meta, files);
            }
        }
    } else if ft.is_symlink() {
        let tgt = std::fs::read_link(path)
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_default();
        meta.push(format!("l {mode:o} {} {rel} -> {tgt}", md.nlink()));
    } else if ft.is_file() {
        meta.push(format!("f {mode:o} {} {} {rel}", md.nlink(), md.len()));
        if let Some(hexd) = file_sha256(path) {
            files.push(format!("{hexd}  {rel}"));
        }
    } else {
        meta.push(format!("s {mode:o} {} rdev={} {rel}", md.nlink(), md.rdev()));
    }
}

/// Streaming lowercase-hex sha256 of a file's contents (no `sha256:` prefix); `None` if it can't be read.
fn file_sha256(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(hex_lower(&h.finalize()))
}

/// Lowercase-hex encode of raw digest bytes (32 bytes -> 64 chars).
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}

/// Chain hash for a step's cache id: sha256(parent + descriptor), falling back to a stable non-crypto id
/// if `sha256sum` is unavailable so the cache still keys deterministically (never an empty/colliding id).
pub fn cache_id(parent: &str, descriptor: &str) -> String {
    let seed = format!("{parent}\n{descriptor}");
    let h = sha256_hex(seed.as_bytes());
    if h.len() == 64 {
        h
    } else {
        fake_id(&seed)
    }
}

/// Instructions that mutate the rootfs (so their cache layer needs a full snapshot). Everything else is
/// config-only (ENV/CMD/ENTRYPOINT/LABEL/EXPOSE/USER/...) and stores just metadata.
pub fn is_fs_inst(inst: &str) -> bool {
    matches!(inst, "RUN" | "COPY" | "ADD" | "WORKDIR")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- content+metadata digest regression tests (native in-process walk) ---

    fn scratch(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir()
            .join(format!("hl-cache-test-{}-{}-{}", label, std::process::id(), nanos));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // rootfs_digest MUST fold in file mode: flipping a file `0644 -> 0755` (a runtime-behavior change)
    // must change the digest so cache/image identity does not reuse a stale layer with different bits.
    #[test]
    fn rootfs_digest_changes_when_file_mode_changes() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("rootfs-mode");
        let f = root.join("tool");
        std::fs::write(&f, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        let before = rootfs_digest(&root);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let after = rootfs_digest(&root);
        assert_eq!(before.len(), 64, "digest is a sha256 hex string");
        assert_ne!(before, after, "0644 -> 0755 must change the rootfs digest");
        let _ = std::fs::remove_dir_all(&root);
    }

    // path_digest MUST fold in symlink target: two symlinks with the same path (and same target length)
    // but different targets must hash differently, else a rebuild can reuse a layer with the old target.
    #[test]
    fn path_digest_changes_when_symlink_target_changes() {
        use std::os::unix::fs::symlink;
        let a = scratch("sym-a");
        let b = scratch("sym-b");
        symlink("aa", a.join("link")).unwrap();
        symlink("bb", b.join("link")).unwrap();
        let da = path_digest(&a);
        let db = path_digest(&b);
        assert_eq!(da.len(), 64);
        assert_ne!(da, db, "link -> aa and link -> bb must produce distinct digests");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    // path_digest MUST fold in hardlink topology: a two-name hardlink tree and two independent files with
    // identical bytes must hash differently, else `cp -a` (which preserves hardlinks) can produce a
    // different filesystem than the cache key claims.
    #[test]
    fn path_digest_changes_when_hardlink_topology_changes() {
        let linked = scratch("hl-linked");
        let indep = scratch("hl-indep");
        std::fs::write(linked.join("a"), b"same bytes\n").unwrap();
        std::fs::hard_link(linked.join("a"), linked.join("b")).unwrap();
        std::fs::write(indep.join("a"), b"same bytes\n").unwrap();
        std::fs::write(indep.join("b"), b"same bytes\n").unwrap();
        let dl = path_digest(&linked);
        let di = path_digest(&indep);
        assert_eq!(dl.len(), 64);
        assert_ne!(dl, di, "hardlinked tree must digest differently from independent files");
        let _ = std::fs::remove_dir_all(&linked);
        let _ = std::fs::remove_dir_all(&indep);
    }

    // A path containing a single quote (apostrophe) must still produce a valid 64-hex digest: paths are
    // passed as argv, never interpolated into a single-quoted shell string (which the apostrophe broke,
    // yielding an empty/wrong digest).
    #[test]
    fn rootfs_digest_handles_apostrophe_in_path() {
        let base = scratch("rootfs-apos");
        let root = base.join("O'Brien's rootfs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file"), b"data\n").unwrap();
        let d = rootfs_digest(&root);
        assert_eq!(d.len(), 64, "apostrophe path must still yield a sha256 hex digest, got {d:?}");
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn path_digest_handles_apostrophe_in_path() {
        let base = scratch("path-apos");
        let dir = base.join("can't stop");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"x\n").unwrap();
        let d = path_digest(&dir);
        assert_eq!(d.len(), 64, "apostrophe path must still yield a sha256 hex digest, got {d:?}");
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        // a distinct content under an apostrophe path digests differently (proves it hashed real content,
        // not an empty/short-circuited digest).
        let dir2 = base.join("won't stop");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join("f"), b"y\n").unwrap();
        assert_ne!(d, path_digest(&dir2));
        let _ = std::fs::remove_dir_all(&base);
    }

    // The digest is deterministic and independent of filesystem creation/iteration order: two trees with
    // the same entries built in a DIFFERENT order must produce the SAME digest, and a content change must
    // change it. Guards the native walker's canonical (sorted) serialization.
    #[test]
    fn rootfs_digest_is_order_independent_and_content_sensitive() {
        let a = scratch("det-a");
        let b = scratch("det-b");
        // Build `a` in one order.
        std::fs::create_dir_all(a.join("d1")).unwrap();
        std::fs::write(a.join("d1").join("x"), b"hello\n").unwrap();
        std::fs::write(a.join("top"), b"root file\n").unwrap();
        // Build `b` with identical content but a different creation order.
        std::fs::write(b.join("top"), b"root file\n").unwrap();
        std::fs::create_dir_all(b.join("d1")).unwrap();
        std::fs::write(b.join("d1").join("x"), b"hello\n").unwrap();

        let da = rootfs_digest(&a);
        let db = rootfs_digest(&b);
        assert_eq!(da.len(), 64);
        assert!(da.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(da, db, "identical trees must digest identically regardless of build order");
        // The same call is stable across invocations.
        assert_eq!(da, rootfs_digest(&a), "digest must be stable across calls");
        // Changing one file's contents changes the digest.
        std::fs::write(b.join("d1").join("x"), b"HELLO\n").unwrap();
        assert_ne!(da, rootfs_digest(&b), "a content change must change the digest");
        // A missing/unreadable path digests to "" so the caller forces a miss.
        assert_eq!(path_digest(&a.join("does-not-exist")), "");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn fs_instructions_true() {
        for i in ["RUN", "COPY", "ADD", "WORKDIR"] {
            assert!(is_fs_inst(i), "{i} should be an fs instruction");
        }
    }
    #[test]
    fn config_instructions_false() {
        for i in ["ENV", "CMD", "LABEL", "EXPOSE", "USER", "FROM", "ENTRYPOINT"] {
            assert!(!is_fs_inst(i), "{i} should NOT be an fs instruction");
        }
        // match is exact/case-sensitive.
        assert!(!is_fs_inst("run"));
        assert!(!is_fs_inst(""));
    }

    #[test]
    fn cache_id_is_deterministic() {
        let a = cache_id("parent-a", "RUN echo hi");
        let b = cache_id("parent-a", "RUN echo hi");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
    #[test]
    fn cache_id_diverges_on_parent() {
        assert_ne!(
            cache_id("parent-a", "RUN echo hi"),
            cache_id("parent-b", "RUN echo hi")
        );
    }
    #[test]
    fn cache_id_diverges_on_descriptor() {
        assert_ne!(
            cache_id("parent-a", "RUN echo hi"),
            cache_id("parent-a", "RUN echo bye")
        );
    }
}

/// The build layer cache rooted at `<buildcache>/layers`: it snapshots the rootfs a step produced and
/// restores it on a later hit, and records/serves each step's cumulative image config. Construct it with
/// the caller's buildcache directory ([`BuildCache::new`]); the daemon passes `~/.hl/buildcache`.
#[derive(Clone, Debug)]
pub struct BuildCache {
    layers_dir: PathBuf,
}

impl BuildCache {
    /// A cache whose layers live under `<buildcache_dir>/layers`.
    pub fn new(buildcache_dir: PathBuf) -> Self {
        BuildCache {
            layers_dir: buildcache_dir.join("layers"),
        }
    }

    fn layer_dir(&self, cache_id: &str) -> PathBuf {
        self.layers_dir.join(cache_id)
    }

    /// Load a cache layer's metadata iff it is present AND complete (an fs layer's rootfs snapshot must
    /// exist). Returns None on any miss so a partial/corrupt layer is never served as a hit.
    pub fn load_layer(&self, id: &str) -> Option<Value> {
        let dir = self.layer_dir(id);
        let meta: Value = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        if meta.get("fs").and_then(|v| v.as_bool()).unwrap_or(false) && !dir.join("rootfs").is_dir() {
            return None;
        }
        Some(meta)
    }

    /// Materialize a cached fs layer's rootfs snapshot into `dst` (the live stage rootfs), replacing it.
    /// Returns false on failure — the caller aborts the build rather than continue on a wrong rootfs.
    pub fn materialize(&self, id: &str, dst: &Path) -> bool {
        let src = self.layer_dir(id).join("rootfs");
        if !src.is_dir() {
            return false;
        }
        let _ = std::fs::remove_dir_all(dst);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        matches!(std::process::Command::new("cp").arg("-a").arg(&src).arg(dst).status(), Ok(s) if s.success())
    }

    /// Persist a freshly executed step as a cache layer: a full rootfs snapshot for filesystem-mutating
    /// instructions, plus a meta.json sidecar capturing the cumulative image config so a future hit can
    /// restore it without re-running. Atomic & best-effort: the snapshot is written first and meta.json
    /// LAST, so a layer only becomes loadable once complete; a failed snapshot leaves no (false-hit)
    /// layer behind.
    #[allow(clippy::too_many_arguments)]
    pub fn store_layer(
        &self,
        id: &str,
        parent: &str,
        inst: &str,
        args: &str,
        rootfs: &Path,
        cmd: &[String],
        entrypoint: &[String],
        workdir: &str,
        env: &[String],
        labels: &HashMap<String, String>,
    ) {
        let dir = self.layer_dir(id);
        let _ = std::fs::remove_dir_all(&dir);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let fs = is_fs_inst(inst);
        if fs {
            let lr = dir.join("rootfs");
            if !matches!(std::process::Command::new("cp").arg("-a").arg(rootfs).arg(&lr).status(), Ok(s) if s.success())
            {
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
        }
        let meta = json!({"v": 1, "parent": parent, "inst": inst, "args": args, "fs": fs,
            "created": now_secs(), "cmd": cmd, "entrypoint": entrypoint, "workdir": workdir,
            "env": env, "labels": labels});
        let tmp = dir.join(".meta.json.tmp");
        if std::fs::write(&tmp, meta.to_string()).is_ok()
            && std::fs::rename(&tmp, dir.join("meta.json")).is_ok()
        {
            return;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
