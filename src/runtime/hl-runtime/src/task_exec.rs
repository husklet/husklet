use std::sync::Arc;

use hl_linux::ExecPlan;
use hl_task::{PreparedTaskExec, ProcessId, TaskError, TaskRegistry, ThreadId};

use crate::{PreparedExecParticipant, RuntimeExecError, RuntimeExecParticipant};

pub struct TaskExecParticipant {
    tasks: Arc<TaskRegistry>,
}

impl TaskExecParticipant {
    #[must_use]
    pub const fn new(tasks: Arc<TaskRegistry>) -> Self {
        Self { tasks }
    }

    pub fn prepare_target(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &ExecPlan,
    ) -> Result<(Box<dyn PreparedExecParticipant>, ThreadId), RuntimeExecError> {
        let prepared = self
            .tasks
            .prepare_image(process, thread, plan.comm(), plan.arguments.clone())
            .map_err(Self::project)?;
        let target = prepared.resulting_thread();
        Ok((Box::new(TaskExecStage { prepared }), target))
    }
}

struct TaskExecStage {
    prepared: PreparedTaskExec,
}

impl PreparedExecParticipant for TaskExecStage {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.prepared.publish().map_err(TaskExecParticipant::project)
    }

    fn rollback(&mut self) {
        self.prepared.rollback();
    }

    fn finish(&mut self) {
        self.prepared.finish();
    }
}

impl RuntimeExecParticipant for TaskExecParticipant {
    fn prepare(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        self.prepare_target(process, thread, plan).map(|(prepared, _)| prepared)
    }
}

impl TaskExecParticipant {
    const fn project(_: TaskError) -> RuntimeExecError {
        RuntimeExecError::Failed
    }
}
