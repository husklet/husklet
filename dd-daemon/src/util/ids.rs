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
