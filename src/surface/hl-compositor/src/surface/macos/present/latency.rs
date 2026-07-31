//! Host-input age: how long an `NSEvent` waited between AppKit stamping it and the presenter turning
//! it into a Wayland seat event. This is the host half of end-to-end input latency, and the only half
//! this crate owns — the guest half belongs to the seat and the client.

use hl_log::{tag, Level};

/// Nanoseconds a translated host event is allowed to age before its dispatch is reported as slow. One
/// 60 Hz refresh period: beyond this the event cannot reach the guest within the frame that produced it.
const SLOW_DISPATCH_NANOS: u64 = 16_666_667;

/// Darwin's `CLOCK_UPTIME_RAW`: nanoseconds since boot EXCLUDING time asleep — the `mach_absolute_time`
/// epoch `NSEvent.timestamp` is stamped on.
///
/// Deliberately not [`crate::scene::port::clock::monotonic_nanos`]. That reads `CLOCK_MONOTONIC`, which
/// on Darwin INCLUDES time asleep, so subtracting an `NSEvent` stamp from it yields the machine's total
/// sleep time rather than the event's age. On a host that had slept overnight this reported every input
/// as ~3.7 hours old, which pushed all 155 events in a measured run over [`SLOW_DISPATCH_NANOS`] and
/// logged each one at `Warn` — a level that survives the default filter, so every mouse-motion event
/// wrote a log line from the input hot path.
fn appkit_now_nanos() -> Option<u64> {
    #[repr(C)]
    struct Timespec {
        seconds: i64,
        nanos: i64,
    }
    extern "C" {
        fn clock_gettime(clock: i32, value: *mut Timespec) -> i32;
    }
    const CLOCK_UPTIME_RAW: i32 = 8;

    let mut value = Timespec {
        seconds: 0,
        nanos: 0,
    };
    // SAFETY: `value` is a valid writable timespec and CLOCK_UPTIME_RAW is supported on Darwin.
    let result = unsafe { clock_gettime(CLOCK_UPTIME_RAW, &mut value) };
    (result == 0 && value.seconds >= 0 && (0..1_000_000_000).contains(&value.nanos)).then(|| {
        (value.seconds as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(value.nanos as u64)
    })
}

/// One host input event's arrival time on the AppKit event clock, so the reported age is the time the
/// event actually waited rather than the offset between two unrelated epochs.
pub(super) struct HostInput {
    stamped_nanos: Option<u64>,
}

impl HostInput {
    /// `timestamp` is AppKit's `NSEvent.timestamp`: seconds since boot on the mach timebase, the epoch
    /// [`appkit_now_nanos`] reads.
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
            .zip(appkit_now_nanos())
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

    /// The bug this file was fixed for: an event stamped NOW must report an age near zero. Reading the
    /// wrong Darwin clock made this the machine's total sleep time instead, so the regression is only
    /// visible on a host that has actually slept — assert the property, not a clock identity.
    #[test]
    fn an_event_stamped_now_is_not_reported_as_stale() {
        let now = appkit_now_nanos().expect("AppKit uptime clock");
        let age = HostInput::stamped(now as f64 / 1_000_000_000.0)
            .dispatched("pointer")
            .expect("a real stamp yields a real age");
        assert!(
            age < SLOW_DISPATCH_NANOS,
            "an event stamped now aged {age} ns, over the {SLOW_DISPATCH_NANOS} ns slow threshold"
        );
    }

    #[test]
    fn age_grows_with_the_gap_between_stamp_and_dispatch() {
        let now = appkit_now_nanos().expect("AppKit uptime clock");
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
        let now = appkit_now_nanos().expect("AppKit uptime clock");
        let ahead = (now + 5_000_000_000) as f64 / 1_000_000_000.0;
        assert_eq!(HostInput::stamped(ahead).dispatched("pointer"), Some(0));
    }

    #[test]
    fn slow_dispatch_threshold_is_one_refresh_period() {
        assert_eq!(SLOW_DISPATCH_NANOS, 16_666_667);
    }
}
