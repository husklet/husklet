use super::{ProcessId, State, TaskError, TaskRegistry};

impl TaskRegistry {
    pub(super) fn reparent_children(
        state: &mut State,
        process: ProcessId,
        max_pending: usize,
    ) -> Result<Vec<ProcessId>, TaskError> {
        let adopter = Self::reparent_target(state, process)?;
        let children = Self::process(state, process)?
            .children
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for child in &children {
            Self::queue_parent_death(state, *child, max_pending)?;
            Self::process_mut(state, *child)?.parent = Some(adopter);
            Self::retarget_wait_event(state, *child, adopter);
        }
        Self::process_mut(state, adopter)?.children.extend(children);
        Self::process_mut(state, process)?.children.clear();
        Self::refresh_orphaned_groups(state)
    }

    fn reparent_target(state: &State, process: ProcessId) -> Result<ProcessId, TaskError> {
        let init = state.init.ok_or(TaskError::InvalidSnapshot)?;
        let mut ancestor = Self::process(state, process)?.parent;
        while let Some(candidate) = ancestor {
            let parent = Self::process(state, candidate)?;
            if parent.child_subreaper {
                return Ok(candidate);
            }
            ancestor = parent.parent;
        }
        Ok(init)
    }

    fn queue_parent_death(state: &mut State, child: ProcessId, maximum: usize) -> Result<(), TaskError> {
        let raw = Self::process(state, child)?.parent_death_signal;
        let signal = u8::try_from(raw)
            .ok()
            .and_then(|number| crate::SignalNumber::new(number).ok());
        let Some(signal) = signal else { return Ok(()) };
        let _ = Self::process_mut(state, child)?
            .signals
            .pending
            .enqueue(crate::SignalInfo::bare(signal), maximum);
        Ok(())
    }

    fn retarget_wait_event(state: &mut State, child: ProcessId, parent: ProcessId) {
        for event in &mut state.waits {
            if event.child == child {
                event.parent = parent;
            }
        }
        for event in &mut state.child_events {
            if event.child == child {
                event.parent = parent;
            }
        }
    }
}
