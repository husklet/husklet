//! Timing spans, gated by the counters mask.
//!
//! `hl_span!(tag, name)` returns a [`Span`] guard. When `COUNTERS_ON & tag != 0` the
//! guard captures `Instant::now()` and, on `Drop`, folds the elapsed nanoseconds into
//! a `name -> {count, sum_ns, max_ns}` registry. When the tag is off the guard holds
//! `None` and its `Drop` is a no-op — zero cost beyond one relaxed load.
//!
//! Like counters, the registry is sharded so concurrent spans on different names do
//! not contend, and the fold on `Drop` is a tiny O(1) critical section with no I/O.

use crate::shard::Sharded;
use crate::sink;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::Instant;

/// Accumulated statistics for one named span.
#[derive(Copy, Clone, Debug, Default)]
pub struct TimingStat {
    /// Number of completed spans.
    pub count: u64,
    /// Total elapsed nanoseconds across all spans.
    pub sum_ns: u64,
    /// Largest single span in nanoseconds.
    pub max_ns: u64,
}

fn registry() -> &'static Sharded<TimingStat> {
    static REG: OnceLock<Sharded<TimingStat>> = OnceLock::new();
    REG.get_or_init(Sharded::new)
}

/// A running timing span. Created by [`start`] / `hl_span!`. Records on `Drop` iff it
/// was started active. Holds only a name + start instant.
#[must_use = "a span records elapsed time on drop; bind it to a variable (`let _s = ...`)"]
pub struct Span {
    name: &'static str,
    start: Option<Instant>,
}

impl Span {
    /// A no-op span (tag was disabled). Drop does nothing.
    #[inline]
    pub const fn disabled() -> Span {
        Span {
            name: "",
            start: None,
        }
    }
}

impl Drop for Span {
    #[inline]
    fn drop(&mut self) {
        if let Some(start) = self.start {
            let ns = start.elapsed().as_nanos() as u64;
            registry().update(self.name, |e| {
                e.count += 1;
                e.sum_ns += ns;
                if ns > e.max_ns {
                    e.max_ns = ns;
                }
            });
        }
    }
}

/// Start an active span. Reached only after the tag gate passed.
#[inline]
pub fn start(name: &'static str) -> Span {
    Span {
        name,
        start: Some(Instant::now()),
    }
}

/// All timing stats as `(name, stat)`, sorted by name.
pub fn timing_snapshot() -> Vec<(&'static str, TimingStat)> {
    registry().snapshot()
}

/// Clear every timing stat.
pub fn timing_reset() {
    registry().clear();
}

/// Write a human-readable timing table (count, total ms, avg us, max us) to the sink.
/// Snapshots first, then writes — no registry lock is held during the sink I/O.
pub fn timing_dump() {
    let snap = timing_snapshot();
    let mut buf = String::with_capacity(64 + snap.len() * 48);
    buf.push_str("[timing] name / count / total ms / avg us / max us\n");
    for (name, s) in snap {
        let total_ms = s.sum_ns as f64 / 1.0e6;
        let avg_us = if s.count > 0 {
            (s.sum_ns as f64 / s.count as f64) / 1.0e3
        } else {
            0.0
        };
        let max_us = s.max_ns as f64 / 1.0e3;
        let _ = writeln!(
            buf,
            "  {name:<28} {:>8} {total_ms:>12.3} {avg_us:>12.3} {max_us:>12.3}",
            s.count
        );
    }
    sink::write_line(&buf);
}
