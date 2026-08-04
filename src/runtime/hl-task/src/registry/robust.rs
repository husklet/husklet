use super::TaskRegistry;
use crate::{RobustListRegistration, TaskError, ThreadId};

impl TaskRegistry {
    pub fn set_robust_list(&self, thread: ThreadId, registration: RobustListRegistration) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        Self::thread_mut(&mut state, thread)?.robust_list = Some(registration);
        Ok(())
    }

    pub fn robust_list(&self, thread: ThreadId) -> Result<Option<RobustListRegistration>, TaskError> {
        Ok(Self::thread(&self.lock(), thread)?.robust_list)
    }

    pub fn take_robust_exit(&self, thread: ThreadId) -> Result<Option<RobustListRegistration>, TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        Ok(Self::thread_mut(&mut state, thread)?.robust_list.take())
    }
}
