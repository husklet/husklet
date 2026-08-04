use std::sync::Arc;

use hl_descriptor::{DescriptorTable, OpenFileDescription, OperationLease};
use hl_event::{
    Epoll, EpollSnapshot, EpollTargetCheckpoint, EventCatalogRestore, EventCheckpointError, EventCheckpointImage,
    EventCheckpointRebind, EventObjectId, EventResourceKey, Inotify, InotifySnapshot, InotifyWatchCheckpoint, SignalFd,
    SignalFdSnapshot, TimerFd, TimerFdSnapshot,
};

use crate::CheckpointDescriptorTable;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DescriptorReference {
    pub number: i32,
    pub generation: u32,
}

pub trait BindingRestore: Send + Sync {
    fn descriptor(&self, key: EventResourceKey) -> Result<DescriptorReference, EventCheckpointError>;

    fn timerfd(&self, snapshot: TimerFdSnapshot, clock: EventResourceKey)
    -> Result<Arc<TimerFd>, EventCheckpointError>;

    fn signalfd(
        &self,
        snapshot: SignalFdSnapshot,
        task_queue: EventResourceKey,
    ) -> Result<Arc<SignalFd>, EventCheckpointError>;

    fn inotify(
        &self,
        snapshot: &InotifySnapshot,
        source: EventResourceKey,
        watches: &[InotifyWatchCheckpoint],
    ) -> Result<Arc<Inotify>, EventCheckpointError>;

    fn bind(&self, id: EventObjectId, object: Arc<dyn OpenFileDescription>) -> Result<(), EventCheckpointError>;

    fn commit(&self) -> Result<(), EventCheckpointError>;
    fn rollback(&self);
    fn resume(&self) -> Result<(), EventCheckpointError>;
}

pub struct DescriptorRebind {
    descriptors: Arc<CheckpointDescriptorTable>,
    bindings: Arc<dyn BindingRestore>,
}

impl DescriptorRebind {
    #[must_use]
    pub fn new(descriptors: Arc<CheckpointDescriptorTable>, bindings: Arc<dyn BindingRestore>) -> Self {
        Self { descriptors, bindings }
    }
}

impl EventCheckpointRebind for DescriptorRebind {
    fn stage(&self, _: &EventCheckpointImage) -> Result<Box<dyn EventCatalogRestore>, EventCheckpointError> {
        let descriptors = self.descriptors.staged().ok_or(EventCheckpointError::InvalidImage)?;
        Ok(Box::new(RestoreTransaction {
            descriptors,
            bindings: self.bindings.clone(),
        }))
    }
}

struct RestoreTransaction {
    descriptors: Arc<DescriptorTable>,
    bindings: Arc<dyn BindingRestore>,
}

impl RestoreTransaction {
    fn target(&self, key: EventResourceKey) -> Result<OperationLease, EventCheckpointError> {
        let reference = self.bindings.descriptor(key)?;
        let lease = self
            .descriptors
            .pin_checkpoint(reference.number)
            .map_err(|_| EventCheckpointError::InvalidImage)?;
        if lease.descriptor_generation() != reference.generation {
            return Err(EventCheckpointError::InvalidImage);
        }
        Ok(lease)
    }
}

impl EventCatalogRestore for RestoreTransaction {
    fn timerfd(
        &mut self,
        snapshot: TimerFdSnapshot,
        clock: EventResourceKey,
    ) -> Result<Arc<TimerFd>, EventCheckpointError> {
        self.bindings.timerfd(snapshot, clock)
    }

    fn signalfd(
        &mut self,
        snapshot: SignalFdSnapshot,
        task_queue: EventResourceKey,
    ) -> Result<Arc<SignalFd>, EventCheckpointError> {
        self.bindings.signalfd(snapshot, task_queue)
    }

    fn epoll(
        &mut self,
        snapshot: &EpollSnapshot,
        targets: &[EpollTargetCheckpoint],
    ) -> Result<Arc<Epoll>, EventCheckpointError> {
        let leases = targets
            .iter()
            .map(|target| self.target(target.descriptor))
            .collect::<Result<Vec<_>, _>>()?;
        Epoll::from_snapshot(snapshot, leases)
            .map(Arc::new)
            .map_err(|_| EventCheckpointError::InvalidImage)
    }

    fn inotify(
        &mut self,
        snapshot: &InotifySnapshot,
        source: EventResourceKey,
        watches: &[InotifyWatchCheckpoint],
    ) -> Result<Arc<Inotify>, EventCheckpointError> {
        self.bindings.inotify(snapshot, source, watches)
    }

    fn bind(&mut self, id: EventObjectId, object: Arc<dyn OpenFileDescription>) -> Result<(), EventCheckpointError> {
        self.bindings.bind(id, object)
    }

    fn commit(&mut self) -> Result<(), EventCheckpointError> {
        self.bindings.commit()
    }

    fn rollback(&mut self) {
        self.bindings.rollback();
    }

    fn resume(&mut self) -> Result<(), EventCheckpointError> {
        self.bindings.resume()
    }
}
