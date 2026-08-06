//! Generational authority-owned object slots.

use super::TreeObject;

pub(super) struct Slot {
    pub(super) generation: u16,
    pub(super) object: Option<Box<dyn TreeObject>>,
}

pub(super) struct Slots(pub(super) Vec<Slot>);

impl Slots {
    pub(super) fn handle(index: usize, generation: u16) -> u64 {
        (u64::from(generation) << 32) | (index as u64 + 1)
    }

    pub(super) fn resolve(&mut self, value: u64) -> Result<&mut Box<dyn TreeObject>, i32> {
        self.resolve_slot(value)?.object.as_mut().ok_or(9)
    }

    pub(super) fn resolve_slot(&mut self, value: u64) -> Result<&mut Slot, i32> {
        let index = usize::try_from((value & 0xffff_ffff).checked_sub(1).ok_or(9)?).map_err(|_| 9)?;
        let generation = u16::try_from(value >> 32).map_err(|_| 9)?;
        let slot = self.0.get_mut(index).ok_or(9)?;
        if generation == 0 || slot.generation != generation || slot.object.is_none() {
            return Err(9);
        }
        Ok(slot)
    }
}
