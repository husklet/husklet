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
            .prepare_image(process, thread, linux_comm(&plan.path), plan.arguments.clone())
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

#[must_use]
pub fn linux_comm(path: &[u8]) -> [u8; 16] {
    let leaf = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
    let mut name = [0; 16];
    let count = leaf.len().min(15);
    name[..count].copy_from_slice(&leaf[..count]);
    name
}

impl TaskExecParticipant {
    const fn project(_: TaskError) -> RuntimeExecError {
        RuntimeExecError::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::linux_comm;

    #[test]
    fn comm_uses_exec_leaf_and_linux_limit() {
        assert_eq!(linux_comm(b"./selfexe"), *b"selfexe\0\0\0\0\0\0\0\0\0");
        assert_eq!(linux_comm(b"/proc/self/exe"), *b"exe\0\0\0\0\0\0\0\0\0\0\0\0\0");
        assert_eq!(linux_comm(b"/long-executable-name"), *b"long-executable\0");
    }
}
