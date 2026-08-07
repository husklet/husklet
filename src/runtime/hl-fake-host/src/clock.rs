use crate::{FakeHost, FakeHostError, Fault};
use hl_time::{ClockError, Deadline, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};
use std::sync::Mutex;

pub struct VirtualClock {
    host: FakeHost,
    monotonic: Mutex<u64>,
    realtime: Mutex<u64>,
}

impl VirtualClock {
    #[must_use]
    pub fn new(host: FakeHost, monotonic: u64, realtime: u64) -> Self {
        Self {
            host,
            monotonic: Mutex::new(monotonic),
            realtime: Mutex::new(realtime),
        }
    }

    fn error(error: FakeHostError) -> ClockError {
        match error {
            FakeHostError::Fault {
                fault: Fault::Interrupted,
                ..
            } => ClockError::Interrupted,
            _ => ClockError::Failed,
        }
    }
}

impl MonotonicClock for VirtualClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        let mut value = self.monotonic.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.host.record("clock", "monotonic", 0, 0, 0).map_err(Self::error)?;
        let current = *value;
        *value = value.saturating_add(1);
        Ok(MonotonicInstant::from_nanoseconds(current))
    }

    fn sleep_until(&self, deadline: Deadline) -> Result<(), ClockError> {
        self.host.record("clock", "sleep", 0, 0, 0).map_err(Self::error)?;
        let mut value = self.monotonic.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *value = (*value).max(deadline.nanoseconds());
        Ok(())
    }
}

impl RealtimeClock for VirtualClock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        let mut value = self.realtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.host.record("clock", "realtime", 0, 0, 0).map_err(Self::error)?;
        let current = *value;
        *value = value.saturating_add(1);
        Ok(Timespec::from_nanoseconds(current))
    }
}
