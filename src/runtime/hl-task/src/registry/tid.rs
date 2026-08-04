use super::TaskRegistry;
use crate::{CloneThreadPlan, ForkProcessPlan, TaskError, ThreadId, ThreadLifecycle};

impl TaskRegistry {
    pub fn stage_fork_clear(&self, plan: &ForkProcessPlan, address: u64) -> Result<(), TaskError> {
        let mut state = self.lock();
        let thread = Self::thread_mut(&mut state, plan.thread())?;
        if thread.lifecycle != ThreadLifecycle::Starting || thread.pending_transaction != Some(plan.transaction) {
            return Err(TaskError::InvalidPlan);
        }
        thread.clear_tid = (address != 0).then_some(address);
        Ok(())
    }

    pub fn stage_clear_tid(&self, plan: &CloneThreadPlan, address: u64) -> Result<(), TaskError> {
        let mut state = self.lock();
        let thread = Self::thread_mut(&mut state, plan.thread)?;
        if thread.lifecycle != ThreadLifecycle::Starting || thread.pending_transaction != Some(plan.transaction) {
            return Err(TaskError::InvalidPlan);
        }
        thread.clear_tid = (address != 0).then_some(address);
        Ok(())
    }

    pub fn set_clear_tid(&self, thread: ThreadId, address: u64) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        Self::thread_mut(&mut state, thread)?.clear_tid = (address != 0).then_some(address);
        Ok(())
    }

    pub fn clear_tid(&self, thread: ThreadId) -> Result<Option<u64>, TaskError> {
        Ok(Self::thread(&self.lock(), thread)?.clear_tid)
    }

    pub fn take_clear_tid(&self, thread: ThreadId) -> Result<Option<u64>, TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        Ok(Self::thread_mut(&mut state, thread)?.clear_tid.take())
    }
}
