use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Default)]
pub(crate) struct CheckpointActivity {
    state: Mutex<ActivityState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct ActivityState {
    frozen: bool,
    admitted: usize,
}

impl CheckpointActivity {
    pub(crate) fn admit(self: &Arc<Self>) -> ActivityAdmission {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.frozen {
            state = self.changed.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.admitted += 1;
        ActivityAdmission(self.clone())
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.frozen = true;
        while state.admitted != 0 {
            state = self.changed.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(crate) fn thaw(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.frozen = false;
        self.changed.notify_all();
    }

    pub(crate) fn frozen(&self) -> bool {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).frozen
    }
}

pub(crate) struct ActivityAdmission(Arc<CheckpointActivity>);

impl Drop for ActivityAdmission {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.admitted -= 1;
        if state.admitted == 0 {
            self.0.changed.notify_all();
        }
    }
}
