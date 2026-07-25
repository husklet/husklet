//! [`Clock`] — the pacing/timeline port, so the runtime's fence timestamps and scheduling are testable
//! with a deterministic fake instead of wall-clock time.
//!
//! The runtime stamps every fence signal with `now_nanos()` (see
//! [`FenceTimeline`](crate::runtime::model::timeline::FenceTimeline)); a host binary injects
//! [`SystemClock`], tests inject [`FakeClock`].

use std::cell::Cell;

/// A monotonic time source for pacing + timeline stamping. `&self` so a `Session` can hold a
/// `Box<dyn Clock>` and stamp signals without a mutable borrow.
pub trait Clock {
    /// Monotonic nanoseconds since an arbitrary fixed epoch. Never decreases.
    fn now_nanos(&self) -> u64;
}

/// A real monotonic clock backed by [`std::time::Instant`], epoch = construction time.
pub struct SystemClock {
    start: std::time::Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_nanos(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

/// A deterministic test clock: reads back exactly what it was set to, advances only when told to.
pub struct FakeClock {
    now: Cell<u64>,
}

impl FakeClock {
    pub fn new(start: u64) -> Self {
        Self {
            now: Cell::new(start),
        }
    }

    /// Advance the clock by `delta` nanoseconds.
    pub fn advance(&self, delta: u64) {
        self.now.set(self.now.get().saturating_add(delta));
    }

    /// Set the clock to an absolute value.
    pub fn set(&self, now: u64) {
        self.now.set(now);
    }
}

impl Clock for FakeClock {
    fn now_nanos(&self) -> u64 {
        self.now.get()
    }
}
