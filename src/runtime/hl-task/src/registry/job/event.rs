use super::TaskRegistry;
use crate::{ExitStatus, ProcessId, ProcessLifecycle, TaskError, WaitEvent, WaitSelector};

impl TaskRegistry {
    pub fn wait(&self, parent: ProcessId, selector: WaitSelector) -> Result<Option<WaitEvent>, TaskError> {
        let state = self.lock();
        Self::validate_wait_selector(&state, parent, selector)?;
        Ok(state
            .waits
            .iter()
            .find(|event| {
                event.parent == parent
                    && match selector {
                        WaitSelector::Any => true,
                        WaitSelector::Process(process) => event.child == process,
                    }
            })
            .copied())
    }

    pub fn reap(&self, parent: ProcessId, child: ProcessId) -> Result<ExitStatus, TaskError> {
        let mut state = self.lock();
        Self::process(&state, parent)?;
        let child_state = Self::process(&state, child)?;
        if child_state.parent != Some(parent) || child_state.lifecycle != ProcessLifecycle::Zombie {
            return Err(TaskError::NotWaitable);
        }
        if !child_state.children.is_empty() {
            return Err(TaskError::HasChildren);
        }
        let status = child_state.exit_status.ok_or(TaskError::NotWaitable)?;
        Self::process_mut(&mut state, parent)?.children.remove(&child);
        state
            .waits
            .retain(|event| !(event.parent == parent && event.child == child));
        state.child_events.retain(|event| event.child != child);
        Self::detach_group_member(&mut state, child)?;
        Self::release_process(&mut state, child)?;
        Ok(status)
    }
}
