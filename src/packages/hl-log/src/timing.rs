//! Timing spans, gated by the counters mask.
//!
//! `hl_span!(tag, name)` returns a [`Span`] guard. When profiling includes the tag the
//! guard captures `Instant::now()` and, on `Drop`, folds the elapsed nanoseconds into
//! a `name -> {count, sum_ns, max_ns}` registry. When the tag is off the guard holds
//! `None` and its `Drop` is a no-op — zero cost beyond one relaxed load.
//!
//! Like counters, the registry is sharded so concurrent spans on different names do
//! not contend, and the fold on `Drop` is a tiny O(1) critical section with no I/O.

use crate::shard::ShardMap;
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

/// Process-wide collection of named timing statistics.
pub struct Timings {
    values: ShardMap<TimingStat>,
}

impl Timings {
    pub fn global() -> &'static Self {
        static TIMINGS: OnceLock<Timings> = OnceLock::new();
        TIMINGS.get_or_init(|| Self {
            values: ShardMap::new(),
        })
    }

    #[inline]
    pub fn start(&self, name: &'static str) -> Span {
        Span {
            name,
            start: Some(Instant::now()),
        }
    }

    pub fn snapshot(&self) -> Vec<(&'static str, TimingStat)> {
        self.values.snapshot()
    }

    pub fn reset(&self) {
        self.values.clear();
    }

    pub fn dump(&self) {
        let snapshot = self.snapshot();
        let mut buffer = String::with_capacity(64 + snapshot.len() * 48);
        buffer.push_str("[timing] name / count / total ms / avg us / max us\n");
        for (name, statistic) in snapshot {
            let total_ms = statistic.sum_ns as f64 / 1.0e6;
            let avg_us = if statistic.count > 0 {
                (statistic.sum_ns as f64 / statistic.count as f64) / 1.0e3
            } else {
                0.0
            };
            let max_us = statistic.max_ns as f64 / 1.0e3;
            let _ = writeln!(
                buffer,
                "  {name:<28} {:>8} {total_ms:>12.3} {avg_us:>12.3} {max_us:>12.3}",
                statistic.count
            );
        }
        sink::Output::global().write(&buffer);
    }
}

/// A running timing span. Created by [`Timings::start`] / `hl_span!`. Records on `Drop` iff it
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
        Span { name: "", start: None }
    }
}

impl Drop for Span {
    #[inline]
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };

        let ns = start.elapsed().as_nanos() as u64;
        Timings::global().values.update(self.name, |statistic| {
            statistic.count += 1;
            statistic.sum_ns += ns;
            statistic.max_ns = statistic.max_ns.max(ns);
        });
    }
}
