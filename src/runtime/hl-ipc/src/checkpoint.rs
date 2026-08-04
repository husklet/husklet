use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::Arc;

use hl_memory::{SharedObjectId, SharedObjectStore};

use crate::{
    MessageLimits, MessageQueueSnapshot, PipeSnapshot, SemaphoreLimits, SemaphoreSnapshot, SharedMemoryId,
    SharedMemoryLimits, SharedMemorySnapshot,
};

pub const IPC_CHECKPOINT_VERSION: u32 = 1;
pub const IPC_PIPE_MAXIMUM: usize = 1 << 20;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IpcResourceKey(NonZeroU64);

impl IpcResourceKey {
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SharedBackingKey(NonZeroU64);

impl SharedBackingKey {
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IpcPipeId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipeEndpointKind {
    Reader,
    Writer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipeCheckpoint {
    pub id: IpcPipeId,
    pub snapshot: PipeSnapshot,
    pub reader: IpcResourceKey,
    pub writer: IpcResourceKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SharedBackingCheckpoint {
    pub segment: SharedMemoryId,
    pub object: SharedObjectId,
    pub resource: SharedBackingKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskCheckpoint {
    pub process: u32,
    pub resource: IpcResourceKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpcCheckpointImage {
    pub version: u32,
    pub pipe_generations: Vec<u32>,
    pub pipes: Vec<PipeCheckpoint>,
    pub shared_limits: SharedMemoryLimits,
    pub shared: SharedMemorySnapshot,
    pub backings: Vec<SharedBackingCheckpoint>,
    pub message_limits: MessageLimits,
    pub messages: MessageQueueSnapshot,
    pub semaphore_limits: SemaphoreLimits,
    pub semaphores: SemaphoreSnapshot,
    pub tasks: Vec<TaskCheckpoint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpcCheckpointError {
    InvalidImage,
    ResourceLimit,
    Busy,
}

pub trait PipeEndpointBinding: Send + Sync {
    fn bind(&self, endpoint: Arc<crate::PipeEndpoint>) -> Result<(), IpcCheckpointError>;
    fn attach(&self, _catalog: std::sync::Weak<crate::IpcCatalog>, _pipe: IpcPipeId) -> Result<(), IpcCheckpointError> {
        Ok(())
    }
}

pub trait SharedBackingAccess: std::fmt::Debug + Send + Sync {
    fn create(&self, owner: u64, size: usize) -> Result<SharedObjectId, crate::SharedMemoryError>;
    fn remove(&self, object: SharedObjectId) -> Result<(), crate::SharedMemoryError>;
    fn validate(&self, backing: hl_memory::SharedBackingRef) -> Result<(), crate::SharedMemoryError>;
}

impl SharedBackingAccess for SharedObjectStore {
    fn create(&self, owner: u64, size: usize) -> Result<SharedObjectId, crate::SharedMemoryError> {
        self.create(owner, size).map_err(crate::SharedMemoryError::Shared)
    }

    fn remove(&self, object: SharedObjectId) -> Result<(), crate::SharedMemoryError> {
        self.remove(object).map_err(crate::SharedMemoryError::Shared)
    }

    fn validate(&self, backing: hl_memory::SharedBackingRef) -> Result<(), crate::SharedMemoryError> {
        drop(self.pin_backing(backing, false)?);
        Ok(())
    }
}

pub trait IpcCatalogRestore: Send {
    fn memory(
        &mut self,
        backings: &[SharedBackingCheckpoint],
    ) -> Result<Arc<dyn SharedBackingAccess>, IpcCheckpointError>;
    fn descriptor(
        &mut self,
        key: IpcResourceKey,
        endpoint: PipeEndpointKind,
    ) -> Result<Arc<dyn PipeEndpointBinding>, IpcCheckpointError>;
    fn task(&mut self, task: TaskCheckpoint) -> Result<(), IpcCheckpointError>;
    fn commit(&mut self) -> Result<(), IpcCheckpointError>;
    fn rollback(&mut self);
    fn resume(&mut self) -> Result<(), IpcCheckpointError>;
}

pub trait IpcCheckpointRebind: Send + Sync {
    fn stage(&self, image: &IpcCheckpointImage) -> Result<Box<dyn IpcCatalogRestore>, IpcCheckpointError>;
}

impl IpcCheckpointImage {
    pub fn validate(&self) -> Result<(), IpcCheckpointError> {
        if self.version != IPC_CHECKPOINT_VERSION
            || self.pipe_generations.len() > IPC_PIPE_MAXIMUM
            || self.pipe_generations.contains(&0)
            || !Self::valid_limits(self.shared_limits, self.message_limits, self.semaphore_limits)
        {
            return Err(IpcCheckpointError::InvalidImage);
        }
        self.validate_pipes()?;
        self.validate_shared()?;
        self.validate_messages()?;
        self.validate_semaphores()?;
        self.validate_tasks()
    }

    fn valid_limits(shared: SharedMemoryLimits, messages: MessageLimits, semaphores: SemaphoreLimits) -> bool {
        shared.segments != 0
            && shared.segments <= 4096
            && shared.segment_bytes != 0
            && shared.segment_bytes <= (1 << 30)
            && shared.total_bytes != 0
            && shared.total_bytes <= (1_u64 << 32) as usize
            && shared.attachments != 0
            && shared.attachments <= (1 << 20)
            && messages.queues != 0
            && messages.queues <= 32_000
            && messages.queue_bytes != 0
            && messages.queue_bytes <= (1 << 20)
            && messages.queue_messages != 0
            && messages.queue_messages <= (1 << 20)
            && messages.total_bytes != 0
            && messages.total_bytes <= (1 << 30)
            && messages.total_messages != 0
            && messages.total_messages <= (1 << 20)
            && messages.message_bytes != 0
            && messages.message_bytes <= (1 << 20)
            && semaphores.sets != 0
            && semaphores.sets <= 32_000
            && semaphores.set_semaphores != 0
            && semaphores.set_semaphores <= 32_000
            && semaphores.total_semaphores != 0
            && semaphores.total_semaphores <= (1 << 20)
            && semaphores.maximum_value != 0
            && semaphores.operations != 0
            && semaphores.operations <= 4096
            && semaphores.undo_entries != 0
            && semaphores.undo_entries <= (1 << 20)
    }

    fn validate_pipes(&self) -> Result<(), IpcCheckpointError> {
        let mut ids = BTreeSet::new();
        let mut descriptors = BTreeSet::new();
        for pipe in &self.pipes {
            let index = pipe.id.slot as usize;
            if index >= self.pipe_generations.len()
                || self.pipe_generations[index] != pipe.id.generation
                || !ids.insert(pipe.id)
                || !descriptors.insert(pipe.reader)
                || !descriptors.insert(pipe.writer)
                || pipe.snapshot.validate().is_err()
            {
                return Err(IpcCheckpointError::InvalidImage);
            }
        }
        Ok(())
    }

    fn validate_shared(&self) -> Result<(), IpcCheckpointError> {
        if self.shared.generations.len() > self.shared_limits.segments
            || self.shared.generations.contains(&0)
            || self.shared.attachments.len() > self.shared_limits.attachments
            || self.shared.next_attachment == 0
        {
            return Err(IpcCheckpointError::ResourceLimit);
        }
        let mut ids = BTreeMap::new();
        let mut keys = BTreeSet::new();
        let mut total = 0_usize;
        for segment in &self.shared.segments {
            let index = segment.id.slot as usize;
            total = total
                .checked_add(segment.size)
                .ok_or(IpcCheckpointError::ResourceLimit)?;
            if index >= self.shared.generations.len()
                || self.shared.generations[index] != segment.id.generation
                || segment.size == 0
                || segment.size > self.shared_limits.segment_bytes
                || segment.mode & !0o777 != 0
                || ids.insert(segment.id, segment).is_some()
                || Self::duplicate_shared_key(segment.key, &mut keys)
            {
                return Err(IpcCheckpointError::InvalidImage);
            }
        }
        if total > self.shared_limits.total_bytes {
            return Err(IpcCheckpointError::ResourceLimit);
        }
        self.validate_attachments(&ids)?;
        self.validate_backings(&ids)
    }

    fn duplicate_shared_key(key: Option<crate::IpcKey>, keys: &mut BTreeSet<crate::IpcKey>) -> bool {
        match key {
            Some(key) => !keys.insert(key),
            None => false,
        }
    }

    fn validate_attachments(
        &self,
        ids: &BTreeMap<SharedMemoryId, &crate::SharedMemoryMetadata>,
    ) -> Result<(), IpcCheckpointError> {
        let mut tokens = BTreeSet::new();
        let mut counts = BTreeMap::new();
        for (token, segment, process) in &self.shared.attachments {
            if *token == 0
                || *token >= self.shared.next_attachment
                || *process == 0
                || !tokens.insert(*token)
                || !ids.contains_key(segment)
            {
                return Err(IpcCheckpointError::InvalidImage);
            }
            *counts.entry(*segment).or_insert(0_usize) += 1;
        }
        for (id, metadata) in ids {
            if counts.get(id).copied().unwrap_or(0) != metadata.attaches
                || metadata.marked_for_removal && metadata.attaches == 0
            {
                return Err(IpcCheckpointError::InvalidImage);
            }
        }
        Ok(())
    }

    fn validate_backings(
        &self,
        ids: &BTreeMap<SharedMemoryId, &crate::SharedMemoryMetadata>,
    ) -> Result<(), IpcCheckpointError> {
        let mut segments = BTreeSet::new();
        let mut resources = BTreeSet::new();
        for backing in &self.backings {
            let Some(segment) = ids.get(&backing.segment) else {
                return Err(IpcCheckpointError::InvalidImage);
            };
            if segment.backing != backing.object
                || !segments.insert(backing.segment)
                || !resources.insert(backing.resource)
            {
                return Err(IpcCheckpointError::InvalidImage);
            }
        }
        if segments.len() != ids.len() {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn validate_messages(&self) -> Result<(), IpcCheckpointError> {
        if self.messages.generations.len() > self.message_limits.queues || self.messages.generations.contains(&0) {
            return Err(IpcCheckpointError::ResourceLimit);
        }
        crate::MessageQueueNamespace::restore(self.message_limits, self.messages.clone())
            .map_err(|_| IpcCheckpointError::InvalidImage)?;
        Ok(())
    }

    fn validate_semaphores(&self) -> Result<(), IpcCheckpointError> {
        if self.semaphores.generations.len() > self.semaphore_limits.sets || self.semaphores.generations.contains(&0) {
            return Err(IpcCheckpointError::ResourceLimit);
        }
        crate::SemaphoreNamespace::restore(self.semaphore_limits, self.semaphores.clone())
            .map_err(|_| IpcCheckpointError::InvalidImage)?;
        Ok(())
    }

    fn validate_tasks(&self) -> Result<(), IpcCheckpointError> {
        let mut tasks = BTreeMap::new();
        for task in &self.tasks {
            if task.process == 0 || tasks.insert(task.process, task.resource).is_some() {
                return Err(IpcCheckpointError::InvalidImage);
            }
        }
        let required = self
            .shared
            .attachments
            .iter()
            .map(|(_, _, process)| *process)
            .chain(self.semaphores.undo.iter().map(|(process, _, _, _)| *process))
            .collect::<BTreeSet<_>>();
        if required.iter().any(|process| !tasks.contains_key(process))
            || tasks.keys().any(|process| !required.contains(process))
        {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }
}
