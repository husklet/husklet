use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

use hl_descriptor::{CancellationNotification, ObjectError, OperationCancellation, StatusFlags};

use super::description::{VfsFileDescription, VfsFileHost};

#[derive(Clone, Copy, Debug)]
pub(super) struct State {
    pub(super) offset: u64,
    pub(super) status: StatusFlags,
    reserved: bool,
}

impl<H: VfsFileHost> VfsFileDescription<H> {
    pub(super) fn lock_state(&self) -> MutexGuard<'_, State> {
        self.cursor.lock()
    }

    pub(super) fn lock_cursor(
        &self,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<MutexGuard<'_, State>, ObjectError> {
        self.cursor
            .acquire(false, cancellation, || self.retired.load(Ordering::Acquire))
    }

    pub(super) fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }
}

pub(super) struct Cursor {
    state: Mutex<State>,
    wait: Condvar,
}

struct Wake(Weak<Cursor>);

impl CancellationNotification for Wake {
    fn notify(&self) {
        if let Some(cursor) = self.0.upgrade() {
            cursor.wake();
        }
    }
}

impl Cursor {
    pub(super) fn new(status: StatusFlags) -> Self {
        Self {
            state: Mutex::new(State {
                offset: 0,
                status,
                reserved: false,
            }),
            wait: Condvar::new(),
        }
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
        retired: impl Fn() -> bool,
    ) -> Result<MutexGuard<'_, State>, ObjectError> {
        let _subscription =
            cancellation.map(|cancellation| cancellation.subscribe(Arc::new(Wake(Arc::downgrade(self)))));
        let mut state = self.lock();
        while state.reserved {
            if nonblocking {
                return Err(ObjectError::WouldBlock);
            }
            if cancellation.is_some_and(OperationCancellation::interrupted) {
                return Err(ObjectError::Interrupted);
            }
            if retired() {
                return Err(ObjectError::Retired);
            }
            state = self.wait.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if cancellation.is_some_and(OperationCancellation::interrupted) {
            return Err(ObjectError::Interrupted);
        }
        if retired() {
            return Err(ObjectError::Retired);
        }
        Ok(state)
    }

    pub(super) fn reserve(
        self: &Arc<Self>,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
        retired: impl Fn() -> bool,
    ) -> Result<u64, ObjectError> {
        let mut state = self.acquire(nonblocking, cancellation, retired)?;
        state.reserved = true;
        Ok(state.offset)
    }

    pub(super) fn commit(&self, start: u64, count: usize) -> Result<(), ObjectError> {
        let mut state = self.lock();
        if !state.reserved || state.offset != start {
            return Err(ObjectError::WouldBlock);
        }
        let offset = state
            .offset
            .checked_add(count as u64)
            .ok_or(ObjectError::InvalidArgument)?;
        state.offset = offset;
        state.reserved = false;
        self.wait.notify_all();
        Ok(())
    }

    pub(super) fn commit_pair(
        first: (&Arc<Self>, u64),
        second: (&Arc<Self>, u64),
        count: usize,
    ) -> Result<(), ObjectError> {
        let (lower, upper) = if Self::address(first.0) < Self::address(second.0) {
            (first, second)
        } else {
            (second, first)
        };
        let mut lower_state = lower.0.lock();
        let mut upper_state = upper.0.lock();
        if !lower_state.reserved
            || lower_state.offset != lower.1
            || !upper_state.reserved
            || upper_state.offset != upper.1
        {
            return Err(ObjectError::WouldBlock);
        }
        let lower_offset = lower_state
            .offset
            .checked_add(count as u64)
            .ok_or(ObjectError::InvalidArgument)?;
        let upper_offset = upper_state
            .offset
            .checked_add(count as u64)
            .ok_or(ObjectError::InvalidArgument)?;
        lower_state.offset = lower_offset;
        lower_state.reserved = false;
        upper_state.offset = upper_offset;
        upper_state.reserved = false;
        drop(upper_state);
        drop(lower_state);
        lower.0.wait.notify_all();
        upper.0.wait.notify_all();
        Ok(())
    }

    pub(super) fn release(&self) {
        let mut state = self.lock();
        state.reserved = false;
        self.wait.notify_all();
    }

    pub(super) fn wake(&self) {
        let _state = self.lock();
        self.wait.notify_all();
    }

    pub(super) fn address(cursor: &Arc<Self>) -> usize {
        Arc::as_ptr(cursor) as usize
    }
}
