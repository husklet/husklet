use std::sync::{Arc, Mutex};

use hl_task::{ForkProcessPlan, ForkRequest, ProcessId, TaskRegistry, ThreadId};

use crate::{ForkContext, ForkParticipant, ForkParticipantRole};

struct TaskForkState {
    plan: Option<ForkProcessPlan>,
    child: Option<(ProcessId, ThreadId)>,
    coordinator_committed: bool,
}

/// Owns the task reservation that must publish after every resource participant.
pub struct TaskForkParticipant {
    tasks: Arc<TaskRegistry>,
    request: ForkRequest,
    deferred: bool,
    state: Mutex<TaskForkState>,
}

impl TaskForkParticipant {
    pub fn reserve(tasks: Arc<TaskRegistry>, source: ThreadId) -> Result<Self, ()> {
        let plan = tasks.begin_fork_process(source).map_err(|_| ())?;
        let request = ForkRequest {
            parent: plan.parent().fork_identity(),
            child: plan.process().fork_identity(),
            flags: hl_task::ForkCloneFlags::default(),
        };
        Ok(Self {
            tasks,
            request,
            deferred: false,
            state: Mutex::new(TaskForkState {
                plan: Some(plan),
                child: None,
                coordinator_committed: false,
            }),
        })
    }

    pub fn reserve_deferred(tasks: Arc<TaskRegistry>, source: ThreadId) -> Result<Self, ()> {
        let mut participant = Self::reserve(tasks, source)?;
        participant.deferred = true;
        Ok(participant)
    }

    pub fn reserved_child(&self) -> Option<(ProcessId, ThreadId)> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .plan
            .as_ref()
            .map(|plan| (plan.process(), plan.thread()))
    }

    pub fn publish_deferred(&self) -> Result<(ProcessId, ThreadId), ()> {
        if !self.deferred {
            return Err(());
        }
        let mut state = self.state.lock().map_err(|_| ())?;
        if !state.coordinator_committed || state.child.is_some() {
            return Err(());
        }
        let plan = state.plan.take().ok_or(())?;
        let child = self.tasks.commit_fork_process(plan).map_err(|_| ())?;
        state.child = Some(child);
        Ok(child)
    }

    #[must_use]
    pub const fn request(&self) -> ForkRequest {
        self.request
    }

    pub fn child(&self) -> Option<(ProcessId, ThreadId)> {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).child
    }
}

impl ForkParticipant for TaskForkParticipant {
    fn role(&self) -> ForkParticipantRole {
        ForkParticipantRole::Task
    }

    fn prepare(&self, context: ForkContext) -> Result<u64, ()> {
        let state = self.state.lock().map_err(|_| ())?;
        if context.request.parent != self.request.parent
            || context.request.child != self.request.child
            || state.plan.is_none()
            || state.child.is_some()
        {
            return Err(());
        }
        Ok(1)
    }

    fn freeze(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn repair_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn commit(&self, _: ForkContext, reservation: u64) -> Result<(), ()> {
        if reservation != 1 {
            return Err(());
        }
        let mut state = self.state.lock().map_err(|_| ())?;
        if self.deferred {
            state.coordinator_committed = true;
            return Ok(());
        }
        let plan = state.plan.take().ok_or(())?;
        match self.tasks.commit_fork_process(plan) {
            Ok(child) => {
                state.child = Some(child);
                Ok(())
            }
            Err(_) => Err(()),
        }
    }

    fn rollback(&self, _: ForkContext, _: u64) {
        let plan = self.state.lock().unwrap_or_else(|error| error.into_inner()).plan.take();
        if let Some(plan) = plan {
            let _ = self.tasks.rollback_fork_process(plan);
        }
    }
}

impl Drop for TaskForkParticipant {
    fn drop(&mut self) {
        let plan = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .plan
            .take();
        if let Some(plan) = plan {
            let _ = self.tasks.rollback_fork_process(plan);
        }
    }
}
