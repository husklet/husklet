//! Host-neutral clocks, normalized times, and overflow-safe deadlines.
//!
//! This crate contains no operating-system clock implementation. Runtime and
//! test code inject clock implementations through [`MonotonicClock`] and
//! [`RealtimeClock`].

#![forbid(unsafe_code)]

use core::fmt;

/// Number of nanoseconds in one second.
pub const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

/// A failure reported by an injected clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockError {
    /// The requested clock is unavailable on this host.
    NotSupported,
    /// A blocking clock operation was interrupted.
    Interrupted,
    /// The clock provider failed.
    Failed,
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSupported => formatter.write_str("clock is not supported"),
            Self::Interrupted => formatter.write_str("clock operation was interrupted"),
            Self::Failed => formatter.write_str("clock provider failed"),
        }
    }
}

impl std::error::Error for ClockError {}

/// A non-negative normalized `timespec`.
///
/// `nanoseconds` is always less than one billion. This is a value type rather
/// than an FFI layout; platform adapters explicitly convert to native types.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Timespec {
    seconds: u64,
    nanoseconds: u32,
}

impl Timespec {
    /// Zero seconds and zero nanoseconds.
    pub const ZERO: Self = Self {
        seconds: 0,
        nanoseconds: 0,
    };

    /// Creates an already-normalized value.
    ///
    /// Returns `None` when `nanoseconds` is outside `0..1_000_000_000`.
    #[must_use]
    pub const fn new(seconds: u64, nanoseconds: u32) -> Option<Self> {
        if nanoseconds < NANOSECONDS_PER_SECOND as u32 {
            Some(Self { seconds, nanoseconds })
        } else {
            None
        }
    }

    /// Normalizes an arbitrary non-negative seconds/nanoseconds pair.
    ///
    /// Returns `None` if carrying whole seconds would overflow.
    #[must_use]
    pub const fn normalize(seconds: u64, nanoseconds: u64) -> Option<Self> {
        let carried = nanoseconds / NANOSECONDS_PER_SECOND;
        let Some(seconds) = seconds.checked_add(carried) else {
            return None;
        };
        Some(Self {
            seconds,
            nanoseconds: (nanoseconds % NANOSECONDS_PER_SECOND) as u32,
        })
    }

    /// Converts the engine's host-neutral nanosecond clock value.
    #[must_use]
    pub const fn from_nanoseconds(value: u64) -> Self {
        Self {
            seconds: value / NANOSECONDS_PER_SECOND,
            nanoseconds: (value % NANOSECONDS_PER_SECOND) as u32,
        }
    }

    /// Whole seconds.
    #[must_use]
    pub const fn seconds(self) -> u64 {
        self.seconds
    }

    /// Fractional nanoseconds in `0..1_000_000_000`.
    #[must_use]
    pub const fn subsecond_nanoseconds(self) -> u32 {
        self.nanoseconds
    }

    /// Converts to a nanosecond count if the value is representable.
    #[must_use]
    pub const fn checked_nanoseconds(self) -> Option<u64> {
        let Some(seconds) = self.seconds.checked_mul(NANOSECONDS_PER_SECOND) else {
            return None;
        };
        seconds.checked_add(self.nanoseconds as u64)
    }

    /// Adds two normalized values, returning `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, other: Self) -> Option<Self> {
        let Some(seconds) = self.seconds.checked_add(other.seconds) else {
            return None;
        };
        Self::normalize(seconds, self.nanoseconds as u64 + other.nanoseconds as u64)
    }

    /// Subtracts `earlier`, borrowing one second when necessary.
    ///
    /// Returns `None` when `earlier` is later than `self`.
    #[must_use]
    pub const fn checked_sub(self, earlier: Self) -> Option<Self> {
        if self.seconds < earlier.seconds || (self.seconds == earlier.seconds && self.nanoseconds < earlier.nanoseconds)
        {
            return None;
        }

        if self.nanoseconds >= earlier.nanoseconds {
            Some(Self {
                seconds: self.seconds - earlier.seconds,
                nanoseconds: self.nanoseconds - earlier.nanoseconds,
            })
        } else {
            Some(Self {
                seconds: self.seconds - earlier.seconds - 1,
                nanoseconds: self.nanoseconds + NANOSECONDS_PER_SECOND as u32 - earlier.nanoseconds,
            })
        }
    }
}

/// A reading from a monotonic clock, expressed in nanoseconds from its epoch.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicInstant(u64);

impl MonotonicInstant {
    /// Creates a reading from the host-neutral nanosecond representation.
    #[must_use]
    pub const fn from_nanoseconds(value: u64) -> Self {
        Self(value)
    }

    /// Returns the host-neutral nanosecond representation.
    #[must_use]
    pub const fn nanoseconds(self) -> u64 {
        self.0
    }

    /// Builds a deadline relative to this reading, saturating at `u64::MAX`.
    #[must_use]
    pub const fn deadline_after(self, duration: Duration) -> Deadline {
        Deadline(self.0.saturating_add(duration.0))
    }

    /// Elapsed time since an earlier reading.
    ///
    /// Returns `None` if the injected monotonic clock moved backwards.
    #[must_use]
    pub const fn checked_duration_since(self, earlier: Self) -> Option<Duration> {
        match self.0.checked_sub(earlier.0) {
            Some(value) => Some(Duration(value)),
            None => None,
        }
    }
}

/// A non-negative duration represented in nanoseconds.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Duration(u64);

impl Duration {
    /// Zero duration.
    pub const ZERO: Self = Self(0);

    /// Creates a duration from nanoseconds.
    #[must_use]
    pub const fn from_nanoseconds(value: u64) -> Self {
        Self(value)
    }

    /// Creates a duration from a normalized timespec.
    #[must_use]
    pub const fn from_timespec(value: Timespec) -> Option<Self> {
        match value.checked_nanoseconds() {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the duration in nanoseconds.
    #[must_use]
    pub const fn nanoseconds(self) -> u64 {
        self.0
    }

    /// Returns the normalized timespec representation.
    #[must_use]
    pub const fn timespec(self) -> Timespec {
        Timespec::from_nanoseconds(self.0)
    }
}

/// An absolute monotonic deadline.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Deadline(u64);

impl Deadline {
    /// The latest representable deadline.
    pub const MAX: Self = Self(u64::MAX);

    /// Creates an absolute deadline.
    #[must_use]
    pub const fn from_nanoseconds(value: u64) -> Self {
        Self(value)
    }

    /// Returns the absolute host-neutral nanosecond value.
    #[must_use]
    pub const fn nanoseconds(self) -> u64 {
        self.0
    }

    /// Returns the remaining duration, or zero when the deadline has elapsed.
    #[must_use]
    pub const fn remaining_at(self, now: MonotonicInstant) -> Duration {
        Duration(self.0.saturating_sub(now.0))
    }

    /// Reports whether the deadline has elapsed at `now`.
    #[must_use]
    pub const fn has_elapsed_at(self, now: MonotonicInstant) -> bool {
        now.0 >= self.0
    }
}

/// An injectable monotonic clock.
pub trait MonotonicClock {
    /// Reads the current monotonic time.
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError>;

    /// Sleeps until an absolute monotonic deadline.
    ///
    /// Clocks that cannot sleep retain the default refusal.
    fn sleep_until(&self, _deadline: Deadline) -> Result<(), ClockError> {
        Err(ClockError::NotSupported)
    }
}

/// An injectable realtime clock.
pub trait RealtimeClock {
    /// Reads realtime as a non-negative normalized timespec.
    fn realtime_now(&self) -> Result<Timespec, ClockError>;
}

/// Convenience trait for providers implementing both required clock domains.
pub trait Clock: MonotonicClock + RealtimeClock + Send + Sync {}

impl<T: MonotonicClock + RealtimeClock + Send + Sync + ?Sized> Clock for T {}

#[cfg(test)]
mod tests {
    use super::{
        ClockError, Deadline, Duration, MonotonicClock, MonotonicInstant, NANOSECONDS_PER_SECOND, RealtimeClock,
        Timespec,
    };
    use std::cell::Cell;

    struct InjectedClock {
        monotonic: Cell<u64>,
        realtime: Cell<u64>,
        fail_next: Cell<Option<ClockError>>,
    }

    impl InjectedClock {
        fn take_failure(&self) -> Result<(), ClockError> {
            match self.fail_next.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    impl MonotonicClock for InjectedClock {
        fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
            self.take_failure()?;
            Ok(MonotonicInstant::from_nanoseconds(self.monotonic.get()))
        }

        fn sleep_until(&self, deadline: Deadline) -> Result<(), ClockError> {
            self.take_failure()?;
            self.monotonic.set(deadline.nanoseconds());
            Ok(())
        }
    }

    impl RealtimeClock for InjectedClock {
        fn realtime_now(&self) -> Result<Timespec, ClockError> {
            self.take_failure()?;
            Ok(Timespec::from_nanoseconds(self.realtime.get()))
        }
    }

    #[test]
    fn injected_clock_matches() {
        let clock = InjectedClock {
            monotonic: Cell::new(3_000_000_007),
            realtime: Cell::new(9_000_000_011),
            fail_next: Cell::new(None),
        };

        let monotonic = clock.monotonic_now().unwrap();
        assert_eq!(monotonic.nanoseconds(), 3_000_000_007);
        assert_eq!(
            Timespec::from_nanoseconds(monotonic.nanoseconds()),
            Timespec::new(3, 7).unwrap()
        );
        assert_eq!(clock.realtime_now().unwrap(), Timespec::new(9, 11).unwrap());

        clock.sleep_until(Deadline::from_nanoseconds(7_000_000_000)).unwrap();
        assert_eq!(clock.monotonic_now().unwrap().nanoseconds(), 7_000_000_000);

        clock.fail_next.set(Some(ClockError::Interrupted));
        assert_eq!(
            clock.sleep_until(Deadline::from_nanoseconds(10_000_000_000)),
            Err(ClockError::Interrupted)
        );
        assert_eq!(clock.monotonic.get(), 7_000_000_000);
    }

    #[test]
    fn injected_realtime_matches() {
        let clock = InjectedClock {
            monotonic: Cell::new(0),
            realtime: Cell::new(123_456_789 * NANOSECONDS_PER_SECOND),
            fail_next: Cell::new(None),
        };
        let value = clock.realtime_now().unwrap();
        assert_eq!(value.seconds(), 123_456_789);
        assert_eq!(value.subsecond_nanoseconds(), 0);
    }

    #[test]
    fn timespec_normalizes_and() {
        assert_eq!(Timespec::normalize(3, 2_000_000_007), Timespec::new(5, 7));
        let maximum = Timespec::from_nanoseconds(u64::MAX);
        assert_eq!(maximum.checked_nanoseconds(), Some(u64::MAX));
        assert_eq!(Timespec::new(1, 1_000_000_000), None);
        assert_eq!(Timespec::normalize(u64::MAX, 1_000_000_000), None);
    }

    #[test]
    fn timespec_arithmetic_normalizes() {
        let sum = Timespec::new(3, 900_000_000)
            .unwrap()
            .checked_add(Timespec::new(2, 200_000_007).unwrap());
        assert_eq!(sum, Timespec::new(6, 100_000_007));

        let difference = Timespec::new(6, 100_000_007)
            .unwrap()
            .checked_sub(Timespec::new(2, 200_000_008).unwrap());
        assert_eq!(difference, Timespec::new(3, 899_999_999));
        assert_eq!(
            Timespec::new(1, 0).unwrap().checked_sub(Timespec::new(1, 1).unwrap()),
            None
        );
    }

    #[test]
    fn timespec_arithmetic_rejects() {
        assert_eq!(
            Timespec::new(u64::MAX, 999_999_999)
                .unwrap()
                .checked_add(Timespec::new(0, 1).unwrap()),
            None
        );
        assert_eq!(Duration::from_timespec(Timespec::new(u64::MAX, 0).unwrap()), None);
    }

    #[test]
    fn deadlines_saturate_and() {
        let now = MonotonicInstant::from_nanoseconds(u64::MAX - 10);
        let deadline = now.deadline_after(Duration::from_nanoseconds(20));
        assert_eq!(deadline, Deadline::MAX);
        assert_eq!(
            deadline.remaining_at(MonotonicInstant::from_nanoseconds(u64::MAX - 4)),
            Duration::from_nanoseconds(4)
        );
        assert!(!deadline.has_elapsed_at(MonotonicInstant::from_nanoseconds(u64::MAX - 1)));
        assert!(deadline.has_elapsed_at(MonotonicInstant::from_nanoseconds(u64::MAX)));
        assert_eq!(
            Deadline::from_nanoseconds(10).remaining_at(MonotonicInstant::from_nanoseconds(11)),
            Duration::ZERO
        );
    }

    #[test]
    fn backwards_monotonic_reading() {
        let earlier = MonotonicInstant::from_nanoseconds(20);
        let later = MonotonicInstant::from_nanoseconds(10);
        assert_eq!(later.checked_duration_since(earlier), None);
    }

    #[test]
    fn clock_without_sleep() {
        struct ReadOnly;
        impl MonotonicClock for ReadOnly {
            fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
                Ok(MonotonicInstant::from_nanoseconds(1))
            }
        }
        assert_eq!(
            ReadOnly.sleep_until(Deadline::from_nanoseconds(2)),
            Err(ClockError::NotSupported)
        );
    }
}
