use std::sync::Arc;

use hl_ipc::{CommittedMemoryExec, CommittedSemaphoreExit, IpcCatalog, PreparedMemoryExec, PreparedSemaphoreExit};
use hl_memory::MappingHost;
use hl_task::{ProcessId, ThreadId};

use super::exec::PreparedExecMappings;
use crate::MemoryMappings;
use crate::exit_runtime::{ExitParticipant as RuntimeExitParticipant, ExitRuntimeError, PreparedExitParticipant};

/// Reversibly retires a process's System V IPC state and IPC mappings.
pub struct ExitHandler<H: MappingHost> {
    catalog: Arc<IpcCatalog>,
    mappings: Arc<MemoryMappings<H>>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
}

struct PreparedIpcExit<H: MappingHost> {
    shared: Option<PreparedMemoryExec>,
    semaphores: Option<PreparedSemaphoreExit>,
    mappings: PreparedExecMappings<H>,
    committed_shared: Option<CommittedMemoryExec>,
    committed_semaphores: Option<CommittedSemaphoreExit>,
    published: bool,
}

impl<H: MappingHost> ExitHandler<H> {
    #[must_use]
    pub fn new(
        catalog: Arc<IpcCatalog>,
        mappings: Arc<MemoryMappings<H>>,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self { catalog, mappings, now }
    }
}

impl<H: MappingHost + 'static> RuntimeExitParticipant for ExitHandler<H> {
    fn prepare(
        &self,
        process: ProcessId,
        _: &[ThreadId],
    ) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError> {
        let process = process.number();
        let now = (self.now)();
        let shared = self
            .catalog
            .shared_memory()
            .prepare_exec(process, now)
            .map_err(|_| ExitRuntimeError::Failed)?;
        let semaphores = self
            .catalog
            .semaphores()
            .prepare_exit(process, now)
            .map_err(|_| ExitRuntimeError::Failed)?;
        let mappings = PreparedExecMappings::new(&self.mappings)?;
        Ok(Box::new(PreparedIpcExit {
            shared: Some(shared),
            semaphores: Some(semaphores),
            mappings,
            committed_shared: None,
            committed_semaphores: None,
            published: false,
        }))
    }
}

impl From<crate::RuntimeExecError> for ExitRuntimeError {
    fn from(_: crate::RuntimeExecError) -> Self {
        Self::Failed
    }
}

impl<H: MappingHost> PreparedExitParticipant for PreparedIpcExit<H> {
    fn publish(&mut self) -> Result<(), ExitRuntimeError> {
        self.mappings.publish()?;
        let Ok(shared) = self.shared.take().ok_or(ExitRuntimeError::Failed)?.commit() else {
            self.mappings.rollback()?;
            return Err(ExitRuntimeError::Failed);
        };
        let Ok(semaphores) = self.semaphores.take().ok_or(ExitRuntimeError::Failed)?.commit() else {
            shared.rollback().map_err(|_| ExitRuntimeError::Failed)?;
            self.mappings.rollback()?;
            return Err(ExitRuntimeError::Failed);
        };
        self.committed_shared = Some(shared);
        self.committed_semaphores = Some(semaphores);
        self.published = true;
        Ok(())
    }

    fn rollback(&mut self) {
        if !self.published {
            return;
        }
        if let Some(semaphores) = self.committed_semaphores.take() {
            let _ = semaphores.rollback();
        }
        if let Some(shared) = self.committed_shared.take() {
            let _ = shared.rollback();
        }
        let _ = self.mappings.rollback();
        self.published = false;
    }

    fn finish(&mut self) {
        if let Some(semaphores) = self.committed_semaphores.take() {
            semaphores.finish();
        }
        if let Some(shared) = self.committed_shared.take() {
            let _ = shared.finish();
        }
        self.published = false;
    }
}

#[cfg(test)]
#[path = "exit_test.rs"]
mod tests;
