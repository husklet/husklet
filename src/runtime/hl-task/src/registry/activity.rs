use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
pub(crate) struct Activity {
    state: Mutex<ActivityState>,
    idle: Condvar,
}

#[derive(Default)]
struct ActivityState {
    admitted: usize,
    frozen: bool,
}

impl Activity {
    pub(crate) fn admit(self: &Arc<Self>) -> ActivityAdmission {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.frozen {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        state.admitted = state.admitted.saturating_add(1);
        ActivityAdmission(self.clone())
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.frozen = true;
        while state.admitted != 0 {
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
}

pub(crate) struct ActivityAdmission(Arc<Activity>);

impl Drop for ActivityAdmission {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|error| error.into_inner());
        state.admitted = state.admitted.saturating_sub(1);
        if state.admitted == 0 {
            self.0.idle.notify_all();
        }
    }
}
