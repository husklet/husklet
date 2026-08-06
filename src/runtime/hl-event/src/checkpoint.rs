use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use crate::inotify::model::INOTIFY_HEADER_SIZE;
use crate::{
    Epoll, EpollSnapshot, EventFdSnapshot, Inotify, InotifySnapshot, SignalFd, SignalFdSnapshot, TimerFd,
    TimerFdSnapshot,
};
use hl_descriptor::OpenFileDescription;
use std::sync::Arc;

pub const EVENT_CHECKPOINT_VERSION: u32 = 2;
pub const EVENT_CHECKPOINT_OBJECT_MAXIMUM: usize = 1 << 20;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EventResourceKey(NonZeroU64);

impl EventResourceKey {
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EventObjectId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventObjectState {
    EventFd(EventFdSnapshot),
    TimerFd {
        snapshot: TimerFdSnapshot,
        clock: EventResourceKey,
    },
    SignalFd {
        snapshot: SignalFdSnapshot,
        task_queue: EventResourceKey,
    },
    Epoll {
        snapshot: EpollSnapshot,
        targets: Vec<EpollTargetCheckpoint>,
    },
    Inotify {
        snapshot: InotifySnapshot,
        source: EventResourceKey,
        watches: Vec<InotifyWatchCheckpoint>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpollTargetCheckpoint {
    pub watch: usize,
    pub descriptor: EventResourceKey,
    pub nested: Option<EventObjectId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InotifyWatchCheckpoint {
    pub watch: usize,
    pub source: EventResourceKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventObjectCheckpoint {
    pub id: EventObjectId,
    pub state: EventObjectState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventCheckpointImage {
    pub version: u32,
    pub generations: Vec<u32>,
    pub objects: Vec<EventObjectCheckpoint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventCheckpointError {
    InvalidImage,
    ResourceLimit,
    Cycle,
}

pub trait EventCatalogRestore: Send {
    fn timerfd(
        &mut self,
        snapshot: TimerFdSnapshot,
        clock: EventResourceKey,
    ) -> Result<Arc<TimerFd>, EventCheckpointError>;
    fn signalfd(
        &mut self,
        snapshot: SignalFdSnapshot,
        task_queue: EventResourceKey,
    ) -> Result<Arc<SignalFd>, EventCheckpointError>;
    fn epoll(
        &mut self,
        snapshot: &EpollSnapshot,
        targets: &[EpollTargetCheckpoint],
    ) -> Result<Arc<Epoll>, EventCheckpointError>;
    fn inotify(
        &mut self,
        snapshot: &InotifySnapshot,
        source: EventResourceKey,
        watches: &[InotifyWatchCheckpoint],
    ) -> Result<Arc<Inotify>, EventCheckpointError>;
    fn bind(&mut self, _id: EventObjectId, _object: Arc<dyn OpenFileDescription>) -> Result<(), EventCheckpointError> {
        Ok(())
    }
    fn commit(&mut self) -> Result<(), EventCheckpointError>;
    fn rollback(&mut self);
    fn resume(&mut self) -> Result<(), EventCheckpointError>;
}

pub trait EventCheckpointRebind: Send + Sync {
    fn stage(&self, image: &EventCheckpointImage) -> Result<Box<dyn EventCatalogRestore>, EventCheckpointError>;
}

impl EventCheckpointImage {
    pub fn validate(&self) -> Result<(), EventCheckpointError> {
        if self.version != EVENT_CHECKPOINT_VERSION
            || self.generations.len() > EVENT_CHECKPOINT_OBJECT_MAXIMUM
            || self.objects.len() > EVENT_CHECKPOINT_OBJECT_MAXIMUM
        {
            return Err(EventCheckpointError::ResourceLimit);
        }
        let mut ids = BTreeSet::new();
        for object in &self.objects {
            if object.id.generation == 0
                || object.id.slot as usize >= self.generations.len()
                || self.generations[object.id.slot as usize] != object.id.generation
                || !ids.insert(object.id)
            {
                return Err(EventCheckpointError::InvalidImage);
            }
            Self::validate_object(object)?;
        }
        self.validate_graph(&ids)
    }

    fn validate_object(object: &EventObjectCheckpoint) -> Result<(), EventCheckpointError> {
        match &object.state {
            EventObjectState::EventFd(snapshot) if snapshot.counter == u64::MAX => {
                Err(EventCheckpointError::InvalidImage)
            }
            EventObjectState::Epoll { snapshot, targets } => Self::validate_epoll(snapshot, targets),
            EventObjectState::Inotify { snapshot, watches, .. } => Self::validate_inotify(snapshot, watches),
            _ => Ok(()),
        }
    }

    fn validate_epoll(snapshot: &EpollSnapshot, targets: &[EpollTargetCheckpoint]) -> Result<(), EventCheckpointError> {
        let keys = snapshot.watches.iter().map(|watch| watch.key).collect::<BTreeSet<_>>();
        let ready = snapshot.ready.iter().copied().collect::<BTreeSet<_>>();
        if snapshot.watch_limit == 0
            || snapshot.next_token == 0
            || snapshot.watches.len() > snapshot.watch_limit
            || keys.len() != snapshot.watches.len()
            || ready.len() != snapshot.ready.len()
            || snapshot.ready.iter().any(|key| !keys.contains(key))
            || snapshot.watches.len() != targets.len()
            || targets.iter().enumerate().any(|(index, target)| target.watch != index)
        {
            return Err(EventCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn validate_inotify(
        snapshot: &InotifySnapshot,
        watches: &[InotifyWatchCheckpoint],
    ) -> Result<(), EventCheckpointError> {
        if snapshot.limits.watches == 0
            || snapshot.limits.queued_events < 2
            || snapshot.limits.name_bytes == 0
            || snapshot.watch_generations.len() > snapshot.limits.watches
            || snapshot.watches.len() != watches.len()
            || snapshot.queue.len() > snapshot.limits.queued_events
        {
            return Err(EventCheckpointError::InvalidImage);
        }
        let mut descriptors = BTreeSet::new();
        for (index, watch) in snapshot.watches.iter().enumerate() {
            let slot = usize::try_from(watch.watch_descriptor)
                .ok()
                .and_then(|value| value.checked_sub(1))
                .ok_or(EventCheckpointError::InvalidImage)?;
            if slot >= snapshot.watch_generations.len()
                || snapshot.watch_generations[slot] != watch.generation
                || watch.generation == 0
                || !descriptors.insert(watch.watch_descriptor)
                || watches[index].watch != index
            {
                return Err(EventCheckpointError::InvalidImage);
            }
        }
        if snapshot
            .queue
            .iter()
            .any(|event| event.name.len() > snapshot.limits.name_bytes)
        {
            return Err(EventCheckpointError::InvalidImage);
        }
        let bytes = snapshot.queue.iter().try_fold(0_usize, |total, event| {
            let padded = event
                .name
                .len()
                .checked_add(3)
                .map(|value| value & !3)
                .ok_or(EventCheckpointError::ResourceLimit)?;
            total
                .checked_add(INOTIFY_HEADER_SIZE)
                .and_then(|value| value.checked_add(padded))
                .ok_or(EventCheckpointError::ResourceLimit)
        })?;
        if bytes > snapshot.limits.queued_bytes {
            return Err(EventCheckpointError::ResourceLimit);
        }
        Ok(())
    }

    fn validate_graph(&self, ids: &BTreeSet<EventObjectId>) -> Result<(), EventCheckpointError> {
        let kinds = self
            .objects
            .iter()
            .map(|object| (object.id, matches!(&object.state, EventObjectState::Epoll { .. })))
            .collect::<BTreeMap<_, _>>();
        let edges = self
            .objects
            .iter()
            .filter_map(|object| {
                let EventObjectState::Epoll { targets, .. } = &object.state else {
                    return None;
                };
                Some((
                    object.id,
                    targets.iter().filter_map(|target| target.nested).collect::<Vec<_>>(),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        for targets in edges.values() {
            if Self::has_invalid_target(targets, ids, &kinds) {
                return Err(EventCheckpointError::InvalidImage);
            }
        }
        for root in edges.keys() {
            let mut visiting = BTreeSet::new();
            if Self::cyclic(*root, &edges, &mut visiting) {
                return Err(EventCheckpointError::Cycle);
            }
        }
        Ok(())
    }

    fn has_invalid_target(
        targets: &[EventObjectId],
        ids: &BTreeSet<EventObjectId>,
        kinds: &BTreeMap<EventObjectId, bool>,
    ) -> bool {
        targets
            .iter()
            .any(|target| !ids.contains(target) || kinds.get(target) != Some(&true))
    }

    fn cyclic(
        node: EventObjectId,
        edges: &BTreeMap<EventObjectId, Vec<EventObjectId>>,
        visiting: &mut BTreeSet<EventObjectId>,
    ) -> bool {
        if !visiting.insert(node) {
            return true;
        }
        let cyclic = edges
            .get(&node)
            .is_some_and(|targets| targets.iter().any(|target| Self::cyclic(*target, edges, visiting)));
        visiting.remove(&node);
        cyclic
    }
}
