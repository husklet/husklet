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
use std::collections::HashMap;
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
/// digest helper (`crate::image::digest::sha256_hex`); kept here as the public `dd_images::build`
/// surface external callers (the daemon's build handler/prune) already import.
pub fn sha256_hex(data: &[u8]) -> String {
    crate::image::digest::sha256_hex(data)
}

/// A deterministic content digest of an assembled rootfs: hash of a sorted
/// (type,mode,nlink,symlink-target,size,path) listing combined with the sha256 of every regular file's
/// contents. Including `%m` (mode) means a permission change (e.g. `0644 -> 0755`) changes the digest, so
/// image/cache identity tracks the executable bit and other mode/metadata; `%n`/`%l` fold in hardlink
/// count and symlink target so topology/target changes are not aliased. Same tree -> same hash,
/// independent of filesystem iteration order. Returns "" on failure.
pub fn rootfs_digest(rootfs: &Path) -> String {
    let script = format!(
        "cd '{}' 2>/dev/null || exit 0; \
         {{ find . -printf '%y %m %n %l %s %p\\n' 2>/dev/null | LC_ALL=C sort; \
            find . -type f -print0 2>/dev/null | LC_ALL=C sort -z | xargs -0 sha256sum 2>/dev/null; \
         }} | sha256sum",
        rootfs.display());
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string(),
        Err(_) => String::new(),
    }
}

/// Deterministic content+metadata digest of a file or directory subtree at `p` (absolute host path):
/// type, mode, hardlink count (`%n`), symlink target (`%l`) and size of every entry plus the sha256 of
/// each regular file's contents, sorted so it is independent of fs iteration order. Including `%l` means a
/// changed symlink target (`link -> aa` vs `link -> bb`) changes the digest; including `%n`/hardlink-count
/// distinguishes a hardlinked tree from independent files with identical bytes, so a `COPY`/`ADD` cache
/// key does not alias those. Used to make COPY/ADD cache keys content-addressed. Returns "" on failure
/// (the caller then forces a miss rather than risk serving a stale layer).
pub fn path_digest(p: &Path) -> String {
    let script = format!(
        "p='{}'; if [ -d \"$p\" ]; then cd \"$p\" 2>/dev/null || exit 0; \
            {{ find . -printf '%y %m %n %l %s %p\\n' 2>/dev/null | LC_ALL=C sort; \
               find . -type f -print0 2>/dev/null | LC_ALL=C sort -z | xargs -0 sha256sum 2>/dev/null; }} | sha256sum; \
         elif [ -h \"$p\" ]; then {{ stat -c '%F %a' \"$p\" 2>/dev/null; readlink \"$p\" 2>/dev/null; }} | sha256sum; \
         elif [ -e \"$p\" ]; then {{ stat -c '%F %a %h %s' \"$p\" 2>/dev/null; sha256sum \"$p\" 2>/dev/null; }} | sha256sum; \
         else echo missing; fi",
        p.display());
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string(),
        Err(_) => String::new(),
    }
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

    // --- content+metadata digest regression tests (shell out to find/stat/sha256sum) ---

    fn scratch(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir()
            .join(format!("dd-cache-test-{}-{}-{}", label, std::process::id(), nanos));
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
/// the caller's buildcache directory ([`BuildCache::new`]); the daemon passes `~/.dd/buildcache`.
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
