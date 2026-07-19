//! Container/exec id generation and reference resolution: a cheap FNV handle, real-entropy docker
//! ids, and the prefix/short-id/name resolver docker clients rely on.
use super::*;

/// Resolve a container ref (full id, **id prefix** like the docker CLI sends, or short name) to its
/// full map key. Docker clients show/round-trip the 12-char short id, so prefix resolution is
/// required for `docker logs/inspect/rm <shortid>` to work.
pub(crate) struct ContainerId;
impl ContainerId {
    pub(crate) fn resolve(g: &Inner, id: &str) -> Option<String> {
        if g.containers.contains_key(id) {
            return Some(id.to_string());
        }
        let hits: Vec<String> = g
            .containers
            .keys()
            .filter(|k| k.starts_with(id))
            .cloned()
            .collect();
        if hits.len() == 1 {
            return hits.into_iter().next();
        }
        // Fall back to the short-id "name" we expose in containers_json, then the user-assigned --name.
        let want = id.trim_start_matches('/');
        g.containers
            .keys()
            .find(|k| k.get(..12).map(|p| p == want).unwrap_or(false))
            .cloned()
            .or_else(|| {
                g.containers
                    .iter()
                    .find(|(_, c)| c.name == want)
                    .map(|(k, _)| k.clone())
            })
    }

    /// Resolve a container ref to its (full id, &Container) in one immutable borrow of `g`.
    /// `None` if the ref doesn't resolve or the container is gone. The returned reference
    /// borrows `g` for `'g`, so callers clone out any fields they need inside the borrow.
    pub(crate) fn get<'g>(g: &'g Inner, id: &str) -> Option<(String, &'g Container)> {
        let full = Self::resolve(g, id)?;
        let c = g.containers.get(&full)?;
        Some((full, c))
    }
}

/// A cheap, stable hex id for a built image (not a real digest — just a handle for the CLI).
pub(crate) struct Digest;
impl Digest {
    pub(crate) fn stable(s: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
}

/// A process-global monotonically-increasing sequence, used to make per-request staging paths unique
/// (`.build-ctx-<pid>-<seq>`, `.push-<pid>-<seq>`). A bare `<pid>` collides when two builds/pushes run
/// concurrently in one daemon process — one would delete/reuse the other's staging dir mid-flight; the
/// added seq gives each request a distinct path.
pub(crate) fn next_staging_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

impl Digest {
    pub(crate) fn fake(seed: &str) -> String {
        let mut h: u64 = 1469598103934665603;
        for b in seed.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        format!("{h:016x}{h:016x}{h:016x}{h:08x}")
    }
}

/// A fresh container/exec id with real entropy, shaped exactly like Docker's: 32 random bytes
/// (256 bits) hex-encoded to 64 lowercase chars. Docker derives the 12-char short id that clients
/// display/round-trip from the leading bytes of this, so the short id inherits the full entropy too.
/// (The previous implementation hashed a seed to a single 64-bit value and TILED it 4x — a 64-hex
/// string with only 16 hex of real entropy, trivially collidable and visibly not a real Docker id.)
/// Reads the OS CSPRNG (`/dev/urandom`); if that is somehow unavailable it falls back to a splitmix64
/// stream seeded from the nanosecond clock, pid and a never-reset per-process counter (the `image`
/// arg only seeds this fallback), so ids stay unique even across create/rm churn.
impl ContainerId {
    pub(crate) fn new(image: &str) -> String {
        let mut buf = [0u8; 32];
        if std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
            .is_err()
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let mut s = nanos
                ^ (std::process::id() as u64).rotate_left(32)
                ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ Digest::stable(image);
            for chunk in buf.chunks_mut(8) {
                // splitmix64: distinct 64-bit output per 8-byte chunk.
                s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = s;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^= z >> 31;
                for (i, b) in chunk.iter_mut().enumerate() {
                    *b = (z >> (i * 8)) as u8;
                }
            }
        }
        let mut out = String::with_capacity(64);
        for b in &buf {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(id: &str, name: &str) -> Container {
        Container {
            id: id.to_string(),
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn inner(cs: &[(&str, &str)]) -> Inner {
        let mut g = Inner::default();
        for (id, name) in cs {
            g.containers.insert(id.to_string(), container(id, name));
        }
        g
    }

    // A distinct 64-char full id (leading bytes vary so 12-char short ids differ).
    const FULL_A: &str = "aaaa1111000000000000000000000000000000000000000000000000000000aa";
    const FULL_B: &str = "bbbb2222000000000000000000000000000000000000000000000000000000bb";

    #[test]
    fn resolve_cid_exact_full_id() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        assert_eq!(ContainerId::resolve(&g, FULL_A), Some(FULL_A.to_string()));
    }

    #[test]
    fn resolve_cid_unique_prefix() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        // The 12-char short id docker clients round-trip resolves via prefix match.
        assert_eq!(
            ContainerId::resolve(&g, &FULL_A[..12]),
            Some(FULL_A.to_string())
        );
        // A shorter unique prefix works too.
        assert_eq!(ContainerId::resolve(&g, "aaaa"), Some(FULL_A.to_string()));
    }

    #[test]
    fn resolve_cid_ambiguous_prefix_is_none() {
        // Two containers share the "dead" prefix -> ambiguous -> unresolvable.
        let d1 = "dead0001000000000000000000000000000000000000000000000000000000d1";
        let d2 = "dead0002000000000000000000000000000000000000000000000000000000d2";
        let g = inner(&[(d1, "one"), (d2, "two")]);
        assert_eq!(ContainerId::resolve(&g, "dead"), None);
    }

    #[test]
    fn resolve_cid_name_fallback_trims_leading_slash() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        // Plain name resolves.
        assert_eq!(ContainerId::resolve(&g, "web"), Some(FULL_A.to_string()));
        // Docker-style "/web" trims the leading slash before matching the name.
        assert_eq!(ContainerId::resolve(&g, "/web"), Some(FULL_A.to_string()));
    }

    #[test]
    fn resolve_cid_no_match_is_none() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        assert_eq!(ContainerId::resolve(&g, "nonexistent"), None);
    }

    #[test]
    fn md5_like_is_fnv1a_deterministic() {
        // FNV-1a over zero bytes returns the offset basis unchanged.
        assert_eq!(Digest::stable(""), 0xcbf2_9ce4_8422_2325);
        // Deterministic and input-sensitive.
        assert_eq!(Digest::stable("nginx"), Digest::stable("nginx"));
        assert_ne!(Digest::stable("nginx"), Digest::stable("redis"));
    }

    // Finding 27: per-request staging paths must be distinct. `next_staging_seq` hands out a fresh
    // monotonic value each call, so `.build-ctx-<pid>-<seq>` / `.push-<pid>-<seq>` never collide across
    // concurrent operations in one process.
    #[test]
    fn next_staging_seq_is_monotonic_and_unique() {
        let a = next_staging_seq();
        let b = next_staging_seq();
        let c = next_staging_seq();
        assert!(a < b && b < c, "strictly increasing: {a} {b} {c}");
        // Distinct staging paths for two concurrent builds.
        let p1 = format!(".build-ctx-{}-{}", std::process::id(), next_staging_seq());
        let p2 = format!(".build-ctx-{}-{}", std::process::id(), next_staging_seq());
        assert_ne!(p1, p2, "two staging paths differ");
    }

    #[test]
    fn fake_id_shape_and_determinism() {
        let id = Digest::fake("nginx");
        // `{h:016x}{h:016x}{h:016x}{h:08x}` — the same 64-bit hash tiled: three zero-padded
        // 16-hex groups (48 fixed chars) then `{h:08x}` (8..=16 chars), so total is 56..=64.
        assert!((56..=64).contains(&id.len()), "len {}", id.len());
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // The first three groups are identical (the tiling).
        assert_eq!(&id[0..16], &id[16..32]);
        assert_eq!(&id[16..32], &id[32..48]);
        // The trailing `{h:08x}` group is the low-order tail of the same zero-padded hash.
        assert!(
            id[0..16].ends_with(&id[48..]),
            "tail {:?} of {:?}",
            &id[48..],
            &id[0..16]
        );
        // Stable across calls; distinct seeds differ.
        assert_eq!(Digest::fake("nginx"), id);
        assert_ne!(Digest::fake("nginx"), Digest::fake("redis"));
    }

    #[test]
    fn new_id_is_64_hex_and_unique() {
        let a = ContainerId::new("img");
        let b = ContainerId::new("img");
        // Docker-shaped: 64 lowercase hex chars (32 random bytes).
        assert_eq!(a.len(), 64);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Real entropy: two calls with the same seed still differ.
        assert_ne!(a, b);
    }

    #[test]
    fn new_id_bulk_are_all_64_lowercase_hex_and_collision_free() {
        // Structural invariant at scale: every id is exactly 64 lowercase hex chars, and across many
        // samples none collide (32 bytes of CSPRNG entropy). We assert the structure, not the
        // distribution of the random bytes.
        let ids: Vec<String> = (0..200).map(|_| ContainerId::new("img")).collect();
        for id in &ids {
            assert_eq!(id.len(), 64, "bad length for {id}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "non-lowercase-hex char in {id}"
            );
        }
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "ids collided");
    }

    #[test]
    fn new_id_short_id_prefix_is_hex() {
        // Docker derives the 12-char short id clients display from the leading bytes, so that prefix
        // must itself be valid lowercase hex.
        let id = ContainerId::new("nginx");
        let short = &id[..12];
        assert_eq!(short.len(), 12);
        assert!(short
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn new_id_ignores_image_arg_for_shape() {
        // The `image` arg only seeds the (unused-here) fallback path; the produced id shape is
        // identical regardless of the argument, and distinct-argument ids never collide.
        let a = ContainerId::new("");
        let b = ContainerId::new("some/very:long-image@sha256:deadbeef");
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
        assert_ne!(a, b);
    }
}
