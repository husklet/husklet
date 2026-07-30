//! Host-input age: how long an `NSEvent` waited between AppKit stamping it and the presenter turning
//! it into a Wayland seat event. This is the host half of end-to-end input latency, and the only half
//! this crate owns — the guest half belongs to the seat and the client.

use hl_log::{tag, Level};

/// Nanoseconds a translated host event is allowed to age before its dispatch is reported as slow. One
/// 60 Hz refresh period: beyond this the event cannot reach the guest within the frame that produced it.
const SLOW_DISPATCH_NANOS: u64 = 16_666_667;

/// One host input event's arrival time, measured on the same monotonic clock the drawable's
/// `presentedTime` is validated against, so input age and presentation time are directly comparable.
pub(super) struct HostInput {
    stamped_nanos: Option<u64>,
}

impl HostInput {
    /// `timestamp` is AppKit's `NSEvent.timestamp`: seconds since boot on the mach timebase, which is
    /// the clock [`crate::scene::port::clock::monotonic_nanos`] reads on macOS.
    pub(super) fn stamped(timestamp: f64) -> Self {
        let nanos = timestamp * 1_000_000_000.0;
        Self {
            stamped_nanos: (timestamp.is_finite() && timestamp >= 0.0 && nanos <= u64::MAX as f64)
                .then(|| nanos.round() as u64),
        }
    }

    /// Report the age of this event now that it has become a seat event. Returns the age in nanoseconds,
    /// or `None` when either clock reading is unusable — a missing measurement is never reported as zero.
    pub(super) fn dispatched(self, kind: &str) -> Option<u64> {
        let age = self
            .stamped_nanos
            .zip(crate::scene::port::clock::monotonic_nanos())
            .map(|(stamped, now)| now.saturating_sub(stamped))?;
        hl_log::hl_add!(tag::PRESENT, "host_input_age_ns", age);
        hl_log::hl_count!(tag::PRESENT, "host_input_events");
        hl_log::hl_log!(
            tag::PRESENT,
            if age >= SLOW_DISPATCH_NANOS {
                Level::Warn
            } else {
                Level::Trace
            },
            "host_input kind={kind} age_us={}",
            age / 1_000
        );
        Some(age)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unusable_appkit_timestamps_report_no_measurement() {
        for timestamp in [-1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert_eq!(
                HostInput::stamped(timestamp).dispatched("pointer"),
                None,
                "timestamp {timestamp} must not be reported as an age"
            );
        }
    }

    #[test]
    fn age_grows_with_the_gap_between_stamp_and_dispatch() {
        let now = crate::scene::port::clock::monotonic_nanos().expect("monotonic clock");
        let one_second_ago = (now - 1_000_000_000) as f64 / 1_000_000_000.0;
        let age = HostInput::stamped(one_second_ago)
            .dispatched("pointer")
            .expect("a real stamp yields a real age");
        assert!(
            (900_000_000..2_000_000_000).contains(&age),
            "one-second-old event aged {age} ns"
        );
    }

    #[test]
    fn a_stamp_from_the_future_never_underflows() {
        let now = crate::scene::port::clock::monotonic_nanos().expect("monotonic clock");
        let ahead = (now + 5_000_000_000) as f64 / 1_000_000_000.0;
        assert_eq!(HostInput::stamped(ahead).dispatched("pointer"), Some(0));
    }

    #[test]
    fn slow_dispatch_threshold_is_one_refresh_period() {
        assert_eq!(SLOW_DISPATCH_NANOS, 16_666_667);
    }
}
