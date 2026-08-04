use super::TaskRegistry;
use crate::{CancellationEvent, SignalPendingEvent, TaskError, ThreadId, ThreadLifecycle};

impl TaskRegistry {
    pub fn request_cancellation(&self, thread: ThreadId) -> Result<CancellationEvent, TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let thread_state = Self::thread_mut(&mut state, thread)?;
        if !matches!(
            thread_state.lifecycle,
            ThreadLifecycle::Runnable | ThreadLifecycle::Blocked
        ) {
            return Err(TaskError::InvalidLifecycle);
        }
        thread_state.cancellation_pending = true;
        drop(state);
        self.publish_interrupt(thread, true);
        Ok(CancellationEvent::new(thread))
    }

    pub fn set_signal_pending(&self, thread: ThreadId, pending: bool) -> Result<SignalPendingEvent, TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let thread_state = Self::thread_mut(&mut state, thread)?;
        if !matches!(
            thread_state.lifecycle,
            ThreadLifecycle::Runnable | ThreadLifecycle::Blocked
        ) {
            return Err(TaskError::InvalidLifecycle);
        }
        thread_state.signal_pending = pending;
        drop(state);
        if pending {
            self.publish_interrupt(thread, true);
        } else {
            let _ = self.acknowledge_interrupt(thread);
        }
        Ok(SignalPendingEvent::new(thread, pending))
    }

    pub fn clear_cancellation(&self, thread: ThreadId) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        Self::thread_mut(&mut state, thread)?.cancellation_pending = false;
        drop(state);
        self.acknowledge_interrupt(thread).map(|_| ())
    }
}
