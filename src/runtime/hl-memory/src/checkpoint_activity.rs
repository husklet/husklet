use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Default)]
pub(crate) struct CheckpointActivity {
    state: Mutex<ActivityState>,
    idle: Condvar,
    requests: AtomicU64,
}

#[derive(Debug, Default)]
struct ActivityState {
    admitted: usize,
    frozen: bool,
    terminal: bool,
}

impl CheckpointActivity {
    fn invalidate_continuations(&self) {
        let _ = self
            .requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |requests| {
                Some(requests.saturating_add(1))
            });
    }

    pub(crate) fn admit(self: &Arc<Self>) -> ActivityAdmission {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.frozen {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        state.admitted = state.admitted.saturating_add(1);
        ActivityAdmission {
            activity: self.clone(),
            requests: self.requests.load(Ordering::Acquire),
        }
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
        Ok(ActivityAdmission {
            activity: self.clone(),
            requests: self.requests.load(Ordering::Acquire),
        })
    }

    pub(crate) fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        self.invalidate_continuations();
        state.frozen = true;
        while state.admitted != 0 {
            state = self.idle.wait(state).unwrap_or_else(|error| error.into_inner());
        }
    }

    pub(crate) fn begin_exit(&self) -> Result<(), crate::MemoryError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        self.invalidate_continuations();
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
        self.invalidate_continuations();
        state.terminal = true;
        state.frozen = true;
        self.idle.notify_all();
    }

    pub(crate) fn frozen(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).frozen
    }
}

/// Lock-free evidence that no checkpoint or address-space exit was requested
/// after the memory activity carrying this token was admitted.
///
/// A current token is necessary, but not sufficient, authority to continue a
/// native execution quantum. Mapping transitions and scheduler events have
/// independent ownership and must be checked separately.
#[must_use = "a continuation token must be checked before extending memory activity"]
#[derive(Clone, Debug)]
pub struct CheckpointContinuation {
    activity: Arc<CheckpointActivity>,
    requests: u64,
}

impl CheckpointContinuation {
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.requests != u64::MAX && self.activity.requests.load(Ordering::Acquire) == self.requests
    }
}

pub(crate) struct ActivityAdmission {
    activity: Arc<CheckpointActivity>,
    requests: u64,
}

impl ActivityAdmission {
    pub(crate) fn continuation(&self) -> CheckpointContinuation {
        CheckpointContinuation {
            activity: Arc::clone(&self.activity),
            requests: self.requests,
        }
    }
}

impl Drop for ActivityAdmission {
    fn drop(&mut self) {
        let mut state = self.activity.state.lock().unwrap_or_else(|error| error.into_inner());
        state.admitted = state.admitted.saturating_sub(1);
        // A freezer publishes `frozen` while holding this same mutex before it
        // can wait for the final admission. With no freeze in progress there
        // cannot be a waiter, so notifying on every ordinary memory access
        // only enters the host futex path. This matters for projected native
        // execution, where an admission can be shorter than one guest block.
        if state.admitted == 0 && state.frozen {
            self.activity.idle.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::CheckpointActivity;

    #[test]
    fn freeze_invalidates_continuation_before_waiting_for_admission() {
        let activity = Arc::new(CheckpointActivity::default());
        let admission = activity.admit_memory().unwrap();
        let continuation = admission.continuation();
        let (finished, completion) = mpsc::channel();
        let freezer = Arc::clone(&activity);
        let thread = std::thread::spawn(move || {
            freezer.freeze();
            finished.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while continuation.is_current() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!continuation.is_current());
        assert!(completion.try_recv().is_err());

        drop(admission);
        completion.recv_timeout(Duration::from_secs(1)).unwrap();
        activity.thaw();
        assert!(!continuation.is_current());
        thread.join().unwrap();
    }

    #[test]
    fn exit_and_termination_invalidate_existing_continuations() {
        let exiting = Arc::new(CheckpointActivity::default());
        let admission = exiting.admit_memory().unwrap();
        let continuation = admission.continuation();
        drop(admission);
        exiting.begin_exit().unwrap();
        assert!(!continuation.is_current());

        let terminal = Arc::new(CheckpointActivity::default());
        let admission = terminal.admit_memory().unwrap();
        let continuation = admission.continuation();
        terminal.terminate();
        assert!(!continuation.is_current());
        drop(admission);
    }

    #[test]
    fn saturated_request_epoch_permanently_denies_continuation() {
        let activity = Arc::new(CheckpointActivity::default());
        activity.requests.store(u64::MAX, Ordering::Release);
        let admission = activity.admit_memory().unwrap();
        let continuation = admission.continuation();
        assert!(!continuation.is_current());
        drop(admission);
        activity.freeze();
        activity.thaw();
        assert!(!continuation.is_current());
    }
}
