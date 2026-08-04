use std::collections::BTreeMap;
use std::sync::Arc;

use hl_ipc::{
    IpcCatalogRestore, IpcCheckpointError, IpcCheckpointImage, IpcCheckpointRebind, IpcResourceKey,
    PipeEndpointBinding, PipeEndpointKind, SharedBackingAccess, SharedBackingCheckpoint, SharedBackingKey,
    TaskCheckpoint,
};
use hl_memory::MappingHost;
use hl_task::TaskResourceKey;

use super::{super::memory::MemoryState, super::task::Registry as TaskRegistry, PipeBindings};

/// Resolves IPC cross-section references against replacements staged earlier.
pub struct ResourceRebind<H> {
    memory: Arc<MemoryState<H>>,
    descriptors: Arc<PipeBindings>,
    tasks: Arc<TaskRegistry>,
}

impl<H> ResourceRebind<H> {
    #[must_use]
    pub const fn new(memory: Arc<MemoryState<H>>, descriptors: Arc<PipeBindings>, tasks: Arc<TaskRegistry>) -> Self {
        Self {
            memory,
            descriptors,
            tasks,
        }
    }
}

impl<H: MappingHost + 'static> IpcCheckpointRebind for ResourceRebind<H> {
    fn stage(&self, image: &IpcCheckpointImage) -> Result<Box<dyn IpcCatalogRestore>, IpcCheckpointError> {
        image.validate()?;
        let memory = self.memory.staged().ok_or(IpcCheckpointError::InvalidImage)?;
        let (tasks, task_image) = self.tasks.staged().ok_or(IpcCheckpointError::InvalidImage)?;
        Ok(Box::new(Transaction {
            memory: memory.shared.clone(),
            backings: image
                .backings
                .iter()
                .map(|backing| (backing.resource, backing.object))
                .collect(),
            descriptors: self.descriptors.clone(),
            tasks,
            task_image,
        }))
    }
}

struct Transaction {
    memory: Arc<hl_memory::SharedObjectStore>,
    backings: BTreeMap<SharedBackingKey, hl_memory::SharedObjectId>,
    descriptors: Arc<PipeBindings>,
    tasks: Arc<hl_task::TaskRegistry>,
    task_image: Arc<hl_task::TaskRegistryImage>,
}

impl IpcCatalogRestore for Transaction {
    fn memory(
        &mut self,
        backings: &[SharedBackingCheckpoint],
    ) -> Result<Arc<dyn SharedBackingAccess>, IpcCheckpointError> {
        if backings.len() != self.backings.len() {
            return Err(IpcCheckpointError::InvalidImage);
        }
        for backing in backings {
            if self.backings.get(&backing.resource) != Some(&backing.object) {
                return Err(IpcCheckpointError::InvalidImage);
            }
            drop(
                self.memory
                    .pin(backing.object, false)
                    .map_err(|_| IpcCheckpointError::InvalidImage)?,
            );
        }
        Ok(self.memory.clone())
    }

    fn descriptor(
        &mut self,
        key: IpcResourceKey,
        endpoint: PipeEndpointKind,
    ) -> Result<Arc<dyn PipeEndpointBinding>, IpcCheckpointError> {
        self.descriptors.descriptor(key, endpoint)
    }

    fn task(&mut self, task: TaskCheckpoint) -> Result<(), IpcCheckpointError> {
        let process = self
            .tasks
            .process_number(task.process)
            .map_err(|_| IpcCheckpointError::InvalidImage)?;
        let reference = self
            .task_image
            .processes
            .iter()
            .find(|reference| reference.process == process)
            .ok_or(IpcCheckpointError::InvalidImage)?;
        let key = TaskResourceKey(task.resource.get());
        if !reference.shared_resources.contains(&key) {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), IpcCheckpointError> {
        self.descriptors.commit()
    }

    fn rollback(&mut self) {
        self.descriptors.rollback();
    }

    fn resume(&mut self) -> Result<(), IpcCheckpointError> {
        self.descriptors.resume()
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        self.descriptors.finish();
    }
}
