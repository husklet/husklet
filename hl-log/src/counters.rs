//! Named counters, gated by the counters mask.
//!
//! A counter is a `&'static str name -> u64` total. Increment/add only touch the
//! registry when `COUNTERS_ON & tag != 0` (checked at the call site by `hl_count!` /
//! `hl_add!`), so a counter under a disabled tag is a pure no-op — one relaxed load
//! + branch, no lock. When enabled, updates go through the sharded registry so
//! concurrent threads bumping different counters don't serialize on one lock.

use crate::shard::Sharded;
use crate::sink;
use std::fmt::Write as _;
use std::sync::OnceLock;

fn registry() -> &'static Sharded<u64> {
    static REG: OnceLock<Sharded<u64>> = OnceLock::new();
    REG.get_or_init(Sharded::new)
}

/// Add `n` to the named counter. Reached only after the tag gate passed.
#[inline]
pub fn add(name: &'static str, n: u64) {
    registry().update(name, |v| *v += n);
}

/// The current value of a counter (0 if never touched).
pub fn get(name: &str) -> u64 {
    registry().get(name)
}

/// All counters as `(name, value)`, sorted by name.
pub fn counters_snapshot() -> Vec<(&'static str, u64)> {
    registry().snapshot()
}

/// Clear every counter.
pub fn counters_reset() {
    registry().clear();
}

/// Write a human-readable counter table to the active sink. Snapshots first, then
/// writes — no registry lock is held during the sink I/O.
pub fn counters_dump() {
    let snap = counters_snapshot();
    let mut buf = String::with_capacity(32 + snap.len() * 24);
    buf.push_str("[counters]\n");
    for (name, val) in snap {
        let _ = writeln!(buf, "  {name:<28} {val}");
    }
    sink::write_line(&buf);
}
