use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_runtime::RuntimePathError;

use super::projected::File;

#[derive(Clone, Debug, Default)]
pub(super) struct Registry(Arc<Mutex<State>>);

#[derive(Debug, Default)]
struct State {
    files: BTreeMap<(u64, u64), std::sync::Weak<File>>,
    pending: usize,
}

impl Registry {
    pub(super) fn get(&self, identity: &(u64, u64)) -> Option<Arc<File>> {
        self.0.lock().ok()?.files.get(identity)?.upgrade()
    }

    pub(super) fn reserve(&self) -> Result<SlotReservation, RuntimePathError> {
        let mut state = self.0.lock().map_err(|_| RuntimePathError::Io)?;
        state.files.retain(|_, file| file.strong_count() != 0);
        if state
            .files
            .len()
            .checked_add(state.pending)
            .is_none_or(|count| count >= 1024)
        {
            return Err(RuntimePathError::TooLarge);
        }
        state.pending += 1;
        Ok(SlotReservation {
            registry: self.clone(),
            active: true,
        })
    }
}

pub(super) struct SlotReservation {
    registry: Registry,
    active: bool,
}

impl SlotReservation {
    pub(super) fn commit(mut self, file: &Arc<File>) -> Result<(), RuntimePathError> {
        let mut state = self.registry.0.lock().map_err(|_| RuntimePathError::Io)?;
        state.pending = state.pending.saturating_sub(1);
        state.files.insert(file.identity(), Arc::downgrade(file));
        drop(state);
        self.active = false;
        Ok(())
    }
}

impl Drop for SlotReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.registry.0.lock() {
            state.pending = state.pending.saturating_sub(1);
        }
    }
}
