use super::arena::Operation;
use super::virtual_memory::{ArenaState, Memory, MemoryError};

impl Memory {
    pub(super) fn failed_operation(
        &self,
        state: &mut ArenaState,
        applied: Vec<Operation>,
        error: MemoryError,
    ) -> Result<(), MemoryError> {
        if self.compensate(applied).is_err() || error == MemoryError::Poisoned {
            self.poison(state);
        }
        Err(error)
    }

    pub(super) fn compensate(&self, applied: Vec<Operation>) -> Result<(), MemoryError> {
        for previous in applied.into_iter().rev() {
            self.apply_host(previous)?;
        }
        Ok(())
    }

    pub(super) fn poison(&self, state: &mut ArenaState) {
        self.disable();
        state.poisoned = true;
    }
}
