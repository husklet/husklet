use super::TaskRegistry;
use crate::{SchedulingProfile, TaskError, ThreadId};

impl TaskRegistry {
    pub fn schedule(&self, thread: ThreadId) -> Result<SchedulingProfile, TaskError> {
        Ok(Self::thread(&self.lock(), thread)?.schedule)
    }

    pub fn set_schedule(&self, thread: ThreadId, profile: SchedulingProfile) -> Result<(), TaskError> {
        Self::thread_mut(&mut self.lock(), thread)?.schedule = profile;
        Ok(())
    }
}
