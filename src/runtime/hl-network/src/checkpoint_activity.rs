use std::sync::{Condvar, Mutex};

#[derive(Default)]
pub(crate) struct CheckpointActivity {
    state: Mutex<ActivityState>,
    changed: Condvar,
}

#[derive(Default)]
struct ActivityState {
    frozen: bool,
    admitted: usize,
}

pub(crate) struct Admission<'activity> {
    activity: &'activity CheckpointActivity,
}

impl CheckpointActivity {
    pub(crate) fn admit(&self) -> Admission<'_> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.frozen {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.admitted += 1;
        Admission { activity: self }
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.frozen = true;
        while state.admitted != 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(crate) fn thaw(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frozen = false;
        self.changed.notify_all();
    }

    pub(crate) fn frozen(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frozen
    }
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        let mut state = self
            .activity
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.admitted -= 1;
        if state.admitted == 0 {
            self.activity.changed.notify_all();
        }
    }
}
