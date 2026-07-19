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

use crate::{Error, Sha256Digest};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::{self, Display, Formatter};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Current unix time in whole seconds (0 before the epoch / on a clock error) — the `created` stamp on a
/// stored cache layer's metadata. Informational only; never folded into a cache key.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Build-cache content and metadata digests.
impl Sha256Digest {
    /// Hashes one filesystem entry or tree, including runtime-relevant metadata.
    pub fn build_path(path: &Path) -> Result<Self, Error> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| Error::Digest(format!("inspect {}: {error}", path.display())))?;
        let kind = metadata.file_type();
        if kind.is_dir() {
            return TreeDigest::read(path);
        }

        let mut hasher = Sha256::new();
        if kind.is_symlink() {
            let target = std::fs::read_link(path)
                .map_err(|error| Error::Digest(format!("read link {}: {error}", path.display())))?
                .to_string_lossy()
                .into_owned();
            hasher.update(format!("l {:o} {target}", metadata.mode() & 0o7777).as_bytes());
        } else if kind.is_file() {
            hasher.update(
                format!(
                    "f {:o} {} {}",
                    metadata.mode() & 0o7777,
                    metadata.nlink(),
                    metadata.len()
                )
                .as_bytes(),
            );
            let digest = Sha256Digest::file(path)?;
            hasher.update(b" ");
            hasher.update(digest.to_string().as_bytes());
        } else {
            hasher.update(
                format!(
                    "s {:o} {} rdev={}",
                    metadata.mode() & 0o7777,
                    metadata.nlink(),
                    metadata.rdev()
                )
                .as_bytes(),
            );
        }
        Ok(Sha256Digest::from_hasher(hasher))
    }
}

/// Stateful canonical traversal behind [`Sha256Digest::build_path`].
struct TreeDigest<'a> {
    root: &'a Path,
    metadata: Vec<String>,
    files: Vec<String>,
}

impl<'a> TreeDigest<'a> {
    fn read(root: &'a Path) -> Result<Sha256Digest, Error> {
        let metadata = std::fs::symlink_metadata(root)
            .map_err(|error| Error::Digest(format!("inspect {}: {error}", root.display())))?;
        if !metadata.is_dir() {
            return Err(Error::Digest(format!(
                "digest tree is not a directory: {}",
                root.display()
            )));
        }

        let mut tree = Self {
            root,
            metadata: Vec::new(),
            files: Vec::new(),
        };
        tree.scan(root)?;
        tree.metadata.sort();
        tree.files.sort();

        let mut hasher = Sha256::new();
        for line in &tree.metadata {
            hasher.update(line.as_bytes());
            hasher.update(b"\n");
        }
        hasher.update(b"--\n");
        for line in &tree.files {
            hasher.update(line.as_bytes());
            hasher.update(b"\n");
        }
        Ok(Sha256Digest::from_hasher(hasher))
    }

    /// Records one entry and descends only into real directories, never through symlinks.
    fn scan(&mut self, path: &Path) -> Result<(), Error> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| Error::Digest(format!("inspect {}: {error}", path.display())))?;
        let kind = metadata.file_type();
        let relative = path
            .strip_prefix(self.root)
            .unwrap_or(path)
            .to_string_lossy();
        let mode = metadata.mode() & 0o7777;

        if kind.is_dir() {
            self.metadata.push(format!(
                "d {mode:o} {} {} {relative}",
                metadata.nlink(),
                metadata.len()
            ));
            let entries = std::fs::read_dir(path)
                .map_err(|error| Error::Digest(format!("read {}: {error}", path.display())))?;
            for entry in entries {
                let entry = entry
                    .map_err(|error| Error::Digest(format!("read {}: {error}", path.display())))?;
                self.scan(&entry.path())?;
            }
        } else if kind.is_symlink() {
            let target = std::fs::read_link(path)
                .map_err(|error| Error::Digest(format!("read link {}: {error}", path.display())))?
                .to_string_lossy()
                .into_owned();
            self.metadata.push(format!(
                "l {mode:o} {} {relative} -> {target}",
                metadata.nlink()
            ));
        } else if kind.is_file() {
            self.metadata.push(format!(
                "f {mode:o} {} {} {relative}",
                metadata.nlink(),
                metadata.len()
            ));
            let digest = Sha256Digest::file(path)?;
            self.files.push(format!("{digest}  {relative}"));
        } else {
            self.metadata.push(format!(
                "s {mode:o} {} rdev={} {relative}",
                metadata.nlink(),
                metadata.rdev()
            ));
        }
        Ok(())
    }
}

/// Stable identity of one step in a parent-linked build-cache chain.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CacheId(Sha256Digest);

impl CacheId {
    /// Derives a step identity from its parent identity and normalized descriptor.
    pub fn from_step(parent: Option<&Self>, descriptor: &str) -> Self {
        let parent = parent.map(ToString::to_string).unwrap_or_default();
        Self(Sha256Digest::from_bytes(
            format!("{parent}\n{descriptor}").as_bytes(),
        ))
    }
}

impl Display for CacheId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

/// Instructions that mutate the rootfs (so their cache layer needs a full snapshot). Everything else is
/// config-only (ENV/CMD/ENTRYPOINT/LABEL/EXPOSE/USER/...) and stores just metadata.
#[cfg(test)]
mod tests {
    use super::*;

    // --- content+metadata digest regression tests (native in-process walk) ---

    fn scratch(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!(
            "hl-cache-test-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
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
        let before = Sha256Digest::build_path(&root).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let after = Sha256Digest::build_path(&root).unwrap();
        assert_eq!(
            before.to_string().len(),
            64,
            "digest is a sha256 hex string"
        );
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
        let da = Sha256Digest::build_path(&a).unwrap();
        let db = Sha256Digest::build_path(&b).unwrap();
        assert_eq!(da.to_string().len(), 64);
        assert_ne!(
            da, db,
            "link -> aa and link -> bb must produce distinct digests"
        );
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
        let dl = Sha256Digest::build_path(&linked).unwrap();
        let di = Sha256Digest::build_path(&indep).unwrap();
        assert_eq!(dl.to_string().len(), 64);
        assert_ne!(
            dl, di,
            "hardlinked tree must digest differently from independent files"
        );
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
        let d = Sha256Digest::build_path(&root).unwrap().to_string();
        assert_eq!(
            d.len(),
            64,
            "apostrophe path must still yield a sha256 hex digest, got {d:?}"
        );
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn path_digest_handles_apostrophe_in_path() {
        let base = scratch("path-apos");
        let dir = base.join("can't stop");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), b"x\n").unwrap();
        let d = Sha256Digest::build_path(&dir).unwrap().to_string();
        assert_eq!(
            d.len(),
            64,
            "apostrophe path must still yield a sha256 hex digest, got {d:?}"
        );
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        // a distinct content under an apostrophe path digests differently (proves it hashed real content,
        // not an empty/short-circuited digest).
        let dir2 = base.join("won't stop");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join("f"), b"y\n").unwrap();
        assert_ne!(d, Sha256Digest::build_path(&dir2).unwrap().to_string());
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

        let da = Sha256Digest::build_path(&a).unwrap();
        let db = Sha256Digest::build_path(&b).unwrap();
        assert_eq!(da.to_string().len(), 64);
        assert!(da.to_string().chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            da, db,
            "identical trees must digest identically regardless of build order"
        );
        // The same call is stable across invocations.
        assert_eq!(
            da,
            Sha256Digest::build_path(&a).unwrap(),
            "digest must be stable across calls"
        );
        // Changing one file's contents changes the digest.
        std::fs::write(b.join("d1").join("x"), b"HELLO\n").unwrap();
        assert_ne!(
            da,
            Sha256Digest::build_path(&b).unwrap(),
            "a content change must change the digest"
        );
        // A missing/unreadable path digests to "" so the caller forces a miss.
        assert!(Sha256Digest::build_path(&a.join("does-not-exist")).is_err());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn fs_instructions_true() {
        for i in ["RUN", "COPY", "ADD", "WORKDIR"] {
            let instruction = crate::build::Instruction {
                name: i.into(),
                arguments: String::new(),
            };
            assert!(
                instruction.mutates_filesystem(),
                "{i} should be an fs instruction"
            );
        }
    }
    #[test]
    fn config_instructions_false() {
        for i in [
            "ENV",
            "CMD",
            "LABEL",
            "EXPOSE",
            "USER",
            "FROM",
            "ENTRYPOINT",
        ] {
            let instruction = crate::build::Instruction {
                name: i.into(),
                arguments: String::new(),
            };
            assert!(
                !instruction.mutates_filesystem(),
                "{i} should NOT be an fs instruction"
            );
        }
        // match is exact/case-sensitive.
        assert!(!crate::build::Instruction {
            name: "run".into(),
            arguments: String::new()
        }
        .mutates_filesystem());
        assert!(!crate::build::Instruction {
            name: String::new(),
            arguments: String::new()
        }
        .mutates_filesystem());
    }

    #[test]
    fn cache_id_is_deterministic() {
        let root = CacheId::from_step(None, "RUN echo hi");
        assert_eq!(
            root.to_string(),
            "dfc02a1c1ecdab159ee1d69e02af9dc1eaf5e2e693507a094503c9842779f9bb"
        );

        let parent = CacheId::from_step(None, "parent-a");
        let a = CacheId::from_step(Some(&parent), "RUN echo hi");
        let b = CacheId::from_step(Some(&parent), "RUN echo hi");
        assert_eq!(a, b);
        assert_eq!(a.to_string().len(), 64);
        assert!(a.to_string().chars().all(|c| c.is_ascii_hexdigit()));
    }
    #[test]
    fn cache_id_diverges_on_parent() {
        assert_ne!(
            CacheId::from_step(Some(&CacheId::from_step(None, "parent-a")), "RUN echo hi"),
            CacheId::from_step(Some(&CacheId::from_step(None, "parent-b")), "RUN echo hi")
        );
    }
    #[test]
    fn cache_id_diverges_on_descriptor() {
        assert_ne!(
            CacheId::from_step(Some(&CacheId::from_step(None, "parent-a")), "RUN echo hi"),
            CacheId::from_step(Some(&CacheId::from_step(None, "parent-a")), "RUN echo bye")
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

    fn layer_dir(&self, id: &CacheId) -> PathBuf {
        self.layers_dir.join(id.to_string())
    }

    /// Load a cache layer's metadata iff it is present AND complete (an fs layer's rootfs snapshot must
    /// exist). Returns None on any miss so a partial/corrupt layer is never served as a hit.
    pub fn load_layer(&self, id: &CacheId) -> Option<Value> {
        let dir = self.layer_dir(id);
        let meta: Value =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        if meta.get("fs").and_then(|v| v.as_bool()).unwrap_or(false) && !dir.join("rootfs").is_dir()
        {
            return None;
        }
        Some(meta)
    }

    /// Materialize a cached fs layer's rootfs snapshot into `dst` (the live stage rootfs), replacing it.
    /// Returns false on failure — the caller aborts the build rather than continue on a wrong rootfs.
    pub fn materialize(&self, id: &CacheId, dst: &Path) -> bool {
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
        id: &CacheId,
        parent: Option<&CacheId>,
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
        let fs = crate::build::Instruction {
            name: inst.into(),
            arguments: args.into(),
        }
        .mutates_filesystem();
        if fs {
            let lr = dir.join("rootfs");
            if !matches!(std::process::Command::new("cp").arg("-a").arg(rootfs).arg(&lr).status(), Ok(s) if s.success())
            {
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
        }
        let parent = parent.map(ToString::to_string).unwrap_or_default();
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
