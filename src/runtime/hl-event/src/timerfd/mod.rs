use std::sync::{Arc, Condvar, Mutex};

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver, ReadinessRegistry,
    ReadinessSubscription, StatusFlags,
};
use hl_time::{ClockError, Duration};

mod prepared;
mod wake;
pub use prepared::PreparedTimerRead;
pub use wake::TimerClockSource;

const TIMERFD_MODE: u32 = 0o100_600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum TimerFdClock {
    Realtime = 0,
    Monotonic = 1,
    Boottime = 7,
    RealtimeAlarm = 8,
    BoottimeAlarm = 9,
}

impl TimerFdClock {
    #[must_use]
    pub const fn from_linux_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Realtime,
            1 => Self::Monotonic,
            7 => Self::Boottime,
            8 => Self::RealtimeAlarm,
            9 => Self::BoottimeAlarm,
            _ => return None,
        })
    }

    const fn is_realtime(self) -> bool {
        matches!(self, Self::Realtime | Self::RealtimeAlarm)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct CreateFlags(u32);

impl CreateFlags {
    pub const NONBLOCKING: u32 = 0x800;
    pub const CLOSE_ON_EXEC: u32 = 0x8_0000;
    const ALLOWED: u32 = Self::NONBLOCKING | Self::CLOSE_ON_EXEC;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn closes_on_exec(self) -> bool {
        self.0 & Self::CLOSE_ON_EXEC != 0
    }

    const fn valid(self) -> bool {
        self.0 & !Self::ALLOWED == 0
    }

    const fn nonblocking(self) -> bool {
        self.0 & Self::NONBLOCKING != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct SetFlags(u32);

impl SetFlags {
    pub const ABSOLUTE: u32 = 1;
    pub const CANCEL_ON_SET: u32 = 2;
    const ALLOWED: u32 = Self::ABSOLUTE | Self::CANCEL_ON_SET;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    const fn valid(self) -> bool {
        self.0 & !Self::ALLOWED == 0
    }

    const fn absolute(self) -> bool {
        self.0 & Self::ABSOLUTE != 0
    }

    const fn cancel_on_set(self) -> bool {
        self.0 & Self::CANCEL_ON_SET != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TimerSetting {
    pub interval: Duration,
    pub value: Duration,
}

pub type TimerFdStatus = crate::EventStatus;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerFdError {
    InvalidArgument,
    WouldBlock,
    Canceled,
    Retired,
    Clock(ClockError),
}

impl TimerFdError {
    pub(crate) const fn object_error(self) -> ObjectError {
        match self {
            Self::InvalidArgument => ObjectError::InvalidArgument,
            Self::WouldBlock => ObjectError::WouldBlock,
            Self::Canceled => ObjectError::Canceled,
            Self::Retired => ObjectError::Retired,
            Self::Clock(ClockError::Interrupted) => ObjectError::Interrupted,
            Self::Clock(ClockError::NotSupported) => ObjectError::NotSupported,
            Self::Clock(ClockError::Failed) => ObjectError::Io,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimerBasis {
    Monotonic,
    Realtime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerFdSnapshot {
    pub clock: TimerFdClock,
    pub deadline_nanoseconds: Option<u64>,
    pub interval_nanoseconds: u64,
    pub pending_expirations: u64,
    pub nonblocking: bool,
    pub absolute_realtime: bool,
    pub cancel_generation: Option<u64>,
    pub canceled: bool,
}

#[derive(Clone)]
struct TimerState {
    deadline: Option<u64>,
    interval: u64,
    pending: u64,
    nonblocking: bool,
    basis: TimerBasis,
    cancel_generation: Option<u64>,
    canceled: bool,
    retired: bool,
    wake: Option<u64>,
}

struct TimerInner {
    clock: TimerFdClock,
    source: Arc<dyn TimerClockSource>,
    state: Mutex<TimerState>,
    changed: Condvar,
    readiness: ReadinessRegistry,
}

/// A Linux timerfd open-file-description object.
#[derive(Clone)]
pub struct TimerFd {
    inner: Arc<TimerInner>,
}

impl std::fmt::Debug for TimerFd {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TimerFd")
            .field("clock", &self.inner.clock)
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl TimerFd {
    pub fn new(
        clock: TimerFdClock,
        flags: CreateFlags,
        source: Arc<dyn TimerClockSource>,
    ) -> Result<Self, TimerFdError> {
        if !flags.valid() {
            return Err(TimerFdError::InvalidArgument);
        }
        Ok(Self {
            inner: Arc::new(TimerInner {
                clock,
                source,
                state: Mutex::new(TimerState {
                    deadline: None,
                    interval: 0,
                    pending: 0,
                    nonblocking: flags.nonblocking(),
                    basis: TimerBasis::Monotonic,
                    cancel_generation: None,
                    canceled: false,
                    retired: false,
                    wake: None,
                }),
                changed: Condvar::new(),
                readiness: ReadinessRegistry::new(),
            }),
        })
    }

    pub fn from_snapshot(snapshot: TimerFdSnapshot, source: Arc<dyn TimerClockSource>) -> Result<Self, TimerFdError> {
        if snapshot.absolute_realtime && !snapshot.clock.is_realtime() {
            return Err(TimerFdError::InvalidArgument);
        }
        Ok(Self {
            inner: Arc::new(TimerInner {
                clock: snapshot.clock,
                source,
                state: Mutex::new(TimerState {
                    deadline: snapshot.deadline_nanoseconds,
                    interval: snapshot.interval_nanoseconds,
                    pending: snapshot.pending_expirations,
                    nonblocking: snapshot.nonblocking,
                    basis: if snapshot.absolute_realtime {
                        TimerBasis::Realtime
                    } else {
                        TimerBasis::Monotonic
                    },
                    cancel_generation: snapshot.cancel_generation,
                    canceled: snapshot.canceled,
                    retired: false,
                    wake: None,
                }),
                changed: Condvar::new(),
                readiness: ReadinessRegistry::new(),
            }),
        })
    }
    pub fn set_time(&self, flags: SetFlags, setting: TimerSetting) -> Result<TimerSetting, TimerFdError> {
        if !flags.valid() {
            return Err(TimerFdError::InvalidArgument);
        }
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh(&mut state)?;
        let previous = self.current_setting(&state)?;
        let original = state.clone();
        state.pending = 0;
        state.canceled = false;
        state.cancel_generation = None;
        if setting.value == Duration::ZERO {
            if let Some(token) = state.wake.take() {
                self.inner.source.cancel_wake(token);
            }
            state.deadline = None;
            state.interval = 0;
            self.inner.changed.notify_all();
            drop(state);
            self.inner.readiness.notify();
            return Ok(previous);
        }
        state.basis = if flags.absolute() && self.inner.clock.is_realtime() {
            TimerBasis::Realtime
        } else {
            TimerBasis::Monotonic
        };
        let value = setting.value.nanoseconds();
        state.deadline = Some(if flags.absolute() {
            value
        } else {
            self.monotonic_now()?
                .checked_add(value)
                .ok_or(TimerFdError::InvalidArgument)?
        });
        state.interval = setting.interval.nanoseconds();
        if flags.absolute() && flags.cancel_on_set() && self.inner.clock.is_realtime() {
            state.cancel_generation = Some(self.inner.source.realtime_generation());
        }
        let wake = self.wake_deadline(&state)?;
        let token = match self.arm(wake) {
            Ok(token) => token,
            Err(error) => {
                *state = original;
                return Err(error);
            }
        };
        if let Some(previous) = state.wake.replace(token) {
            self.inner.source.cancel_wake(previous);
        }
        self.inner.changed.notify_all();
        drop(state);
        self.inner.readiness.notify();
        Ok(previous)
    }
    pub fn get_time(&self) -> Result<TimerSetting, TimerFdError> {
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh(&mut state)?;
        self.current_setting(&state)
    }
    pub fn read(&self, output: &mut [u8]) -> Result<usize, TimerFdError> {
        if output.len() < size_of::<u64>() {
            return Err(TimerFdError::InvalidArgument);
        }
        let prepared = self.prepare_read()?;
        output[..8].copy_from_slice(&prepared.bytes());
        self.commit_read(prepared)?;
        Ok(8)
    }
    #[must_use]
    pub fn readiness(&self, interests: Readiness) -> Readiness {
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ready = match self.refresh(&mut state) {
            Ok(()) if state.retired => Readiness::ERROR,
            Ok(()) if state.pending != 0 || state.canceled => Readiness::READ,
            Ok(()) => 0,
            Err(_) => Readiness::ERROR,
        };
        Readiness::from_bits(ready & (interests.bits() | Readiness::ERROR | Readiness::HANGUP))
    }
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), TimerFdError> {
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(TimerFdError::Retired);
        }
        state.nonblocking = nonblocking;
        self.inner.changed.notify_all();
        Ok(())
    }
    #[must_use]
    pub const fn status(&self) -> TimerFdStatus {
        TimerFdStatus {
            mode: TIMERFD_MODE,
            size: 0,
            link_count: 1,
        }
    }
    #[must_use]
    pub fn snapshot(&self) -> Result<TimerFdSnapshot, TimerFdError> {
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.refresh(&mut state)?;
        Ok(TimerFdSnapshot {
            clock: self.inner.clock,
            deadline_nanoseconds: state.deadline,
            interval_nanoseconds: state.interval,
            pending_expirations: state.pending,
            nonblocking: state.nonblocking,
            absolute_realtime: state.basis == TimerBasis::Realtime,
            cancel_generation: state.cancel_generation,
            canceled: state.canceled,
        })
    }
    fn refresh(&self, state: &mut TimerState) -> Result<(), TimerFdError> {
        if let Some(generation) = state.cancel_generation
            && generation != self.inner.source.realtime_generation()
        {
            state.canceled = true;
            state.deadline = None;
            state.interval = 0;
            if let Some(token) = state.wake.take() {
                self.inner.source.cancel_wake(token);
            }
            return Ok(());
        }
        let Some(deadline) = state.deadline else {
            return Ok(());
        };
        let now = self.now(state.basis)?;
        if now < deadline {
            return Ok(());
        }
        if state.interval == 0 {
            state.pending = state.pending.saturating_add(1);
            state.deadline = None;
            state.wake = None;
            return Ok(());
        }
        let elapsed = 1 + (now - deadline) / state.interval;
        state.pending = state.pending.saturating_add(elapsed);
        state.deadline = Some(deadline.saturating_add(elapsed.saturating_mul(state.interval)));
        let wake = self.wake_deadline(state)?;
        let token = self.arm(wake)?;
        if let Some(previous) = state.wake.replace(token) {
            self.inner.source.cancel_wake(previous);
        }
        Ok(())
    }

    fn current_setting(&self, state: &TimerState) -> Result<TimerSetting, TimerFdError> {
        let remaining = match state.deadline {
            Some(deadline) => deadline.saturating_sub(self.now(state.basis)?),
            None => 0,
        };
        Ok(TimerSetting {
            interval: Duration::from_nanoseconds(state.interval),
            value: Duration::from_nanoseconds(remaining),
        })
    }

    fn now(&self, basis: TimerBasis) -> Result<u64, TimerFdError> {
        match basis {
            TimerBasis::Monotonic => self.monotonic_now(),
            TimerBasis::Realtime => self
                .inner
                .source
                .realtime_now()
                .map_err(TimerFdError::Clock)?
                .checked_nanoseconds()
                .ok_or(TimerFdError::InvalidArgument),
        }
    }

    fn monotonic_now(&self) -> Result<u64, TimerFdError> {
        self.inner
            .source
            .monotonic_now()
            .map(hl_time::MonotonicInstant::nanoseconds)
            .map_err(TimerFdError::Clock)
    }

    fn retire_inner(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return;
        }
        state.retired = true;
        if let Some(token) = state.wake.take() {
            self.inner.source.cancel_wake(token);
        }
        self.inner.changed.notify_all();
        drop(state);
        self.inner.readiness.notify();
        self.inner.readiness.close();
    }
}

impl OpenFileDescription for TimerFd {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Event
    }

    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        Ok(self.status().metadata())
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        TimerFd::read(self, output).map_err(TimerFdError::object_error)
    }

    fn prepare_atomic_read(
        &self,
        maximum: usize,
    ) -> Result<Option<Box<dyn hl_descriptor::PreparedAtomicRead>>, ObjectError> {
        prepared::AtomicRead::prepare(self, maximum)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.set_nonblocking(flags.bits() & StatusFlags::NONBLOCKING != 0)
            .map_err(TimerFdError::object_error)
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        TimerFd::readiness(self, interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.inner.readiness.subscribe(observer)
    }

    fn retire(&self) {
        self.retire_inner();
    }

    fn close(&self) {
        self.retire_inner();
    }
}
