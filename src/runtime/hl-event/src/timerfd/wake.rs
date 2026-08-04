use std::sync::Arc;

use hl_time::{Clock, ClockError};

use super::{TimerBasis, TimerFd, TimerFdError, TimerState};

/// Injectable timerfd clock and bounded deadline scheduler.
pub trait TimerClockSource: Clock + Send + Sync {
    /// Monotonically increasing version changed by discontinuous realtime sets.
    fn realtime_generation(&self) -> u64;

    fn schedule_callback(
        &self,
        deadline_nanoseconds: u64,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<u64, ClockError>;

    fn cancel_wake(&self, _token: u64) {}
}

impl TimerFd {
    pub(super) fn arm(&self, deadline: u64) -> Result<u64, TimerFdError> {
        let inner = Arc::downgrade(&self.inner);
        self.inner
            .source
            .schedule_callback(
                deadline,
                Arc::new(move || {
                    let Some(inner) = inner.upgrade() else { return };
                    inner.changed.notify_all();
                    inner.readiness.notify();
                }),
            )
            .map_err(TimerFdError::Clock)
    }

    pub fn notify_clock_changed(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        if state
            .cancel_generation
            .is_some_and(|generation| generation != self.inner.source.realtime_generation())
        {
            state.canceled = true;
            state.deadline = None;
            state.interval = 0;
            if let Some(token) = state.wake.take() {
                self.inner.source.cancel_wake(token);
            }
        }
        drop(state);
        self.inner.changed.notify_all();
        self.inner.readiness.notify();
    }

    pub(super) fn wake_deadline(&self, state: &TimerState) -> Result<u64, TimerFdError> {
        let deadline = state.deadline.ok_or(TimerFdError::InvalidArgument)?;
        if state.basis == TimerBasis::Monotonic {
            return Ok(deadline);
        }
        let remaining = deadline.saturating_sub(self.now(TimerBasis::Realtime)?);
        self.monotonic_now()?
            .checked_add(remaining)
            .ok_or(TimerFdError::InvalidArgument)
    }
}
