use std::sync::{Condvar, Mutex};

use hl_descriptor::{CancellationNotification, ReadinessObserver};

#[derive(Debug, Default)]
pub(crate) struct WaitGate {
    generation: Mutex<u64>,
    wake: Condvar,
}

impl WaitGate {
    pub(crate) fn generation(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn notify_waiters(&self) {
        let mut value = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *value = value.wrapping_add(1);
        drop(value);
        self.wake.notify_all();
    }

    pub(crate) fn wait(&self, observed: u64) {
        let mut value = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *value == observed {
            value = self.wake.wait(value).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl ReadinessObserver for WaitGate {
    fn readiness_changed(&self) {
        self.notify_waiters();
    }
}

impl CancellationNotification for WaitGate {
    fn notify(&self) {
        self.notify_waiters();
    }
}
