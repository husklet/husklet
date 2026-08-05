use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use hl_task::{ExitStatus, ProcessId, TaskRegistry, ThreadId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitRuntimeError {
    Failed,
}

pub trait PreparedExitParticipant: Send {
    fn publish(&mut self) -> Result<(), ExitRuntimeError>;
    fn rollback(&mut self);
    fn finish(&mut self);
}

pub trait ExitParticipant: Send + Sync {
    fn prepare(
        &self,
        process: ProcessId,
        threads: &[ThreadId],
    ) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError>;
}

pub trait TaskExitFinalizer: Send + Sync {
    fn finalize(&self, process: ProcessId, threads: &[ThreadId], status: ExitStatus) -> Result<(), ExitRuntimeError>;
}

/// Publishes the terminal task state only after every reversible exit domain.
pub struct RegistryExitFinalizer {
    tasks: Arc<TaskRegistry>,
}

impl RegistryExitFinalizer {
    #[must_use]
    pub fn new(tasks: Arc<TaskRegistry>) -> Self {
        Self { tasks }
    }
}

impl TaskExitFinalizer for RegistryExitFinalizer {
    fn finalize(&self, process: ProcessId, _: &[ThreadId], status: ExitStatus) -> Result<(), ExitRuntimeError> {
        self.tasks
            .exit_process(process, status)
            .map_err(|_| ExitRuntimeError::Failed)
    }
}

pub struct ExitRuntime {
    robust: Arc<dyn ExitParticipant>,
    descriptors: Arc<dyn ExitParticipant>,
    ipc: Arc<dyn ExitParticipant>,
    memory: Arc<dyn ExitParticipant>,
    locks: Arc<dyn ExitParticipant>,
    task: Arc<dyn TaskExitFinalizer>,
    transaction: Mutex<()>,
    completed: Mutex<BTreeSet<ProcessId>>,
}

impl ExitRuntime {
    pub fn new(
        robust: Arc<dyn ExitParticipant>,
        descriptors: Arc<dyn ExitParticipant>,
        ipc: Arc<dyn ExitParticipant>,
        memory: Arc<dyn ExitParticipant>,
        locks: Arc<dyn ExitParticipant>,
        task: Arc<dyn TaskExitFinalizer>,
    ) -> Self {
        Self {
            robust,
            descriptors,
            ipc,
            memory,
            locks,
            task,
            transaction: Mutex::new(()),
            completed: Mutex::new(BTreeSet::new()),
        }
    }

    pub fn exit(&self, process: ProcessId, threads: &[ThreadId], status: ExitStatus) -> Result<(), ExitRuntimeError> {
        self.exit_once(process, threads, status).map(|_| ())
    }

    pub(crate) fn exit_once(
        &self,
        process: ProcessId,
        threads: &[ThreadId],
        status: ExitStatus,
    ) -> Result<bool, ExitRuntimeError> {
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&process)
        {
            return Ok(false);
        }
        let roles = [&self.robust, &self.descriptors, &self.ipc, &self.locks];
        let mut prepared = Vec::with_capacity(roles.len() + 1);
        for role in roles {
            match role.prepare(process, threads) {
                Ok(stage) => prepared.push(stage),
                Err(error) => {
                    Self::rollback(&mut prepared);
                    return Err(error);
                }
            }
        }
        for index in 0..prepared.len() {
            if let Err(error) = prepared[index].publish() {
                Self::rollback(&mut prepared);
                return Err(error);
            }
        }
        let mut memory = match self.memory.prepare(process, threads) {
            Ok(stage) => stage,
            Err(error) => {
                Self::rollback(&mut prepared);
                return Err(error);
            }
        };
        if let Err(error) = memory.publish() {
            memory.rollback();
            Self::rollback(&mut prepared);
            return Err(error);
        }
        prepared.push(memory);
        if let Err(error) = self.task.finalize(process, threads, status) {
            Self::rollback(&mut prepared);
            return Err(error);
        }
        self.completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(process);
        for stage in &mut prepared {
            stage.finish();
        }
        Ok(true)
    }

    fn rollback(stages: &mut [Box<dyn PreparedExitParticipant>]) {
        for stage in stages.iter_mut().rev() {
            stage.rollback();
        }
    }
}

#[cfg(test)]
#[path = "exit_test.rs"]
mod tests;
