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

/// Absolute host `CLOCK_MONOTONIC`, suitable for the `wp_presentation` clock domain.
#[cfg(unix)]
pub fn monotonic_nanos() -> Option<u64> {
    #[repr(C)]
    struct Timespec {
        seconds: i64,
        nanos: i64,
    }
    extern "C" {
        fn clock_gettime(clock: i32, value: *mut Timespec) -> i32;
    }
    #[cfg(target_os = "macos")]
    const CLOCK_MONOTONIC: i32 = 6;
    #[cfg(not(target_os = "macos"))]
    const CLOCK_MONOTONIC: i32 = 1;

    let mut value = Timespec {
        seconds: 0,
        nanos: 0,
    };
    // SAFETY: `value` is a valid writable timespec and CLOCK_MONOTONIC is supported on Unix hosts.
    let result = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut value) };
    (result == 0 && value.seconds >= 0 && (0..1_000_000_000).contains(&value.nanos)).then(|| {
        (value.seconds as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(value.nanos as u64)
    })
}
