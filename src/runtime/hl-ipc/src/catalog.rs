use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    IPC_CHECKPOINT_VERSION, IPC_PIPE_MAXIMUM, IpcCatalogRestore, IpcCheckpointError, IpcCheckpointImage, IpcPipeId,
    IpcResourceKey, MessageLimits, MessageQueueNamespace, Pipe, PipeCheckpoint, PipeEndpointBinding, PipeEndpointKind,
    SemaphoreLimits, SemaphoreNamespace, SharedBackingCheckpoint, SharedMemoryLimits, SharedMemoryNamespace,
    TaskCheckpoint,
};

struct PipeObject {
    pipe: Arc<Pipe>,
    reader: IpcResourceKey,
    writer: IpcResourceKey,
    reader_binding: Arc<dyn PipeEndpointBinding>,
    writer_binding: Arc<dyn PipeEndpointBinding>,
}

struct ClosedBinding;

impl PipeEndpointBinding for ClosedBinding {
    fn bind(&self, _: Arc<crate::PipeEndpoint>) -> Result<(), IpcCheckpointError> {
        Ok(())
    }
}

struct PipeSlot {
    generation: u32,
    object: Option<PipeObject>,
    reserved: bool,
}

/// Unpublished catalog ownership for a newly-created anonymous pipe.
pub struct PreparedPipe {
    catalog: Arc<IpcCatalog>,
    id: IpcPipeId,
    object: Option<PipeObject>,
    _admission: crate::checkpoint_activity::Admission,
    published: bool,
}

pub struct IpcCatalog {
    shared: Arc<SharedMemoryNamespace>,
    shared_limits: SharedMemoryLimits,
    backings: Vec<SharedBackingCheckpoint>,
    messages: Arc<MessageQueueNamespace>,
    message_limits: MessageLimits,
    semaphores: Arc<SemaphoreNamespace>,
    semaphore_limits: SemaphoreLimits,
    tasks: Vec<TaskCheckpoint>,
    pipes: Mutex<Vec<PipeSlot>>,
    next_resource: AtomicU64,
    activity: crate::checkpoint_activity::CheckpointActivity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpcCatalogError {
    Capacity,
    Stale,
    Busy,
    Invalid,
    Checkpoint(IpcCheckpointError),
}

impl From<IpcCheckpointError> for IpcCatalogError {
    fn from(error: IpcCheckpointError) -> Self {
        Self::Checkpoint(error)
    }
}

impl IpcCatalog {
    fn resource_seed(tasks: &[TaskCheckpoint]) -> u64 {
        tasks
            .iter()
            .map(|task| task.resource.get())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .unwrap_or(0)
    }

    #[must_use]
    pub fn shared_memory(&self) -> Arc<SharedMemoryNamespace> {
        Arc::clone(&self.shared)
    }

    #[must_use]
    pub const fn shared_limits(&self) -> SharedMemoryLimits {
        self.shared_limits
    }

    #[must_use]
    pub const fn message_limits(&self) -> MessageLimits {
        self.message_limits
    }

    #[must_use]
    pub const fn semaphore_limits(&self) -> SemaphoreLimits {
        self.semaphore_limits
    }

    #[must_use]
    pub fn semaphores(&self) -> Arc<SemaphoreNamespace> {
        Arc::clone(&self.semaphores)
    }

    #[must_use]
    pub fn new(
        shared: Arc<SharedMemoryNamespace>,
        shared_limits: SharedMemoryLimits,
        backings: Vec<SharedBackingCheckpoint>,
        messages: Arc<MessageQueueNamespace>,
        message_limits: MessageLimits,
        semaphores: Arc<SemaphoreNamespace>,
        semaphore_limits: SemaphoreLimits,
        tasks: Vec<TaskCheckpoint>,
    ) -> Self {
        let next_resource = Self::resource_seed(&tasks);
        Self {
            shared,
            shared_limits,
            backings,
            messages,
            message_limits,
            semaphores,
            semaphore_limits,
            tasks,
            pipes: Mutex::new(Vec::new()),
            next_resource: AtomicU64::new(next_resource),
            activity: crate::checkpoint_activity::CheckpointActivity::default(),
        }
    }

    /// Allocates two durable endpoint resource keys without reusing gaps.
    pub fn resource_pair(&self) -> Result<[IpcResourceKey; 2], IpcCatalogError> {
        let first = self
            .next_resource
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(2))
            .map_err(|_| IpcCatalogError::Capacity)?;
        Ok([
            IpcResourceKey::new(first).ok_or(IpcCatalogError::Capacity)?,
            IpcResourceKey::new(first + 1).ok_or(IpcCatalogError::Capacity)?,
        ])
    }

    /// Reserves catalog capacity while descriptor numbers remain unpublished.
    pub fn prepare_pipe(
        self: &Arc<Self>,
        pipe: Arc<Pipe>,
        reader: IpcResourceKey,
        writer: IpcResourceKey,
        reader_binding: Arc<dyn PipeEndpointBinding>,
        writer_binding: Arc<dyn PipeEndpointBinding>,
    ) -> Result<PreparedPipe, IpcCatalogError> {
        let admission = self.activity.admit();
        let mut slots = self.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = slots
            .iter()
            .position(|slot| slot.object.is_none() && !slot.reserved)
            .unwrap_or(slots.len());
        if index >= IPC_PIPE_MAXIMUM {
            return Err(IpcCatalogError::Capacity);
        }
        if index == slots.len() {
            slots.push(PipeSlot {
                generation: 0,
                object: None,
                reserved: false,
            });
        }
        let slot = &mut slots[index];
        slot.generation = slot.generation.wrapping_add(1).max(1);
        slot.reserved = true;
        let id = IpcPipeId {
            slot: index as u32,
            generation: slot.generation,
        };
        drop(slots);
        Ok(PreparedPipe {
            catalog: self.clone(),
            id,
            object: Some(PipeObject {
                pipe,
                reader,
                writer,
                reader_binding,
                writer_binding,
            }),
            _admission: admission,
            published: false,
        })
    }

    pub fn insert_pipe(
        &self,
        pipe: Arc<Pipe>,
        reader: IpcResourceKey,
        writer: IpcResourceKey,
        reader_binding: Arc<dyn PipeEndpointBinding>,
        writer_binding: Arc<dyn PipeEndpointBinding>,
    ) -> Result<IpcPipeId, IpcCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = slots
            .iter()
            .position(|slot| slot.object.is_none() && !slot.reserved)
            .unwrap_or(slots.len());
        if index >= IPC_PIPE_MAXIMUM {
            return Err(IpcCatalogError::Capacity);
        }
        if index == slots.len() {
            slots.push(PipeSlot {
                generation: 0,
                object: None,
                reserved: false,
            });
        }
        let slot = &mut slots[index];
        slot.generation = slot.generation.wrapping_add(1).max(1);
        slot.object = Some(PipeObject {
            pipe,
            reader,
            writer,
            reader_binding,
            writer_binding,
        });
        Ok(IpcPipeId {
            slot: index as u32,
            generation: slot.generation,
        })
    }

    /// Removes a pipe only after both endpoint descriptions have closed.
    pub fn retire_pipe(&self, id: IpcPipeId) -> Result<bool, IpcCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = Self::pipe_slot(&mut slots, id)?;
        let object = slot.object.as_ref().ok_or(IpcCatalogError::Stale)?;
        let snapshot = object.pipe.snapshot().map_err(|_| IpcCatalogError::Busy)?;
        if snapshot.readers != 0 || snapshot.writers != 0 {
            return Ok(false);
        }
        slot.object = None;
        Ok(true)
    }

    pub fn remove_pipe(&self, id: IpcPipeId) -> Result<(), IpcCatalogError> {
        let _admission = self.activity.admit();
        Self::pipe_slot(
            &mut self.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            id,
        )?
        .object
        .take()
        .ok_or(IpcCatalogError::Stale)?;
        Ok(())
    }

    pub fn with_pipe<R>(&self, id: IpcPipeId, operation: impl FnOnce(&Pipe) -> R) -> Result<R, IpcCatalogError> {
        let _admission = self.activity.admit();
        let slots = self.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let object = Self::pipe_slot_ref(&slots, id)?
            .object
            .as_ref()
            .ok_or(IpcCatalogError::Stale)?;
        Ok(operation(&object.pipe))
    }

    pub fn with_shared_memory<R>(&self, operation: impl FnOnce(&SharedMemoryNamespace) -> R) -> R {
        let _admission = self.activity.admit();
        operation(&self.shared)
    }

    pub fn with_messages<R>(&self, operation: impl FnOnce(&MessageQueueNamespace) -> R) -> R {
        let _admission = self.activity.admit();
        operation(&self.messages)
    }

    pub fn with_semaphores<R>(&self, operation: impl FnOnce(&SemaphoreNamespace) -> R) -> R {
        let _admission = self.activity.admit();
        operation(&self.semaphores)
    }

    pub fn freeze_checkpoint(&self) {
        self.activity.freeze();
        drop(self.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    pub fn thaw_checkpoint(&self) {
        self.activity.thaw();
    }

    pub fn checkpoint_image(&self) -> Result<IpcCheckpointImage, IpcCatalogError> {
        if !self.activity.frozen() {
            return Err(IpcCatalogError::Invalid);
        }
        let pipes = self.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.messages.checkpoint_waiters() != 0 || self.semaphores.checkpoint_waiters() != 0 {
            return Err(IpcCatalogError::Busy);
        }
        let mut objects = Vec::new();
        for (slot, value) in pipes.iter().enumerate() {
            let Some(object) = &value.object else { continue };
            let snapshot = object.pipe.snapshot().map_err(|_| IpcCatalogError::Busy)?;
            objects.push(PipeCheckpoint {
                id: IpcPipeId {
                    slot: slot as u32,
                    generation: value.generation,
                },
                snapshot,
                reader: object.reader,
                writer: object.writer,
            });
        }
        let image = IpcCheckpointImage {
            version: IPC_CHECKPOINT_VERSION,
            pipe_generations: pipes.iter().map(|slot| slot.generation).collect(),
            pipes: objects,
            shared_limits: self.shared_limits,
            shared: self.shared.snapshot(),
            backings: self.backings.clone(),
            message_limits: self.message_limits,
            messages: self.messages.snapshot(),
            semaphore_limits: self.semaphore_limits,
            semaphores: self.semaphores.snapshot(),
            tasks: self.tasks.clone(),
        };
        image.validate()?;
        Ok(image)
    }

    pub fn restore_checkpoint(
        image: &IpcCheckpointImage,
        restore: &mut dyn IpcCatalogRestore,
    ) -> Result<Arc<Self>, IpcCatalogError> {
        image.validate()?;
        for task in &image.tasks {
            restore.task(*task)?;
        }
        let memory = restore.memory(&image.backings)?;
        let shared = Arc::new(
            SharedMemoryNamespace::restore(memory, image.shared_limits, image.shared.clone())
                .map_err(|_| IpcCatalogError::Invalid)?,
        );
        let messages = Arc::new(
            MessageQueueNamespace::restore(image.message_limits, image.messages.clone())
                .map_err(|_| IpcCatalogError::Invalid)?,
        );
        let semaphores = Arc::new(
            SemaphoreNamespace::restore(image.semaphore_limits, image.semaphores.clone())
                .map_err(|_| IpcCatalogError::Invalid)?,
        );
        let mut pipes = image
            .pipe_generations
            .iter()
            .map(|generation| PipeSlot {
                generation: *generation,
                object: None,
                reserved: false,
            })
            .collect::<Vec<_>>();
        for item in &image.pipes {
            let pipe = Arc::new(Pipe::restore(&item.snapshot).map_err(|_| IpcCatalogError::Invalid)?);
            let reader_binding = if item.snapshot.readers == 0 {
                Arc::new(ClosedBinding) as Arc<dyn PipeEndpointBinding>
            } else {
                let binding = restore.descriptor(item.reader, PipeEndpointKind::Reader)?;
                binding.bind(Arc::clone(&pipe.reader))?;
                binding
            };
            let writer_binding = if item.snapshot.writers == 0 {
                Arc::new(ClosedBinding) as Arc<dyn PipeEndpointBinding>
            } else {
                let binding = restore.descriptor(item.writer, PipeEndpointKind::Writer)?;
                binding.bind(Arc::clone(&pipe.writer))?;
                binding
            };
            pipes[item.id.slot as usize].object = Some(PipeObject {
                pipe,
                reader: item.reader,
                writer: item.writer,
                reader_binding,
                writer_binding,
            });
        }
        let next_resource = image
            .pipes
            .iter()
            .flat_map(|pipe| [pipe.reader.get(), pipe.writer.get()])
            .chain(image.tasks.iter().map(|task| task.resource.get()))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .unwrap_or(0);
        let catalog = Arc::new(Self {
            shared,
            shared_limits: image.shared_limits,
            backings: image.backings.clone(),
            messages,
            message_limits: image.message_limits,
            semaphores,
            semaphore_limits: image.semaphore_limits,
            tasks: image.tasks.clone(),
            pipes: Mutex::new(pipes),
            next_resource: AtomicU64::new(next_resource),
            activity: crate::checkpoint_activity::CheckpointActivity::default(),
        });
        let weak = Arc::downgrade(&catalog);
        let pipes = catalog.pipes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (slot, entry) in pipes.iter().enumerate() {
            let Some(object) = &entry.object else { continue };
            let id = IpcPipeId {
                slot: slot as u32,
                generation: entry.generation,
            };
            object.reader_binding.attach(weak.clone(), id)?;
            object.writer_binding.attach(weak.clone(), id)?;
        }
        drop(pipes);
        Ok(catalog)
    }

    fn pipe_slot(slots: &mut [PipeSlot], id: IpcPipeId) -> Result<&mut PipeSlot, IpcCatalogError> {
        let slot = slots.get_mut(id.slot as usize).ok_or(IpcCatalogError::Stale)?;
        if slot.generation != id.generation {
            return Err(IpcCatalogError::Stale);
        }
        Ok(slot)
    }

    fn pipe_slot_ref(slots: &[PipeSlot], id: IpcPipeId) -> Result<&PipeSlot, IpcCatalogError> {
        let slot = slots.get(id.slot as usize).ok_or(IpcCatalogError::Stale)?;
        if slot.generation != id.generation {
            return Err(IpcCatalogError::Stale);
        }
        Ok(slot)
    }

    #[must_use]
    pub fn task_resources(&self) -> BTreeMap<u32, IpcResourceKey> {
        self.tasks.iter().map(|task| (task.process, task.resource)).collect()
    }
}

impl PreparedPipe {
    #[must_use]
    pub const fn id(&self) -> IpcPipeId {
        self.id
    }

    /// Publishes the already-reserved slot without another fallible step.
    #[must_use]
    pub fn publish(mut self) -> IpcPipeId {
        let mut slots = self
            .catalog
            .pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = &mut slots[self.id.slot as usize];
        debug_assert!(slot.reserved && slot.generation == self.id.generation);
        slot.object = self.object.take();
        slot.reserved = false;
        self.published = true;
        self.id
    }
}

impl Drop for PreparedPipe {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let mut slots = self
            .catalog
            .pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = slots.get_mut(self.id.slot as usize) else {
            return;
        };
        if slot.reserved && slot.generation == self.id.generation {
            slot.reserved = false;
        }
    }
}
