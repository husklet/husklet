//! Count-carrying latched diagnostics.
//!
//! A latch answers "did this ever happen". It cannot answer "is this still happening", and the two
//! demand opposite responses: a single `presented_time_unreadable` at window creation is a transient a
//! window recovers from, while the same line repeating every frame is a dead window. One latched line
//! looks identical in both cases, and reading it as the second when it was the first cost this project a
//! build cycle and two misrouted investigations.
//!
//! So a cause reports on a schedule instead of once: the first occurrence, then at each power of ten.
//! `count=1` and `count=4177` are then distinguishable at a glance, and a permanently failing surface
//! costs six lines over a hundred thousand frames rather than a hundred thousand.

use std::collections::HashMap;
use std::hash::Hash;

/// Whether an occurrence should speak, given how many times its cause has now fired.
///
/// True at 1, 10, 100, 1000, … — dense enough that a recurring fault announces itself within ten frames
/// of starting, sparse enough that a permanent one never floods.
pub fn milestone(count: u64) -> bool {
    let mut step = 1u64;
    while step < count {
        match step.checked_mul(10) {
            Some(next) => step = next,
            None => return false,
        }
    }
    step == count
}

/// Occurrence counts for latched diagnostics, keyed by whatever identifies one cause at one site.
///
/// Deliberately not a bare `HashSet` of "already reported": the count IS the diagnostic.
pub struct Tally<K>(HashMap<K, u64>);

impl<K: Eq + Hash> Default for Tally<K> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: Eq + Hash> Tally<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one occurrence of `key`. Returns the running count when this one should be reported, and
    /// `None` when it falls between milestones.
    pub fn record(&mut self, key: K) -> Option<u64> {
        let count = self.0.entry(key).or_insert(0);
        *count += 1;
        milestone(*count).then_some(*count)
    }

    /// Forget a key's history — used when the thing it describes is gone, so a later occurrence on a
    /// reused identity reports from 1 again rather than staying silent behind a stale count.
    pub fn forget(&mut self, key: &K) {
        self.0.remove(key);
    }
}

/// A [`Tally`] usable from a `static`, for failure sites that have no `&mut self` to hang state on —
/// free functions, `&self` methods, and completion callbacks arriving on foreign threads.
///
/// Declare one per site: `static SKIPPED: SharedTally<(u32, u32)> = SharedTally::new();`
pub struct SharedTally<K>(std::sync::Mutex<Option<Tally<K>>>);

impl<K: Eq + Hash> SharedTally<K> {
    pub const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    /// The running count for `key` when this occurrence should speak.
    ///
    /// A poisoned lock reports rather than goes silent: over-reporting a failure is recoverable, losing
    /// one is the failure mode this whole family of diagnostics exists to prevent. `0` marks that the
    /// count itself is unavailable, so it can never be misread as a real first occurrence.
    pub fn record(&self, key: K) -> Option<u64> {
        let Ok(mut tally) = self.0.lock() else {
            return Some(0);
        };
        tally.get_or_insert_with(Tally::new).record(key)
    }
}

impl<K: Eq + Hash> Default for SharedTally<K> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{milestone, SharedTally, Tally};

    #[test]
    fn a_shared_tally_counts_across_calls_from_anywhere() {
        static SITE: SharedTally<&'static str> = SharedTally::new();
        assert_eq!(SITE.record("a"), Some(1));
        assert_eq!(SITE.record("a"), None);
        assert_eq!(SITE.record("b"), Some(1), "keys count independently");
        // "a" has fired twice; carry it to 100 and collect what it chose to report.
        let reported: Vec<u64> = (3..=100).filter_map(|_| SITE.record("a")).collect();
        assert_eq!(reported, vec![10, 100]);
    }

    #[test]
    fn milestones_are_the_first_occurrence_then_each_power_of_ten() {
        assert!(milestone(1));
        assert!(milestone(10));
        assert!(milestone(100));
        assert!(milestone(1_000));
        assert!(milestone(1_000_000));
        for quiet in [2, 3, 9, 11, 99, 101, 999, 4177] {
            assert!(!milestone(quiet), "{quiet} should not report");
        }
    }

    #[test]
    fn milestone_never_overflows_on_a_pathological_count() {
        assert!(!milestone(u64::MAX));
    }

    #[test]
    fn a_tally_distinguishes_a_transient_from_a_standing_fault() {
        let mut tally = Tally::new();
        // A one-off reports once and never speaks again.
        assert_eq!(tally.record("transient"), Some(1));

        // A standing fault keeps reporting, carrying a count that says so.
        let mut reported = Vec::new();
        for _ in 0..1_000 {
            if let Some(count) = tally.record("standing") {
                reported.push(count);
            }
        }
        assert_eq!(reported, vec![1, 10, 100, 1_000]);
    }

    #[test]
    fn forgetting_a_key_lets_a_reused_identity_report_again() {
        let mut tally = Tally::new();
        assert_eq!(tally.record(7), Some(1));
        assert_eq!(tally.record(7), None);
        tally.forget(&7);
        assert_eq!(tally.record(7), Some(1));
    }
}
