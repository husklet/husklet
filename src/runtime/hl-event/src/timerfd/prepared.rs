use super::{TimerFd, TimerFdError};
use hl_descriptor::{ObjectError, PreparedAtomicRead};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedTimerRead {
    pub(super) expirations: u64,
}

impl PreparedTimerRead {
    #[must_use]
    pub const fn bytes(self) -> [u8; 8] {
        self.expirations.to_ne_bytes()
    }
}

impl TimerFd {
    fn wait_for_change<'a>(
        &self,
        state: std::sync::MutexGuard<'a, super::TimerState>,
    ) -> Result<std::sync::MutexGuard<'a, super::TimerState>, TimerFdError> {
        let Some(deadline) = state.deadline else {
            return Ok(self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner));
        };
        let remaining = deadline.saturating_sub(self.now(state.basis)?);
        let duration = std::time::Duration::from_nanos(remaining);
        Ok(self
            .inner
            .changed
            .wait_timeout(state, duration)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0)
    }

    pub fn prepare_read(&self) -> Result<PreparedTimerRead, TimerFdError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            self.refresh(&mut state)?;
            if state.retired {
                return Err(TimerFdError::Retired);
            }
            if state.canceled {
                return Err(TimerFdError::Canceled);
            }
            if state.pending != 0 {
                return Ok(PreparedTimerRead {
                    expirations: state.pending,
                });
            }
            if state.nonblocking {
                return Err(TimerFdError::WouldBlock);
            }
            state = self.wait_for_change(state)?;
        }
    }

    pub fn commit_read(&self, prepared: PreparedTimerRead) -> Result<(), TimerFdError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(TimerFdError::Retired);
        }
        if state.pending < prepared.expirations {
            return Err(TimerFdError::WouldBlock);
        }
        state.pending -= prepared.expirations;
        drop(state);
        self.inner.readiness.notify();
        Ok(())
    }
}

pub(super) struct AtomicTimerRead {
    timer: TimerFd,
    prepared: PreparedTimerRead,
    bytes: [u8; 8],
}

impl AtomicTimerRead {
    pub(super) fn prepare(timer: &TimerFd, maximum: usize) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        if maximum < 8 {
            return Err(ObjectError::InvalidArgument);
        }
        let prepared = timer.prepare_read().map_err(TimerFdError::object_error)?;
        Ok(Some(Box::new(Self {
            timer: timer.clone(),
            bytes: prepared.bytes(),
            prepared,
        })))
    }
}

impl PreparedAtomicRead for AtomicTimerRead {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn commit(self: Box<Self>) -> Result<(), ObjectError> {
        self.timer
            .commit_read(self.prepared)
            .map_err(TimerFdError::object_error)
    }
}

pub(super) use AtomicTimerRead as AtomicRead;
