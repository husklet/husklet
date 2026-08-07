use super::TaskRegistry;
use crate::{Limit, ProcessCredentials, ProcessId, ProcessLifecycle, Resource, TaskError, ThreadId, ThreadLifecycle};

impl TaskRegistry {
    pub fn limit(&self, process: ProcessId, resource: Resource) -> Result<Limit, TaskError> {
        Self::process(&self.lock(), process)?
            .limits
            .get(resource)
            .ok_or(TaskError::InvalidLimit)
    }

    pub fn personality(&self, process: ProcessId) -> Result<u32, TaskError> {
        Ok(Self::process(&self.lock(), process)?.personality)
    }

    pub fn set_personality(&self, process: ProcessId, value: u32) -> Result<u32, TaskError> {
        let mut state = self.lock();
        let process = Self::process_mut(&mut state, process)?;
        let previous = process.personality;
        process.personality = value;
        Ok(previous)
    }

    pub fn credentials(&self, process: ProcessId) -> Result<ProcessCredentials, TaskError> {
        Ok(Self::process(&self.lock(), process)?.credentials.clone())
    }

    pub fn set_thread_blocked(&self, thread: ThreadId, blocked: bool) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let thread = Self::thread_mut(&mut state, thread)?;
        let expected = if blocked {
            ThreadLifecycle::Runnable
        } else {
            ThreadLifecycle::Blocked
        };
        if thread.lifecycle != expected {
            return Err(TaskError::InvalidLifecycle);
        }
        thread.lifecycle = if blocked {
            ThreadLifecycle::Blocked
        } else {
            ThreadLifecycle::Runnable
        };
        Ok(())
    }

    pub fn replace_credentials(&self, process: ProcessId, credentials: ProcessCredentials) -> Result<(), TaskError> {
        if credentials.supplementary_groups().len() > self.max_groups {
            return Err(TaskError::GroupLimit);
        }
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        let process = Self::process_mut(&mut state, process)?;
        if process.lifecycle != ProcessLifecycle::Running {
            return Err(TaskError::InvalidLifecycle);
        }
        process.credentials = credentials;
        Ok(())
    }

    pub fn set_limit(&self, process: ProcessId, resource: Resource, limit: Limit) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        let process = Self::process_mut(&mut state, process)?;
        if process.lifecycle != ProcessLifecycle::Running {
            return Err(TaskError::InvalidLifecycle);
        }
        process.limits.set(resource, limit);
        Ok(())
    }

    pub fn set_pdeath(&self, process: ProcessId, signal: u32) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::process_mut(&mut state, process)?.parent_death_signal = signal;
        Ok(())
    }

    pub fn set_subreaper(&self, process: ProcessId, enabled: bool) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::process_mut(&mut state, process)?.child_subreaper = enabled;
        Ok(())
    }

    pub fn set_dumpable(&self, process: ProcessId, enabled: bool) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::process_mut(&mut state, process)?.dumpable = enabled;
        Ok(())
    }

    pub fn set_oom_score_adj(&self, process: ProcessId, value: i16) -> Result<(), TaskError> {
        self.set_task_oom_score_adj(process, None, value)
    }

    /// Reads one process OOM adjustment after atomically validating an optional exact thread owner.
    pub fn task_oom_score_adj(&self, process: ProcessId, thread: Option<crate::ThreadId>) -> Result<i16, TaskError> {
        let state = self.lock();
        Self::validate_oom_target(&state, process, thread)?;
        Ok(Self::process(&state, process)?.oom_score_adj)
    }

    /// Sets one process OOM adjustment after atomically validating an optional exact thread owner.
    pub fn set_task_oom_score_adj(
        &self,
        process: ProcessId,
        thread: Option<crate::ThreadId>,
        value: i16,
    ) -> Result<(), TaskError> {
        if !(-1000..=1000).contains(&value) {
            return Err(TaskError::InvalidLimit);
        }
        let mut state = self.lock();
        Self::validate_oom_target(&state, process, thread)?;
        Self::process_mut(&mut state, process)?.oom_score_adj = value;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_task_oom_score_adj_with_hook(
        &self,
        process: ProcessId,
        thread: Option<crate::ThreadId>,
        value: i16,
        hook: impl FnOnce(),
    ) -> Result<(), TaskError> {
        if !(-1000..=1000).contains(&value) {
            return Err(TaskError::InvalidLimit);
        }
        let mut state = self.lock();
        Self::validate_oom_target(&state, process, thread)?;
        hook();
        Self::process_mut(&mut state, process)?.oom_score_adj = value;
        Ok(())
    }

    fn validate_oom_target(
        state: &super::State,
        process: ProcessId,
        thread: Option<crate::ThreadId>,
    ) -> Result<(), TaskError> {
        Self::process(state, process)?;
        if let Some(thread) = thread
            && Self::thread(state, thread)?.process != process
        {
            return Err(TaskError::WrongProcess);
        }
        Ok(())
    }

    pub fn set_timer_slack(&self, process: ProcessId, value: u64) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::process_mut(&mut state, process)?.timer_slack = value;
        Ok(())
    }

    pub fn set_thp(&self, process: ProcessId, disabled: bool) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::process_mut(&mut state, process)?.thp_disabled = disabled;
        Ok(())
    }

    pub fn set_mce_policy(&self, process: ProcessId, policy: u32) -> Result<(), TaskError> {
        if policy > 2 {
            return Err(TaskError::InvalidLimit);
        }
        let mut state = self.lock();
        Self::process_mut(&mut state, process)?.mce_policy = policy;
        Ok(())
    }

    pub fn set_name(&self, thread: ThreadId, name: [u8; 16]) -> Result<(), TaskError> {
        let mut state = self.lock();
        let process = Self::thread(&state, thread)?.process;
        if Self::process(&state, process)?.leader == thread {
            Self::process_mut(&mut state, process)?.name = name;
        }
        Self::thread_mut(&mut state, thread)?.name = name;
        Ok(())
    }
}
