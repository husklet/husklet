//! A tiny sharded `&'static str -> V` map, shared by counters and timing.
//!
//! Design goals (per the perf brief): never halt, never hold a lock across I/O, and
//! resist contention. Reads/writes on *different* names hit *different* shards, so N
//! threads bumping N distinct counters don't serialize on one lock. Critical sections
//! are a single `HashMap` entry plus an arithmetic fold — no allocation of `V` beyond
//! first insert, no logging, no I/O. Every lock is taken poison-safe
//! (`unwrap_or_else(|e| e.into_inner())`) so a panic in one thread can't wedge others.
//!
//! This path is reached ONLY after the `COUNTERS_ON` gate passed at the call site, so
//! when profiling is off (the default, and always in release) none of this runs.

use std::collections::HashMap;
use std::sync::Mutex;

/// Number of lock stripes. Power of two so `& (SHARDS-1)` selects a shard.
const SHARDS: usize = 16;

/// A striped map keyed by interned `&'static str` names.
pub struct Sharded<V> {
    shards: Vec<Mutex<HashMap<&'static str, V>>>,
}

impl<V: Default + Clone> Sharded<V> {
    /// Build an empty sharded map.
    pub fn new() -> Self {
        let mut shards = Vec::with_capacity(SHARDS);
        for _ in 0..SHARDS {
            shards.push(Mutex::new(HashMap::new()));
        }
        Sharded { shards }
    }

    #[inline]
    fn shard_index(name: &str) -> usize {
        // FNV-1a over the name bytes: deterministic per-name (unlike `RandomState`),
        // cheap, good distribution for short identifiers.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in name.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        (h as usize) & (SHARDS - 1)
    }

    /// Apply `f` to the (default-initialized on first touch) value for `name`. The
    /// shard lock is held only for the duration of `f`, which must be O(1) and must
    /// not perform I/O or re-enter the registry.
    #[inline]
    pub fn update<F: FnOnce(&mut V)>(&self, name: &'static str, f: F) {
        let shard = &self.shards[Self::shard_index(name)];
        let mut map = shard.lock().unwrap_or_else(|e| e.into_inner());
        f(map.entry(name).or_default());
    }

    /// Current value for `name` (default if absent).
    pub fn get(&self, name: &str) -> V {
        let shard = &self.shards[Self::shard_index(name)];
        let map = shard.lock().unwrap_or_else(|e| e.into_inner());
        map.get(name).cloned().unwrap_or_default()
    }

    /// Snapshot every entry, sorted by name. Each shard is locked briefly, in turn;
    /// the collected `Vec` is returned WITHOUT holding any lock, so the caller can do
    /// I/O (dump) with no lock held.
    pub fn snapshot(&self) -> Vec<(&'static str, V)> {
        let mut out = Vec::new();
        for shard in &self.shards {
            let map = shard.lock().unwrap_or_else(|e| e.into_inner());
            out.extend(map.iter().map(|(k, v)| (*k, v.clone())));
        }
        out.sort_by(|a, b| a.0.cmp(b.0));
        out
    }

    /// Clear every shard.
    pub fn clear(&self) {
        for shard in &self.shards {
            shard.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }
}

impl<V: Default + Clone> Default for Sharded<V> {
    fn default() -> Self {
        Self::new()
    }
}
