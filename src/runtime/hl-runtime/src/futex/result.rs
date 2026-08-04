use hl_linux::{Errno, FutexPlan, LinuxResult};
use hl_memory::MemoryAccessHost;
use hl_sync::{FutexClock, FutexDeadline, FutexError, PiFutexError};
use hl_time::Timespec;

use super::SafeRuntimeFutex;

impl<H: MemoryAccessHost + 'static> SafeRuntimeFutex<H> {
    pub(super) fn deadline(&self, plan: FutexPlan) -> Result<Option<FutexDeadline>, FutexError> {
        let Some(deadline) = plan.deadline else { return Ok(None) };
        if plan.timeout_absolute {
            return Ok(Some(deadline));
        }
        let now = match deadline.clock {
            FutexClock::Monotonic => Timespec::from_nanoseconds(
                self.clock
                    .monotonic_now()
                    .map_err(|_| FutexError::ClockFailed)?
                    .nanoseconds(),
            ),
            FutexClock::Realtime => self.clock.realtime_now().map_err(|_| FutexError::ClockFailed)?,
        };
        Ok(Some(FutexDeadline {
            clock: deadline.clock,
            value: now.checked_add(deadline.value).ok_or(FutexError::InvalidArgument)?,
        }))
    }

    pub(super) fn result(result: Result<usize, FutexError>) -> LinuxResult {
        match result {
            Ok(value) => LinuxResult::Value(value as u64),
            Err(error) => LinuxResult::Error(Self::errno(error)),
        }
    }

    pub(super) const fn errno(error: FutexError) -> Errno {
        match error {
            FutexError::InvalidArgument => Errno::EINVAL,
            FutexError::ValueMismatch | FutexError::CompareMismatch => Errno::EAGAIN,
            FutexError::Fault => Errno::EFAULT,
            FutexError::ResourceLimit => Errno::ENOMEM,
            FutexError::ClockFailed => Errno::EIO,
        }
    }

    pub(super) const fn pi_errno(error: PiFutexError) -> Errno {
        match error {
            PiFutexError::Deadlock => Errno::EDEADLK,
            PiFutexError::Permission => Errno::EPERM,
            PiFutexError::WouldBlock => Errno::EAGAIN,
            PiFutexError::Fault => Errno::EFAULT,
            PiFutexError::ClockFailed => Errno::EIO,
            PiFutexError::ResourceLimit => Errno::ENOMEM,
        }
    }

    pub(super) fn wake_predicate(encoded: u32) -> impl FnOnce(i32) -> bool {
        let comparison = (encoded >> 24) & 15;
        let argument = ((encoded << 20) as i32) >> 20;
        move |old| match comparison {
            0 => old == argument,
            1 => old != argument,
            2 => old < argument,
            3 => old <= argument,
            4 => old > argument,
            5 => old >= argument,
            _ => false,
        }
    }
}
