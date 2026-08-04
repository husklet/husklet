use std::sync::{Arc, Weak};

use hl_descriptor::{CancellationNotification, OperationCancellation};

use super::{COUNTER_MAX, EventFd, EventFdError, EventFdInner};

struct Notification(Weak<EventFdInner>);

impl CancellationNotification for Notification {
    fn notify(&self) {
        if let Some(inner) = self.0.upgrade() {
            let state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
            drop(state);
            inner.changed.notify_all();
        }
    }
}

impl EventFd {
    pub fn read(&self, output: &mut [u8]) -> Result<usize, EventFdError> {
        self.read_context(output, None)
    }

    pub(super) fn read_context(
        &self,
        output: &mut [u8],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, EventFdError> {
        if output.len() < size_of::<u64>() {
            return Err(EventFdError::InvalidArgument);
        }
        let _subscription = cancellation
            .map(|cancellation| cancellation.subscribe(Arc::new(Notification(Arc::downgrade(&self.inner)))));
        let (value, notifications) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            while state.counter == 0 {
                if state.retired {
                    return Err(EventFdError::Retired);
                }
                if state.nonblocking {
                    return Err(EventFdError::WouldBlock);
                }
                if cancellation.is_some_and(OperationCancellation::interrupted) {
                    return Err(EventFdError::Interrupted);
                }
                state = self
                    .inner
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            let value = if state.semaphore {
                state.counter -= 1;
                1
            } else {
                std::mem::take(&mut state.counter)
            };
            let notifications = state.subscriptions.values().cloned().collect::<Vec<_>>();
            self.inner.changed.notify_all();
            (value, notifications)
        };
        output[..8].copy_from_slice(&value.to_ne_bytes());
        Self::notify(notifications);
        self.inner.readiness.notify();
        Ok(8)
    }

    pub fn write(&self, input: &[u8]) -> Result<usize, EventFdError> {
        self.write_context(input, None)
    }

    pub(super) fn write_context(
        &self,
        input: &[u8],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, EventFdError> {
        let bytes: [u8; 8] = input.try_into().map_err(|_| EventFdError::InvalidArgument)?;
        let value = u64::from_ne_bytes(bytes);
        if value == u64::MAX {
            return Err(EventFdError::InvalidArgument);
        }
        let _subscription = cancellation
            .map(|cancellation| cancellation.subscribe(Arc::new(Notification(Arc::downgrade(&self.inner)))));
        let notifications = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            while value > COUNTER_MAX - state.counter {
                if state.retired {
                    return Err(EventFdError::Retired);
                }
                if state.nonblocking {
                    return Err(EventFdError::WouldBlock);
                }
                if cancellation.is_some_and(OperationCancellation::interrupted) {
                    return Err(EventFdError::Interrupted);
                }
                state = self
                    .inner
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            if state.retired {
                return Err(EventFdError::Retired);
            }
            if value == 0 {
                return Ok(8);
            }
            state.counter += value;
            let notifications = state.subscriptions.values().cloned().collect::<Vec<_>>();
            self.inner.changed.notify_all();
            notifications
        };
        Self::notify(notifications);
        self.inner.readiness.notify();
        Ok(8)
    }
}
