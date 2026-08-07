use std::sync::Arc;

use super::SharedBackingPin;
use crate::{SharedError, SharedObjectId, SharedSeal};

// `len` is a byte length, not a container count.
#[allow(clippy::len_without_is_empty)]
impl SharedBackingPin {
    #[must_use]
    pub fn id(&self) -> SharedObjectId {
        self.id
    }

    pub fn retain(&self) -> Result<Self, SharedError> {
        {
            let mut state = self
                .object
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let pins = state.pins.checked_add(1).ok_or(SharedError::ResourceLimit)?;
            let writable = if self.writable {
                state.writable_pins.checked_add(1).ok_or(SharedError::ResourceLimit)?
            } else {
                state.writable_pins
            };
            let write_shared = if self.write_shared {
                state
                    .write_shared_pins
                    .checked_add(1)
                    .ok_or(SharedError::ResourceLimit)?
            } else {
                state.write_shared_pins
            };
            state.pins = pins;
            state.writable_pins = writable;
            state.write_shared_pins = write_shared;
        }
        Ok(Self {
            id: self.id,
            object: Arc::clone(&self.object),
            writable: self.writable,
            write_shared: self.write_shared,
            store: Arc::clone(&self.store),
            activity: Arc::clone(&self.activity),
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.object.backing.len().unwrap_or(0)
    }

    pub fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), SharedError> {
        let _admission = self.activity.admit();
        self.object.backing.read(offset, output)
    }

    pub fn write(&self, offset: usize, input: &[u8]) -> Result<(), SharedError> {
        let _admission = self.activity.admit();
        if !self.writable {
            return Err(SharedError::Sealed);
        }
        let state = self
            .object
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.seals.contains(SharedSeal::WRITE) {
            return Err(SharedError::Sealed);
        }
        self.object.backing.write(offset, input)
    }

    pub(crate) fn compare_write_u32(&self, writes: &[(usize, u32, u32)]) -> Result<(), SharedError> {
        let _admission = self.activity.admit();
        if !self.writable {
            return Err(SharedError::Sealed);
        }
        let state = self
            .object
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.seals.contains(SharedSeal::WRITE) {
            return Err(SharedError::Sealed);
        }
        for &(offset, expected, _) in writes {
            offset.checked_add(4).ok_or(SharedError::Range)?;
            let mut bytes = [0_u8; 4];
            self.object.backing.read(offset, &mut bytes)?;
            if u32::from_le_bytes(bytes) != expected {
                return Err(SharedError::Busy);
            }
        }
        for &(offset, _, replacement) in writes {
            self.object.backing.write(offset, &replacement.to_le_bytes())?;
        }
        Ok(())
    }

    pub(crate) fn compare_apply_u32<E>(
        &self,
        offset: usize,
        expected: u32,
        apply: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Result<bool, E>, SharedError> {
        let _admission = self.activity.admit();
        let _state = self
            .object
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        offset.checked_add(4).ok_or(SharedError::Range)?;
        let mut bytes = [0_u8; 4];
        self.object.backing.read(offset, &mut bytes)?;
        if u32::from_le_bytes(bytes) != expected {
            return Ok(Ok(false));
        }
        Ok(apply().map(|()| true))
    }
}

impl Drop for SharedBackingPin {
    fn drop(&mut self) {
        let finalize = {
            let mut state = self
                .object
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.writable {
                state.writable_pins -= 1;
            }
            if self.write_shared {
                state.write_shared_pins -= 1;
            }
            state.pins -= 1;
            state.removed && state.pins == 0
        };
        if !finalize {
            return;
        }
        let mut store = self.store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = store.slots.get_mut(self.id.slot as usize) else {
            return;
        };
        if slot.generation != self.id.generation
            || slot
                .object
                .as_ref()
                .is_none_or(|object| !Arc::ptr_eq(object, &self.object))
        {
            return;
        }
        let state = self
            .object
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.removed || state.pins != 0 {
            return;
        }
        let size = self.object.backing.len().unwrap_or(0);
        drop(state);
        slot.object = None;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        store.allocated -= size;
    }
}
