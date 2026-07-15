//! Global runtime state + the hot-path gate.
//!
//! Three atomics hold all runtime configuration. They default to 0/off, so a build
//! that never calls [`crate::init`] and never sets `HL_LOG` does no logging work
//! beyond the single relaxed load in [`enabled`].

use crate::level::Level;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::Once;

/// Tag mask that is currently ON for logging. `0` = everything off.
pub(crate) static ENABLED: AtomicU64 = AtomicU64::new(0);
/// Minimum severity that passes the gate, as a `Level` `u8`. Default `Warn`.
pub(crate) static MIN_LEVEL: AtomicU8 = AtomicU8::new(Level::Warn as u8);
/// Tag mask that is ON for counters + timing spans. `0` = profiling off.
pub(crate) static COUNTERS_ON: AtomicU64 = AtomicU64::new(0);

/// Runs `init()` exactly once for auto-init on first macro use.
static AUTO_INIT: Once = Once::new();
/// Fast-path flag so `ensure_init` is a single relaxed bool load once warm.
static INITED: AtomicBool = AtomicBool::new(false);

/// THE GATE. Returns whether a call site tagged `tag` at `level` should emit.
///
/// This is the single most performance-critical function in the crate: it sits in
/// front of every `hl_*!` macro. It is one relaxed load, an AND, a compare, and a
/// branch that is predicted not-taken when logging is off. No locks, no allocation.
#[inline(always)]
pub fn enabled(tag: u64, level: Level) -> bool {
    ENABLED.load(Relaxed) & tag != 0 && (level as u8) <= MIN_LEVEL.load(Relaxed)
}

/// Whether counters/timing are on for `tag`. One relaxed load + AND.
#[inline(always)]
pub fn counters_enabled(tag: u64) -> bool {
    COUNTERS_ON.load(Relaxed) & tag != 0
}

/// Turn the given tags ON for logging (OR into the mask).
pub fn enable(mask: u64) {
    ENABLED.fetch_or(mask, Relaxed);
}

/// Turn the given tags OFF for logging (AND-NOT out of the mask).
pub fn disable(mask: u64) {
    ENABLED.fetch_and(!mask, Relaxed);
}

/// Replace the entire enabled logging mask.
pub fn set_enabled(mask: u64) {
    ENABLED.store(mask, Relaxed);
}

/// Set the minimum severity that passes the gate.
pub fn set_level(level: Level) {
    MIN_LEVEL.store(level as u8, Relaxed);
}

/// The current minimum level.
pub fn level() -> Level {
    Level::from_u8(MIN_LEVEL.load(Relaxed))
}

/// The current enabled logging mask.
pub fn enabled_mask() -> u64 {
    ENABLED.load(Relaxed)
}

/// Turn the given tags ON for counters/timing (OR into the mask).
pub fn enable_counters(mask: u64) {
    COUNTERS_ON.fetch_or(mask, Relaxed);
}

/// Turn the given tags OFF for counters/timing.
pub fn disable_counters(mask: u64) {
    COUNTERS_ON.fetch_and(!mask, Relaxed);
}

/// Replace the entire counters mask.
pub fn set_counters(mask: u64) {
    COUNTERS_ON.store(mask, Relaxed);
}

/// The current counters mask.
pub fn counters_mask() -> u64 {
    COUNTERS_ON.load(Relaxed)
}

/// Run `init()` the first time any macro fires, so callers never have to remember
/// to initialize. Warm cost is one relaxed bool load + a predicted-taken branch; the
/// actual env parse happens once behind a `#[cold]` slow path.
#[inline]
pub fn ensure_init() {
    if !INITED.load(Relaxed) {
        ensure_init_slow();
    }
}

#[cold]
fn ensure_init_slow() {
    AUTO_INIT.call_once(|| {
        crate::init::init_from_env();
    });
    INITED.store(true, Relaxed);
}

/// Force the auto-init `Once` to be considered "done" without running env parsing.
/// Used by [`crate::init`] when a caller invokes `init()` explicitly, so the auto
/// path never re-parses afterward.
pub(crate) fn mark_auto_init_done() {
    AUTO_INIT.call_once(|| {});
    INITED.store(true, Relaxed);
}
