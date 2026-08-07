use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
pub(crate) struct CheckpointActivity(Arc<Activity>);

#[derive(Default)]
struct Activity {
    state: Mutex<ActivityState>,
    changed: Condvar,
}

#[derive(Default)]
struct ActivityState {
    frozen: bool,
    admitted: usize,
}

pub(crate) struct Admission(Arc<Activity>);

impl CheckpointActivity {
    pub(crate) fn admit(&self) -> Admission {
        let mut state = self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.frozen {
            state = self
                .0
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.admitted += 1;
        Admission(self.0.clone())
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.frozen = true;
        while state.admitted != 0 {
            state = self
                .0
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(crate) fn thaw(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frozen = false;
        self.0.changed.notify_all();
    }

    pub(crate) fn frozen(&self) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frozen
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.admitted -= 1;
        if state.admitted == 0 {
            self.0.changed.notify_all();
        }
    }
}
