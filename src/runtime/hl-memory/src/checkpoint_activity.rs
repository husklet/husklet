use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Default)]
pub(crate) struct CheckpointActivity {
    state: Mutex<ActivityState>,
    idle: Condvar,
}

#[derive(Debug, Default)]
struct ActivityState {
    admitted: usize,
    frozen: bool,
    terminal: bool,
}

impl CheckpointActivity {
    pub(crate) fn admit(self: &Arc<Self>) -> ActivityAdmission {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.frozen {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        state.admitted = state.admitted.saturating_add(1);
        ActivityAdmission(self.clone())
    }

    pub(crate) fn admit_memory(self: &Arc<Self>) -> Result<ActivityAdmission, crate::MemoryError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.frozen && !state.terminal {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        if state.terminal {
            return Err(crate::MemoryError::NoAddressSpace);
        }
        state.admitted = state.admitted.saturating_add(1);
        Ok(ActivityAdmission(self.clone()))
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.frozen = true;
        while state.admitted != 0 {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
    }

    pub(crate) fn begin_exit(&self) -> Result<(), crate::MemoryError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.terminal {
            return Err(crate::MemoryError::NoAddressSpace);
        }
        if state.frozen {
            return Err(crate::MemoryError::InvariantViolation);
        }
        state.frozen = true;
        while state.admitted != 0 {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        Ok(())
    }

    pub(crate) fn thaw(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.frozen = false;
        self.idle.notify_all();
    }

    pub(crate) fn terminate(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.terminal = true;
        state.frozen = true;
        self.idle.notify_all();
    }

    pub(crate) fn frozen(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).frozen
    }
}

pub(crate) struct ActivityAdmission(Arc<CheckpointActivity>);

impl Drop for ActivityAdmission {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|error| error.into_inner());
        state.admitted = state.admitted.saturating_sub(1);
        // A freezer publishes `frozen` while holding this same mutex before it
        // can wait for the final admission. With no freeze in progress there
        // cannot be a waiter, so notifying on every ordinary memory access
        // only enters the host futex path. This matters for projected native
        // execution, where an admission can be shorter than one guest block.
        if state.admitted == 0 && state.frozen {
            self.0.idle.notify_all();
        }
    }
}
