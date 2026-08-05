use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_ipc::{CommittedMemoryExec, IpcCatalog, PreparedMemoryExec, PreparedSemaphoreExec};
use hl_isa::GuestAddress;
use hl_linux::ExecPlan;
use hl_memory::{MapRequest, MappingBatch, MappingCoordinator, MappingHost, MappingOperation, Placement};
use hl_task::{ProcessId, ThreadId};

use super::segment::Mapping;
use crate::{MemoryMappings, PreparedExecParticipant, RuntimeExecError, RuntimeExecParticipant};

/// Explicit exec participant for a composition root with no IPC namespace.
/// This is distinct from an unavailable syscall port: construction proves
/// that there is no IPC state requiring cleanup.
pub struct EmptyIpcExec;

struct EmptyPrepared;

impl RuntimeExecParticipant for EmptyIpcExec {
    fn prepare(
        &self,
        _: ProcessId,
        _: ThreadId,
        _: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        Ok(Box::new(EmptyPrepared))
    }
}

impl PreparedExecParticipant for EmptyPrepared {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn finish(&mut self) {}
}

pub struct ExecParticipant<H: MappingHost> {
    catalog: Arc<IpcCatalog>,
    mappings: Arc<MemoryMappings<H>>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
}

struct PreparedIpcExec<H: MappingHost> {
    namespace: Option<PreparedMemoryExec>,
    semaphores: Option<PreparedSemaphoreExec>,
    mappings: PreparedExecMappings<H>,
    committed_namespace: Option<CommittedMemoryExec>,
    published: bool,
}

pub(crate) struct PreparedExecMappings<H: MappingHost> {
    coordinator: Arc<MappingCoordinator<H>>,
    mappings: Arc<Mutex<BTreeMap<GuestAddress, Mapping>>>,
    expected: BTreeMap<GuestAddress, Mapping>,
    requests: Vec<MapRequest>,
    published: bool,
}

impl<H: MappingHost> ExecParticipant<H> {
    pub fn new(
        catalog: Arc<IpcCatalog>,
        mappings: Arc<MemoryMappings<H>>,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self { catalog, mappings, now }
    }
}

impl<H: MappingHost + 'static> RuntimeExecParticipant for ExecParticipant<H> {
    fn prepare(
        &self,
        process: ProcessId,
        _: ThreadId,
        _: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        let process = process.number();
        let namespace = self
            .catalog
            .shared_memory()
            .prepare_exec(process, (self.now)())
            .map_err(|_| RuntimeExecError::Failed)?;
        let semaphores = self.catalog.semaphores().prepare_exec(process);
        let mappings = PreparedExecMappings::new(&self.mappings)?;
        let mut namespace_tokens = namespace.attachments().to_vec();
        namespace_tokens.sort_unstable();
        let mut mapping_tokens = mappings
            .expected
            .values()
            .map(|mapping| mapping.attachment.ok_or(RuntimeExecError::Failed))
            .collect::<Result<Vec<_>, _>>()?;
        mapping_tokens.sort_unstable();
        if namespace_tokens != mapping_tokens {
            return Err(RuntimeExecError::Failed);
        }
        Ok(Box::new(PreparedIpcExec {
            namespace: Some(namespace),
            semaphores: Some(semaphores),
            mappings,
            committed_namespace: None,
            published: false,
        }))
    }
}

impl<H: MappingHost> PreparedExecMappings<H> {
    pub(crate) fn new(owner: &MemoryMappings<H>) -> Result<Self, RuntimeExecError> {
        let expected = owner
            .mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let regions = owner.coordinator.ledger().regions();
        let mut requests = Vec::with_capacity(expected.len());
        for mapping in expected.values() {
            let region = regions
                .iter()
                .find(|region| region.range() == mapping.range)
                .ok_or(RuntimeExecError::Failed)?;
            requests.push(MapRequest {
                placement: Placement::FixedNoReplace(mapping.range.start()),
                length: mapping.range.length(),
                alignment: 4096,
                protection: region.protection(),
                backing: region.backing(),
                backing_offset: region.backing_offset(),
            });
        }
        Ok(Self {
            coordinator: Arc::clone(&owner.coordinator),
            mappings: Arc::clone(&owner.mappings),
            expected,
            requests,
            published: false,
        })
    }

    pub(crate) fn publish(&mut self) -> Result<(), RuntimeExecError> {
        let mut batch = MappingBatch::new();
        for mapping in self.expected.values() {
            batch.push(MappingOperation::Unmap(mapping.range));
        }
        self.coordinator.apply(&batch).map_err(|_| RuntimeExecError::Failed)?;
        let mut mappings = self.mappings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *mappings != self.expected {
            drop(mappings);
            self.restore_memory()?;
            return Err(RuntimeExecError::Failed);
        }
        mappings.clear();
        self.published = true;
        Ok(())
    }

    pub(crate) fn rollback(&mut self) -> Result<(), RuntimeExecError> {
        if !self.published {
            return Ok(());
        }
        self.restore_memory()?;
        let mut mappings = self.mappings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !mappings.is_empty() {
            return Err(RuntimeExecError::Failed);
        }
        *mappings = self.expected.clone();
        self.published = false;
        Ok(())
    }

    fn restore_memory(&self) -> Result<(), RuntimeExecError> {
        let mut batch = MappingBatch::new();
        for request in &self.requests {
            batch.push(MappingOperation::Map(*request));
        }
        self.coordinator
            .apply(&batch)
            .map(|_| ())
            .map_err(|_| RuntimeExecError::Failed)
    }
}

impl<H: MappingHost> PreparedExecParticipant for PreparedIpcExec<H> {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.mappings.publish()?;
        let namespace = self
            .namespace
            .take()
            .ok_or(RuntimeExecError::Failed)?
            .commit()
            .map_err(|_| RuntimeExecError::Failed);
        let namespace = match namespace {
            Ok(value) => value,
            Err(error) => {
                self.mappings.rollback()?;
                return Err(error);
            }
        };
        if self
            .semaphores
            .take()
            .ok_or(RuntimeExecError::Failed)?
            .commit()
            .is_err()
        {
            namespace.rollback().map_err(|_| RuntimeExecError::Failed)?;
            self.mappings.rollback()?;
            return Err(RuntimeExecError::Failed);
        }
        self.committed_namespace = Some(namespace);
        self.published = true;
        Ok(())
    }

    fn rollback(&mut self) {
        if !self.published {
            return;
        }
        if let Some(namespace) = self.committed_namespace.take() {
            let _ = namespace.rollback();
        }
        let _ = self.mappings.rollback();
        self.published = false;
    }

    fn finish(&mut self) {
        if let Some(namespace) = self.committed_namespace.take() {
            let _ = namespace.finish();
        }
        self.published = false;
    }
}

#[cfg(test)]
#[path = "exec_test.rs"]
mod tests;
