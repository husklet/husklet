use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
pub(crate) struct Activity {
    state: Mutex<ActivityState>,
    idle: Condvar,
    #[cfg(test)]
    observed: Condvar,
}

#[derive(Default)]
struct ActivityState {
    admitted: usize,
    frozen: bool,
    #[cfg(test)]
    freeze_waiting: bool,
    #[cfg(test)]
    admit_waiting: bool,
}

impl Activity {
    pub(crate) fn admit(self: &Arc<Self>) -> ActivityAdmission {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.frozen {
            #[cfg(test)]
            {
                state.admit_waiting = true;
                self.observed.notify_all();
            }
            state = self.idle.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.admitted = state.admitted.saturating_add(1);
        ActivityAdmission(self.clone())
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.frozen = true;
        while state.admitted != 0 {
            #[cfg(test)]
            {
                state.freeze_waiting = true;
                self.observed.notify_all();
            }
            state = self.idle.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(crate) fn thaw(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.frozen = false;
        self.idle.notify_all();
    }

    pub(crate) fn frozen(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frozen
    }

    #[cfg(test)]
    pub(crate) fn wait_until_freeze_waits(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.freeze_waiting {
            state = self
                .observed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_until_admit_waits(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.admit_waiting {
            state = self
                .observed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

pub(crate) struct ActivityAdmission(Arc<Activity>);

impl Drop for ActivityAdmission {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.admitted = state.admitted.saturating_sub(1);
        if state.admitted == 0 {
            self.0.idle.notify_all();
        }
    }
}
