//! Container/exec id generation and reference resolution: a cheap FNV handle, real-entropy docker
//! ids, and the prefix/short-id/name resolver docker clients rely on.
use super::*;

/// Resolve a container ref (full id, **id prefix** like the docker CLI sends, or short name) to its
/// full map key. Docker clients show/round-trip the 12-char short id, so prefix resolution is
/// required for `docker logs/inspect/rm <shortid>` to work.
pub(crate) fn resolve_cid(g: &Inner, id: &str) -> Option<String> {
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
pub(crate) fn resolve_get<'g>(g: &'g Inner, id: &str) -> Option<(String, &'g Container)> {
    let full = resolve_cid(g, id)?;
    let c = g.containers.get(&full)?;
    Some((full, c))
}

/// A cheap, stable hex id for a built image (not a real digest — just a handle for the CLI).
pub(crate) fn md5_like(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub(crate) fn fake_id(seed: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}{h:016x}{h:016x}{h:08x}")
}

/// A fresh container/exec id with real entropy, shaped exactly like Docker's: 32 random bytes
/// (256 bits) hex-encoded to 64 lowercase chars. Docker derives the 12-char short id that clients
/// display/round-trip from the leading bytes of this, so the short id inherits the full entropy too.
/// (The previous implementation hashed a seed to a single 64-bit value and TILED it 4x — a 64-hex
/// string with only 16 hex of real entropy, trivially collidable and visibly not a real Docker id.)
/// Reads the OS CSPRNG (`/dev/urandom`); if that is somehow unavailable it falls back to a splitmix64
/// stream seeded from the nanosecond clock, pid and a never-reset per-process counter (the `image`
/// arg only seeds this fallback), so ids stay unique even across create/rm churn.
pub(crate) fn new_id(image: &str) -> String {
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
            ^ md5_like(image);
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
            g.containers
                .insert(id.to_string(), container(id, name));
        }
        g
    }

    // A distinct 64-char full id (leading bytes vary so 12-char short ids differ).
    const FULL_A: &str = "aaaa1111000000000000000000000000000000000000000000000000000000aa";
    const FULL_B: &str = "bbbb2222000000000000000000000000000000000000000000000000000000bb";

    #[test]
    fn resolve_cid_exact_full_id() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        assert_eq!(resolve_cid(&g, FULL_A), Some(FULL_A.to_string()));
    }

    #[test]
    fn resolve_cid_unique_prefix() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        // The 12-char short id docker clients round-trip resolves via prefix match.
        assert_eq!(resolve_cid(&g, &FULL_A[..12]), Some(FULL_A.to_string()));
        // A shorter unique prefix works too.
        assert_eq!(resolve_cid(&g, "aaaa"), Some(FULL_A.to_string()));
    }

    #[test]
    fn resolve_cid_ambiguous_prefix_is_none() {
        // Two containers share the "dead" prefix -> ambiguous -> unresolvable.
        let d1 = "dead0001000000000000000000000000000000000000000000000000000000d1";
        let d2 = "dead0002000000000000000000000000000000000000000000000000000000d2";
        let g = inner(&[(d1, "one"), (d2, "two")]);
        assert_eq!(resolve_cid(&g, "dead"), None);
    }

    #[test]
    fn resolve_cid_name_fallback_trims_leading_slash() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        // Plain name resolves.
        assert_eq!(resolve_cid(&g, "web"), Some(FULL_A.to_string()));
        // Docker-style "/web" trims the leading slash before matching the name.
        assert_eq!(resolve_cid(&g, "/web"), Some(FULL_A.to_string()));
    }

    #[test]
    fn resolve_cid_no_match_is_none() {
        let g = inner(&[(FULL_A, "web"), (FULL_B, "db")]);
        assert_eq!(resolve_cid(&g, "nonexistent"), None);
    }

    #[test]
    fn md5_like_is_fnv1a_deterministic() {
        // FNV-1a over zero bytes returns the offset basis unchanged.
        assert_eq!(md5_like(""), 0xcbf2_9ce4_8422_2325);
        // Deterministic and input-sensitive.
        assert_eq!(md5_like("nginx"), md5_like("nginx"));
        assert_ne!(md5_like("nginx"), md5_like("redis"));
    }

    #[test]
    fn fake_id_shape_and_determinism() {
        let id = fake_id("nginx");
        // `{h:016x}{h:016x}{h:016x}{h:08x}` — the same 64-bit hash tiled: three zero-padded
        // 16-hex groups (48 fixed chars) then `{h:08x}` (8..=16 chars), so total is 56..=64.
        assert!((56..=64).contains(&id.len()), "len {}", id.len());
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // The first three groups are identical (the tiling).
        assert_eq!(&id[0..16], &id[16..32]);
        assert_eq!(&id[16..32], &id[32..48]);
        // The trailing `{h:08x}` group is the low-order tail of the same zero-padded hash.
        assert!(id[0..16].ends_with(&id[48..]), "tail {:?} of {:?}", &id[48..], &id[0..16]);
        // Stable across calls; distinct seeds differ.
        assert_eq!(fake_id("nginx"), id);
        assert_ne!(fake_id("nginx"), fake_id("redis"));
    }

    #[test]
    fn new_id_is_64_hex_and_unique() {
        let a = new_id("img");
        let b = new_id("img");
        // Docker-shaped: 64 lowercase hex chars (32 random bytes).
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Real entropy: two calls with the same seed still differ.
        assert_ne!(a, b);
    }
}
