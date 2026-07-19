//! Named counters, gated by the counters mask.
//!
//! A counter is a `&'static str name -> u64` total. Increment/add only touch the
//! registry when profiling is enabled for the tag (checked by `hl_count!` /
//! `hl_add!`), so a counter under a disabled tag is a pure no-op — one relaxed load
//! + branch, no lock. When enabled, updates go through the sharded registry so
//! concurrent threads bumping different counters don't serialize on one lock.

use crate::shard::ShardMap;
use crate::sink;
use std::fmt::Write as _;
use std::sync::OnceLock;

/// Process-wide collection of named counters.
pub struct Counters {
    values: ShardMap<u64>,
}

impl Counters {
    pub fn global() -> &'static Self {
        static COUNTERS: OnceLock<Counters> = OnceLock::new();
        COUNTERS.get_or_init(|| Self {
            values: ShardMap::new(),
        })
    }

    #[inline]
    pub fn add(&self, name: &'static str, n: u64) {
        self.values.update(name, |value| *value += n);
    }

    pub fn get(&self, name: &str) -> u64 {
        self.values.get(name)
    }

    pub fn snapshot(&self) -> Vec<(&'static str, u64)> {
        self.values.snapshot()
    }

    pub fn reset(&self) {
        self.values.clear();
    }

    pub fn dump(&self) {
        let snapshot = self.snapshot();
        let mut buffer = String::with_capacity(32 + snapshot.len() * 24);
        buffer.push_str("[counters]\n");
        for (name, value) in snapshot {
            let _ = writeln!(buffer, "  {name:<28} {value}");
        }
        sink::Output::global().write(&buffer);
    }
}
