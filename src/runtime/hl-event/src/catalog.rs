use std::sync::{Arc, Mutex};

use crate::{
    EVENT_CHECKPOINT_OBJECT_MAXIMUM, EVENT_CHECKPOINT_VERSION, Epoll, EpollTargetCheckpoint, EventCatalogRestore,
    EventCheckpointError, EventCheckpointImage, EventFd, EventObjectCheckpoint, EventObjectId, EventObjectState,
    EventResourceKey, Inotify, InotifyWatchCheckpoint, SignalFd, TimerFd,
};

#[derive(Clone)]
enum CatalogObject {
    EventFd(Arc<EventFd>),
    TimerFd {
        object: Arc<TimerFd>,
        clock: EventResourceKey,
    },
    SignalFd {
        object: Arc<SignalFd>,
        task_queue: EventResourceKey,
    },
    Epoll {
        object: Arc<Epoll>,
        targets: Vec<EpollTargetCheckpoint>,
    },
    Inotify {
        object: Arc<Inotify>,
        source: EventResourceKey,
        watches: Vec<InotifyWatchCheckpoint>,
    },
}

struct Slot {
    generation: u32,
    object: Option<CatalogObject>,
}

struct CatalogState {
    slots: Vec<Slot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventCatalogError {
    InvalidCapacity,
    Full,
    NotFound,
    WrongKind,
    Checkpoint(EventCheckpointError),
}

impl From<EventCheckpointError> for EventCatalogError {
    fn from(error: EventCheckpointError) -> Self {
        Self::Checkpoint(error)
    }
}

pub struct EventCatalog {
    capacity: usize,
    state: Mutex<CatalogState>,
    activity: Arc<crate::checkpoint_activity::CheckpointActivity>,
}

impl EventCatalog {
    pub fn restore_checkpoint(
        image: &EventCheckpointImage,
        restore: &mut dyn EventCatalogRestore,
    ) -> Result<Self, EventCatalogError> {
        image.validate()?;
        let mut slots = image
            .generations
            .iter()
            .copied()
            .map(|generation| Slot {
                generation,
                object: None,
            })
            .collect::<Vec<_>>();
        for item in image
            .objects
            .iter()
            .filter(|item| !matches!(item.state, EventObjectState::Epoll { .. }))
        {
            Self::restore_item(item, &mut slots, restore)?;
        }
        let mut pending = image
            .objects
            .iter()
            .filter(|item| matches!(item.state, EventObjectState::Epoll { .. }))
            .collect::<Vec<_>>();
        while !pending.is_empty() {
            let Some(index) = pending.iter().position(|item| Self::epoll_ready(item, &slots)) else {
                return Err(EventCatalogError::Checkpoint(EventCheckpointError::Cycle));
            };
            let item = pending.remove(index);
            Self::restore_item(item, &mut slots, restore)?;
        }
        Ok(Self {
            capacity: image.generations.len(),
            state: Mutex::new(CatalogState { slots }),
            activity: Arc::new(crate::checkpoint_activity::CheckpointActivity::default()),
        })
    }

    fn epoll_ready(item: &EventObjectCheckpoint, slots: &[Slot]) -> bool {
        let EventObjectState::Epoll { targets, .. } = &item.state else {
            return false;
        };
        targets
            .iter()
            .filter_map(|target| target.nested)
            .all(|id| slots.get(id.slot as usize).is_some_and(|slot| slot.object.is_some()))
    }

    pub fn new(capacity: usize) -> Result<Self, EventCatalogError> {
        if capacity == 0 || capacity > EVENT_CHECKPOINT_OBJECT_MAXIMUM {
            return Err(EventCatalogError::InvalidCapacity);
        }
        Ok(Self {
            capacity,
            state: Mutex::new(CatalogState { slots: Vec::new() }),
            activity: Arc::new(crate::checkpoint_activity::CheckpointActivity::default()),
        })
    }

    pub fn insert_eventfd(&self, object: Arc<EventFd>) -> Result<EventObjectId, EventCatalogError> {
        self.insert(CatalogObject::EventFd(object))
    }

    pub fn insert_timerfd(
        &self,
        object: Arc<TimerFd>,
        clock: EventResourceKey,
    ) -> Result<EventObjectId, EventCatalogError> {
        self.insert(CatalogObject::TimerFd { object, clock })
    }

    pub fn insert_signalfd(
        &self,
        object: Arc<SignalFd>,
        task_queue: EventResourceKey,
    ) -> Result<EventObjectId, EventCatalogError> {
        self.insert(CatalogObject::SignalFd { object, task_queue })
    }

    pub fn insert_epoll(
        &self,
        object: Arc<Epoll>,
        targets: Vec<EpollTargetCheckpoint>,
    ) -> Result<EventObjectId, EventCatalogError> {
        self.insert(CatalogObject::Epoll { object, targets })
    }

    pub fn insert_inotify(
        &self,
        object: Arc<Inotify>,
        source: EventResourceKey,
        watches: Vec<InotifyWatchCheckpoint>,
    ) -> Result<EventObjectId, EventCatalogError> {
        self.insert(CatalogObject::Inotify {
            object,
            source,
            watches,
        })
    }

    pub fn replace_epoll_targets(
        &self,
        id: EventObjectId,
        targets: Vec<EpollTargetCheckpoint>,
    ) -> Result<(), EventCatalogError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = Self::slot_mut(&mut state, id)?;
        let Some(CatalogObject::Epoll {
            object,
            targets: current,
        }) = slot.object.as_mut()
        else {
            return Err(EventCatalogError::WrongKind);
        };
        if object.snapshot().watches.len() != targets.len() {
            return Err(EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage));
        }
        *current = targets;
        Ok(())
    }

    pub fn add_epoll_target(&self, id: EventObjectId, target: EpollTargetCheckpoint) -> Result<(), EventCatalogError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = Self::slot_mut(&mut state, id)?;
        let Some(CatalogObject::Epoll { object, targets }) = slot.object.as_mut() else {
            return Err(EventCatalogError::WrongKind);
        };
        if object.watch_count() != targets.len() + 1 || target.watch != targets.len() {
            return Err(EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage));
        }
        targets.push(target);
        Ok(())
    }

    pub fn remove_epoll_target(
        &self,
        id: EventObjectId,
        descriptor: EventResourceKey,
    ) -> Result<(), EventCatalogError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = Self::slot_mut(&mut state, id)?;
        let Some(CatalogObject::Epoll { object, targets }) = slot.object.as_mut() else {
            return Err(EventCatalogError::WrongKind);
        };
        let index = targets
            .iter()
            .position(|target| target.descriptor == descriptor)
            .ok_or(EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage))?;
        targets.remove(index);
        for (watch, target) in targets.iter_mut().enumerate().skip(index) {
            target.watch = watch;
        }
        if object.watch_count() != targets.len() {
            return Err(EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage));
        }
        Ok(())
    }

    pub fn replace_inotify_watches(
        &self,
        id: EventObjectId,
        watches: Vec<InotifyWatchCheckpoint>,
    ) -> Result<(), EventCatalogError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = Self::slot_mut(&mut state, id)?;
        let Some(CatalogObject::Inotify {
            object,
            watches: current,
            ..
        }) = slot.object.as_mut()
        else {
            return Err(EventCatalogError::WrongKind);
        };
        if object.snapshot().watches.len() != watches.len() {
            return Err(EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage));
        }
        *current = watches;
        Ok(())
    }

    fn insert(&self, object: CatalogObject) -> Result<EventObjectId, EventCatalogError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = state.slots.iter().position(|slot| slot.object.is_none());
        let index = match index {
            Some(index) => index,
            None if state.slots.len() < self.capacity => {
                state.slots.push(Slot {
                    generation: 0,
                    object: None,
                });
                state.slots.len() - 1
            }
            None => return Err(EventCatalogError::Full),
        };
        let slot = &mut state.slots[index];
        slot.generation = slot.generation.wrapping_add(1).max(1);
        slot.object = Some(object);
        Ok(EventObjectId {
            slot: u32::try_from(index).map_err(|_| EventCatalogError::Full)?,
            generation: slot.generation,
        })
    }

    pub fn remove(&self, id: EventObjectId) -> Result<(), EventCatalogError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::slot_mut(&mut state, id)?
            .object
            .take()
            .ok_or(EventCatalogError::NotFound)?;
        Ok(())
    }

    pub fn with_eventfd<R>(
        &self,
        id: EventObjectId,
        operation: impl FnOnce(&EventFd) -> R,
    ) -> Result<R, EventCatalogError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let CatalogObject::EventFd(object) = object else {
            return Err(EventCatalogError::WrongKind);
        };
        Ok(operation(&object))
    }

    pub fn with_timerfd<R>(
        &self,
        id: EventObjectId,
        operation: impl FnOnce(&TimerFd) -> R,
    ) -> Result<R, EventCatalogError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let CatalogObject::TimerFd { object, .. } = object else {
            return Err(EventCatalogError::WrongKind);
        };
        Ok(operation(&object))
    }

    pub fn with_signalfd<R>(
        &self,
        id: EventObjectId,
        operation: impl FnOnce(&SignalFd) -> R,
    ) -> Result<R, EventCatalogError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let CatalogObject::SignalFd { object, .. } = object else {
            return Err(EventCatalogError::WrongKind);
        };
        Ok(operation(&object))
    }

    pub fn with_epoll<R>(
        &self,
        id: EventObjectId,
        operation: impl FnOnce(&Epoll) -> R,
    ) -> Result<R, EventCatalogError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let CatalogObject::Epoll { object, .. } = object else {
            return Err(EventCatalogError::WrongKind);
        };
        Ok(operation(&object))
    }

    pub fn with_inotify<R>(
        &self,
        id: EventObjectId,
        operation: impl FnOnce(&Inotify) -> R,
    ) -> Result<R, EventCatalogError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let CatalogObject::Inotify { object, .. } = object else {
            return Err(EventCatalogError::WrongKind);
        };
        Ok(operation(&object))
    }

    pub fn inotify_source(&self, id: EventObjectId) -> Result<EventResourceKey, EventCatalogError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let CatalogObject::Inotify { source, .. } = object else {
            return Err(EventCatalogError::WrongKind);
        };
        Ok(source)
    }

    fn object(&self, id: EventObjectId) -> Result<CatalogObject, EventCatalogError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::slot(&state, id)?
            .object
            .clone()
            .ok_or(EventCatalogError::NotFound)
    }

    fn slot(state: &CatalogState, id: EventObjectId) -> Result<&Slot, EventCatalogError> {
        let slot = state.slots.get(id.slot as usize).ok_or(EventCatalogError::NotFound)?;
        if slot.generation != id.generation {
            return Err(EventCatalogError::NotFound);
        }
        Ok(slot)
    }

    fn slot_mut(state: &mut CatalogState, id: EventObjectId) -> Result<&mut Slot, EventCatalogError> {
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .ok_or(EventCatalogError::NotFound)?;
        if slot.generation != id.generation {
            return Err(EventCatalogError::NotFound);
        }
        Ok(slot)
    }

    pub fn freeze_checkpoint(&self) {
        self.activity.freeze();
        drop(self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    pub fn thaw_checkpoint(&self) {
        self.activity.thaw();
    }

    pub fn checkpoint_image(&self) -> Result<EventCheckpointImage, EventCatalogError> {
        if !self.activity.frozen() {
            return Err(EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage));
        }
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut objects = Vec::new();
        for (slot, value) in state.slots.iter().enumerate() {
            let Some(object) = value.object.as_ref() else { continue };
            objects.push(EventObjectCheckpoint {
                id: EventObjectId {
                    slot: slot as u32,
                    generation: value.generation,
                },
                state: Self::snapshot_object(object)?,
            });
        }
        let image = EventCheckpointImage {
            version: EVENT_CHECKPOINT_VERSION,
            generations: state.slots.iter().map(|slot| slot.generation).collect(),
            objects,
        };
        image.validate()?;
        Ok(image)
    }

    fn snapshot_object(object: &CatalogObject) -> Result<EventObjectState, EventCatalogError> {
        match object {
            CatalogObject::EventFd(object) => Ok(EventObjectState::EventFd(object.snapshot())),
            CatalogObject::TimerFd { object, clock } => Ok(EventObjectState::TimerFd {
                snapshot: object
                    .snapshot()
                    .map_err(|_| EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage))?,
                clock: *clock,
            }),
            CatalogObject::SignalFd { object, task_queue } => Ok(EventObjectState::SignalFd {
                snapshot: object.snapshot(),
                task_queue: *task_queue,
            }),
            CatalogObject::Epoll { object, targets } => Ok(EventObjectState::Epoll {
                snapshot: object.snapshot(),
                targets: targets.clone(),
            }),
            CatalogObject::Inotify {
                object,
                source,
                watches,
            } => Ok(EventObjectState::Inotify {
                snapshot: object.snapshot(),
                source: *source,
                watches: watches.clone(),
            }),
        }
    }

    fn restore_object(
        state: &EventObjectState,
        restore: &mut dyn EventCatalogRestore,
    ) -> Result<CatalogObject, EventCatalogError> {
        Ok(match state {
            EventObjectState::EventFd(snapshot) => CatalogObject::EventFd(Arc::new(
                EventFd::from_snapshot(*snapshot)
                    .map_err(|_| EventCatalogError::Checkpoint(EventCheckpointError::InvalidImage))?,
            )),
            EventObjectState::TimerFd { snapshot, clock } => CatalogObject::TimerFd {
                object: restore.timerfd(*snapshot, *clock)?,
                clock: *clock,
            },
            EventObjectState::SignalFd { snapshot, task_queue } => CatalogObject::SignalFd {
                object: restore.signalfd(*snapshot, *task_queue)?,
                task_queue: *task_queue,
            },
            EventObjectState::Epoll { snapshot, targets } => CatalogObject::Epoll {
                object: restore.epoll(snapshot, targets)?,
                targets: targets.clone(),
            },
            EventObjectState::Inotify {
                snapshot,
                source,
                watches,
            } => CatalogObject::Inotify {
                object: restore.inotify(snapshot, *source, watches)?,
                source: *source,
                watches: watches.clone(),
            },
        })
    }

    fn restore_item(
        item: &EventObjectCheckpoint,
        slots: &mut [Slot],
        restore: &mut dyn EventCatalogRestore,
    ) -> Result<(), EventCatalogError> {
        let object = Self::restore_object(&item.state, restore)?;
        restore.bind(item.id, Self::description(&object))?;
        slots[item.id.slot as usize].object = Some(object);
        Ok(())
    }

    fn description(object: &CatalogObject) -> Arc<dyn hl_descriptor::OpenFileDescription> {
        match object {
            CatalogObject::EventFd(object) => object.clone(),
            CatalogObject::TimerFd { object, .. } => object.clone(),
            CatalogObject::SignalFd { object, .. } => object.clone(),
            CatalogObject::Epoll { object, .. } => object.clone(),
            CatalogObject::Inotify { object, .. } => object.clone(),
        }
    }
}
