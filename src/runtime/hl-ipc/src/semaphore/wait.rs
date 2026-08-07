use std::sync::atomic::Ordering;

use hl_sync::{Interruption, WaitOutcome};
use hl_time::{Deadline, MonotonicClock};

use crate::{Credentials, IPC_NOWAIT, SemaphoreError, SemaphoreId, SemaphoreNamespace, SemaphoreOperation};

use super::{Set, State};

#[derive(Clone, Copy, Debug)]
enum Blocked {
    Decrement(usize),
    Zero(usize),
}

impl SemaphoreNamespace {
    pub fn operate_wait<C: MonotonicClock + ?Sized>(
        &self,
        id: SemaphoreId,
        actor: Credentials,
        pid: u32,
        operations: &[SemaphoreOperation],
        interruption: &Interruption,
        deadline: Option<Deadline>,
        clock: &C,
        now: u64,
    ) -> Result<(), SemaphoreError> {
        let mut waited = false;
        loop {
            let observed = self.changed.observation();
            let blocked = {
                let mut state = self.lock();
                let set = match Self::set(&state, id) {
                    Ok(value) => value,
                    Err(SemaphoreError::NotFound) if waited => return Err(SemaphoreError::Removed),
                    Err(error) => return Err(error),
                };
                Self::require(&set.metadata, actor, 0o2)?;
                match self.evaluate(&state, id, operations) {
                    Ok(values) => {
                        self.commit(&mut state, id, pid, operations, values, now)?;
                        drop(state);
                        self.changed.notify_all();
                        return Ok(());
                    }
                    Err(SemaphoreError::Again) => self.register(&mut state, id, operations)?,
                    Err(error) => return Err(error),
                }
            };
            let result = self.wait(observed, interruption, deadline, clock);
            self.unregister(id, blocked);
            result?;
            waited = true;
        }
    }

    #[allow(clippy::unused_self)]
    fn register(
        &self,
        state: &mut State,
        id: SemaphoreId,
        operations: &[SemaphoreOperation],
    ) -> Result<Blocked, SemaphoreError> {
        let set = Self::set_mut(state, id)?;
        let blocked = Self::first_blocked(set, operations)?;
        match blocked {
            Blocked::Decrement(index) => set.decrement_waiters[index] += 1,
            Blocked::Zero(index) => set.zero_waiters[index] += 1,
        }
        Ok(blocked)
    }

    fn first_blocked(set: &Set, operations: &[SemaphoreOperation]) -> Result<Blocked, SemaphoreError> {
        let mut values = set.values.clone();
        for operation in operations {
            let index = operation.index as usize;
            let value = values.get_mut(index).ok_or(SemaphoreError::Range)?;
            let blocked = if operation.delta == 0 && *value != 0 {
                Some(Blocked::Zero(index))
            } else if operation.delta < 0 && i32::from(*value) < -operation.delta {
                Some(Blocked::Decrement(index))
            } else {
                None
            };
            if blocked.is_some() && operation.flags & IPC_NOWAIT != 0 {
                return Err(SemaphoreError::Again);
            }
            if let Some(blocked) = blocked {
                return Ok(blocked);
            }
            if operation.delta != 0 {
                *value = (i32::from(*value) + operation.delta) as u16;
            }
        }
        Err(SemaphoreError::Again)
    }

    fn unregister(&self, id: SemaphoreId, blocked: Blocked) {
        let mut state = self.lock();
        let Ok(set) = Self::set_mut(&mut state, id) else {
            return;
        };
        match blocked {
            Blocked::Decrement(index) => set.decrement_waiters[index] -= 1,
            Blocked::Zero(index) => set.zero_waiters[index] -= 1,
        }
    }

    fn wait<C: MonotonicClock + ?Sized>(
        &self,
        observed: u64,
        interruption: &Interruption,
        deadline: Option<Deadline>,
        clock: &C,
    ) -> Result<(), SemaphoreError> {
        self.waiters.fetch_add(1, Ordering::AcqRel);
        let outcome = self
            .changed
            .wait(observed, interruption, deadline, clock)
            .map_err(|_| SemaphoreError::Clock);
        self.waiters.fetch_sub(1, Ordering::AcqRel);
        match outcome? {
            WaitOutcome::Notified => Ok(()),
            WaitOutcome::Interrupted => Err(SemaphoreError::Interrupted),
            WaitOutcome::TimedOut => Err(SemaphoreError::TimedOut),
        }
    }

    pub(crate) fn checkpoint_waiters(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn active_waiters(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }
}
