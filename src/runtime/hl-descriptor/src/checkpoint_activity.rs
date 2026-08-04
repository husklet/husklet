use std::sync::{Arc, Condvar, Mutex};

use crate::DescriptorError;

#[derive(Debug, Default)]
pub(crate) struct CheckpointActivity {
    state: Mutex<ActivityState>,
    idle: Condvar,
}

#[derive(Debug, Default)]
struct ActivityState {
    active: usize,
    frozen: bool,
}

impl CheckpointActivity {
    pub(crate) fn admit(&self) -> Result<(), DescriptorError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.frozen {
            return Err(DescriptorError::CheckpointFrozen);
        }
        state.active = state.active.checked_add(1).ok_or(DescriptorError::Corrupt)?;
        Ok(())
    }

    pub(crate) fn release(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            self.idle.notify_all();
        }
    }

    pub(crate) fn retain(&self) -> Result<(), DescriptorError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.active == 0 {
            return Err(DescriptorError::Corrupt);
        }
        state.active = state.active.checked_add(1).ok_or(DescriptorError::Corrupt)?;
        Ok(())
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.frozen = true;
        while state.active != 0 {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
    }

    pub(crate) fn thaw(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.frozen = false;
        self.idle.notify_all();
    }

    pub(crate) fn frozen(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).frozen
    }

    pub(crate) fn operation(self: &Arc<Self>) -> Result<CheckpointAdmission, DescriptorError> {
        self.admit()?;
        Ok(CheckpointAdmission(self.clone()))
    }

    pub(crate) fn operation_wait(self: &Arc<Self>) -> CheckpointAdmission {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.frozen {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        state.active = state.active.saturating_add(1);
        CheckpointAdmission(self.clone())
    }
}

#[derive(Debug)]
pub(crate) struct CheckpointAdmission(Arc<CheckpointActivity>);

impl Drop for CheckpointAdmission {
    fn drop(&mut self) {
        self.0.release();
    }
}
