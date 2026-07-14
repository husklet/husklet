//! [`Clock`]: the monotonic time source the scene reads for frame pacing and `wp_presentation`
//! feedback timing.
//!
//! One method — `now_nanos()` — so the entire policy (schedule decisions, present timing) is driven by
//! a source a test can script. Replaces `hl-compositor`'s `Instant`-based `now_ms`/`now_us`/`start`
//! with an injectable port; a real adapter reads the host monotonic clock, a test reads a `FakeClock`
//! that returns scripted nanos.

/// A monotonic, nanosecond-resolution clock. Values must be non-decreasing across calls.
pub trait Clock {
    /// Nanoseconds since an arbitrary but fixed epoch. The scene only ever takes DIFFERENCES, so the
    /// epoch is irrelevant — only monotonicity and resolution matter.
    fn now_nanos(&self) -> u64;
}
