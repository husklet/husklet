use std::sync::{Arc, Mutex};

use crate::backing::MemoryFactory;
use crate::shared_model::{
    SharedBackingRef, SharedError, SharedLimits, SharedObjectId, SharedObjectSnapshot, SharedSeal, SharedStoreSnapshot,
};
use crate::{SharedBacking, SharedBackingFactory};

#[path = "pin.rs"]
mod pin;

#[derive(Debug)]
struct ObjectState {
    owner: u64,
    seals: SharedSeal,
    pins: usize,
    writable_pins: usize,
    write_shared_pins: usize,
    removed: bool,
}

#[derive(Debug)]
struct Object {
    state: Mutex<ObjectState>,
    backing: Arc<dyn SharedBacking>,
}

#[derive(Debug)]
struct Slot {
    generation: u32,
    object: Option<Arc<Object>>,
}

#[derive(Debug)]
struct StoreState {
    slots: Vec<Slot>,
    allocated: usize,
}

#[derive(Debug)]
pub struct SharedObjectStore {
    limits: SharedLimits,
    state: Arc<Mutex<StoreState>>,
    activity: Arc<crate::CheckpointActivity>,
    factory: Arc<dyn SharedBackingFactory>,
    reservations: Arc<crate::ReservationEpochs>,
}

#[derive(Debug)]
pub struct SharedBackingPin {
    id: SharedObjectId,
    object: Arc<Object>,
    writable: bool,
    write_shared: bool,
    store: Arc<Mutex<StoreState>>,
    activity: Arc<crate::CheckpointActivity>,
}

impl SharedObjectStore {
    #[must_use]
    pub const fn limits(&self) -> SharedLimits {
        self.limits
    }

    pub fn pin_backing(&self, reference: SharedBackingRef, writable: bool) -> Result<SharedBackingPin, SharedError> {
        let _admission = self.activity.admit();
        let pin = self.pin_retained(reference.object, writable, reference.write_shared, false)?;
        let end = reference
            .offset
            .checked_add(reference.length)
            .ok_or(SharedError::Range)?;
        if end > pin.len() as u64 {
            return Err(SharedError::Range);
        }
        Ok(pin)
    }

    pub fn pin_inherited(&self, reference: SharedBackingRef, writable: bool) -> Result<SharedBackingPin, SharedError> {
        let pin = self.pin_retained(reference.object, writable, reference.write_shared, true)?;
        let end = reference
            .offset
            .checked_add(reference.length)
            .ok_or(SharedError::Range)?;
        if end > pin.len() as u64 {
            return Err(SharedError::Range);
        }
        Ok(pin)
    }
    pub fn new(limits: SharedLimits) -> Result<Self, SharedError> {
        Self::with_factory(limits, Arc::new(MemoryFactory))
    }

    pub fn with_factory(limits: SharedLimits, factory: Arc<dyn SharedBackingFactory>) -> Result<Self, SharedError> {
        if limits.objects == 0 || limits.object_bytes > limits.total_bytes {
            return Err(SharedError::InvalidArgument);
        }
        Ok(Self {
            limits,
            state: Arc::new(Mutex::new(StoreState {
                slots: Vec::new(),
                allocated: 0,
            })),
            activity: Arc::new(crate::CheckpointActivity::default()),
            factory,
            reservations: Arc::new(crate::ReservationEpochs::default()),
        })
    }

    #[must_use]
    pub fn reservation_epochs(&self) -> Arc<crate::ReservationEpochs> {
        Arc::clone(&self.reservations)
    }

    pub fn create(&self, owner: u64, size: usize) -> Result<SharedObjectId, SharedError> {
        let _admission = self.activity.admit();
        if size > self.limits.object_bytes {
            return Err(SharedError::ResourceLimit);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .allocated
            .checked_add(size)
            .is_none_or(|v| v > self.limits.total_bytes)
        {
            return Err(SharedError::ResourceLimit);
        }
        let index = state.slots.iter().position(|slot| slot.object.is_none());
        let index = match index {
            Some(index) => index,
            None if state.slots.len() < self.limits.objects => {
                state.slots.push(Slot {
                    generation: 1,
                    object: None,
                });
                state.slots.len() - 1
            }
            None => return Err(SharedError::ResourceLimit),
        };
        let slot = &mut state.slots[index];
        let id = SharedObjectId {
            slot: u32::try_from(index).map_err(|_| SharedError::ResourceLimit)?,
            generation: slot.generation,
        };
        let backing = self.factory.create(id, size)?;
        slot.object = Some(Arc::new(Object {
            state: Mutex::new(ObjectState {
                owner,
                seals: SharedSeal::default(),
                pins: 0,
                writable_pins: 0,
                write_shared_pins: 0,
                removed: false,
            }),
            backing,
        }));
        state.allocated += size;
        Ok(id)
    }

    pub fn pin(&self, id: SharedObjectId, writable: bool) -> Result<SharedBackingPin, SharedError> {
        self.pin_retained(id, writable, writable, false)
    }

    fn pin_retained(
        &self,
        id: SharedObjectId,
        writable: bool,
        write_shared: bool,
        removed: bool,
    ) -> Result<SharedBackingPin, SharedError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        {
            let mut state = object.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.removed && !removed {
                return Err(SharedError::NotFound);
            }
            if writable {
                if state.seals.intersects(SharedSeal::WRITE | SharedSeal::FUTURE_WRITE) {
                    return Err(SharedError::Sealed);
                }
                state.writable_pins += 1;
            }
            if write_shared {
                state.write_shared_pins += 1;
            }
            state.pins += 1;
        }
        Ok(SharedBackingPin {
            id,
            object,
            writable,
            write_shared,
            store: Arc::clone(&self.state),
            activity: self.activity.clone(),
        })
    }

    pub fn pin_count(&self, id: SharedObjectId) -> Result<usize, SharedError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let state = object.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state.pins)
    }

    pub fn resize(&self, id: SharedObjectId, size: usize) -> Result<(), SharedError> {
        let _admission = self.activity.admit();
        if size > self.limits.object_bytes {
            return Err(SharedError::ResourceLimit);
        }
        let mut store = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = store.slots.get(id.slot as usize).ok_or(SharedError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SharedError::NotFound);
        }
        let object = slot.object.clone().ok_or(SharedError::NotFound)?;
        let object_state = object.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let old = object.backing.len()?;
        if size < old && object_state.seals.contains(SharedSeal::SHRINK)
            || size > old && object_state.seals.contains(SharedSeal::GROW)
        {
            return Err(SharedError::Sealed);
        }
        let total = store.allocated - old + size;
        if total > self.limits.total_bytes {
            return Err(SharedError::ResourceLimit);
        }
        object.backing.resize(size)?;
        store.allocated = total;
        Ok(())
    }

    pub fn write_growing(&self, id: SharedObjectId, offset: usize, input: &[u8]) -> Result<usize, SharedError> {
        let _admission = self.activity.admit();
        let end = offset.checked_add(input.len()).ok_or(SharedError::Range)?;
        if end > self.limits.object_bytes {
            return Err(SharedError::ResourceLimit);
        }
        let mut store = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = store.slots.get(id.slot as usize).ok_or(SharedError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SharedError::NotFound);
        }
        let object = slot.object.clone().ok_or(SharedError::NotFound)?;
        let state = object.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.seals.contains(SharedSeal::WRITE) {
            return Err(SharedError::Sealed);
        }
        let old = object.backing.len()?;
        if end > old {
            if state.seals.contains(SharedSeal::GROW) {
                return Err(SharedError::Sealed);
            }
            let total = store
                .allocated
                .checked_sub(old)
                .and_then(|allocated| allocated.checked_add(end))
                .ok_or(SharedError::ResourceLimit)?;
            if total > self.limits.total_bytes {
                return Err(SharedError::ResourceLimit);
            }
            object.backing.resize(end)?;
            store.allocated = total;
        }
        object.backing.write(offset, input)?;
        Ok(input.len())
    }

    pub fn add_seals(&self, id: SharedObjectId, seals: SharedSeal) -> Result<SharedSeal, SharedError> {
        let _admission = self.activity.admit();
        let object = self.object(id)?;
        let mut state = object.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.seals.contains(SharedSeal::SEAL) {
            return Err(SharedError::Sealed);
        }
        if seals.contains(SharedSeal::WRITE) && state.write_shared_pins != 0 {
            return Err(SharedError::Busy);
        }
        state.seals = SharedSeal::from_bits(state.seals.bits() | seals.bits());
        Ok(state.seals)
    }

    pub fn remove(&self, id: SharedObjectId) -> Result<(), SharedError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state.slots.get_mut(id.slot as usize).ok_or(SharedError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SharedError::NotFound);
        }
        let object = slot.object.as_ref().ok_or(SharedError::NotFound)?;
        let mut object_state = object.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if object_state.removed {
            return Err(SharedError::NotFound);
        }
        if object_state.pins != 0 {
            object_state.removed = true;
            return Ok(());
        }
        let size = object.backing.len()?;
        drop(object_state);
        slot.object = None;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        state.allocated -= size;
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> SharedStoreSnapshot {
        let _admission = self.activity.admit();
        self.snapshot_state().expect("live shared backing remains readable")
    }

    pub fn checkpoint_snapshot(&self) -> Result<SharedStoreSnapshot, SharedError> {
        if !self.activity.frozen() {
            return Err(SharedError::Busy);
        }
        self.snapshot_state()
    }

    fn snapshot_state(&self) -> Result<SharedStoreSnapshot, SharedError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let objects = state
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| Self::snapshot_slot(index, slot).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        let generations = state.slots.iter().map(|slot| slot.generation).collect();
        Ok(SharedStoreSnapshot { generations, objects })
    }

    pub fn freeze_checkpoint(&self) {
        self.activity.freeze();
        drop(self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    pub fn thaw_checkpoint(&self) {
        self.activity.thaw();
    }

    fn snapshot_slot(index: usize, slot: &Slot) -> Result<Option<SharedObjectSnapshot>, SharedError> {
        let Some(object) = slot.object.as_ref() else {
            return Ok(None);
        };
        let object = object.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let backing = &slot.object.as_ref().ok_or(SharedError::NotFound)?.backing;
        let size = backing.len()?;
        let mut bytes = vec![0; size];
        backing.read(0, &mut bytes)?;
        Ok(Some(SharedObjectSnapshot {
            id: SharedObjectId {
                slot: index as u32,
                generation: slot.generation,
            },
            owner: object.owner,
            seals: object.seals,
            bytes,
        }))
    }

    pub fn restore(limits: SharedLimits, snapshot: SharedStoreSnapshot) -> Result<Self, SharedError> {
        Self::restore_with(limits, snapshot, Arc::new(MemoryFactory))
    }

    pub fn restore_with(
        limits: SharedLimits,
        snapshot: SharedStoreSnapshot,
        factory: Arc<dyn SharedBackingFactory>,
    ) -> Result<Self, SharedError> {
        if snapshot.generations.len() > limits.objects || snapshot.generations.contains(&0) {
            return Err(SharedError::InvalidArgument);
        }
        let mut slots = snapshot
            .generations
            .into_iter()
            .map(|generation| Slot {
                generation,
                object: None,
            })
            .collect::<Vec<_>>();
        let mut allocated = 0usize;
        for item in snapshot.objects {
            let index = item.id.slot as usize;
            if index >= slots.len() || item.bytes.len() > limits.object_bytes {
                return Err(SharedError::ResourceLimit);
            }
            allocated = allocated
                .checked_add(item.bytes.len())
                .ok_or(SharedError::ResourceLimit)?;
            if allocated > limits.total_bytes {
                return Err(SharedError::ResourceLimit);
            }
            if slots[index].object.is_some() || item.id.generation == 0 || slots[index].generation != item.id.generation
            {
                return Err(SharedError::InvalidArgument);
            }
            let backing = factory.create(item.id, item.bytes.len())?;
            backing.write(0, &item.bytes)?;
            slots[index] = Slot {
                generation: item.id.generation,
                object: Some(Arc::new(Object {
                    state: Mutex::new(ObjectState {
                        owner: item.owner,
                        seals: item.seals,
                        pins: 0,
                        writable_pins: 0,
                        write_shared_pins: 0,
                        removed: false,
                    }),
                    backing,
                })),
            };
        }
        Ok(Self {
            limits,
            state: Arc::new(Mutex::new(StoreState { slots, allocated })),
            activity: Arc::new(crate::CheckpointActivity::default()),
            factory,
            reservations: Arc::new(crate::ReservationEpochs::default()),
        })
    }

    fn object(&self, id: SharedObjectId) -> Result<Arc<Object>, SharedError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state.slots.get(id.slot as usize).ok_or(SharedError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SharedError::NotFound);
        }
        slot.object.clone().ok_or(SharedError::NotFound)
    }
}
