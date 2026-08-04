use std::collections::BTreeMap;

use crate::{Credentials, SemaphoreError, SemaphoreId, SemaphoreNamespace, SemaphoreOperation};

use super::State;
use super::model::{SEM_FLAGS, SEM_UNDO};

impl SemaphoreNamespace {
    pub fn operate(
        &self,
        id: SemaphoreId,
        actor: Credentials,
        pid: u32,
        operations: &[SemaphoreOperation],
        now: u64,
    ) -> Result<(), SemaphoreError> {
        let mut state = self.lock();
        Self::require(&Self::set(&state, id)?.metadata, actor, 0o2)?;
        let values = self.evaluate(&state, id, operations)?;
        self.commit(&mut state, id, pid, operations, values, now)?;
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    pub(super) fn evaluate(
        &self,
        state: &State,
        id: SemaphoreId,
        operations: &[SemaphoreOperation],
    ) -> Result<Vec<u16>, SemaphoreError> {
        if operations.is_empty() || operations.len() > self.limits.operations {
            return Err(SemaphoreError::InvalidArgument);
        }
        let set = Self::set(state, id)?;
        let mut values = set.values.clone();
        for operation in operations {
            self.evaluate_one(&mut values, *operation)?;
        }
        Ok(values)
    }

    pub(super) fn commit(
        &self,
        state: &mut State,
        id: SemaphoreId,
        pid: u32,
        operations: &[SemaphoreOperation],
        values: Vec<u16>,
        now: u64,
    ) -> Result<(), SemaphoreError> {
        let additional = operations
            .iter()
            .filter(|operation| {
                operation.flags & SEM_UNDO != 0
                    && operation.delta != 0
                    && !state.undo.contains_key(&(pid, id, operation.index))
            })
            .count();
        if state.undo.len().saturating_add(additional) > self.limits.undo_entries {
            return Err(SemaphoreError::ResourceLimit);
        }
        let mut undo = state.undo.clone();
        for operation in operations {
            Self::apply_undo(&mut undo, pid, id, *operation)?;
        }
        let set = Self::set_mut(state, id)?;
        set.values = values;
        for operation in operations {
            set.last_pids[operation.index as usize] = pid;
        }
        set.metadata.last_pid = pid;
        set.metadata.operated_at = Some(now);
        state.undo = undo;
        Ok(())
    }

    fn evaluate_one(&self, values: &mut [u16], operation: SemaphoreOperation) -> Result<(), SemaphoreError> {
        if operation.flags & !SEM_FLAGS != 0 {
            return Err(SemaphoreError::InvalidArgument);
        }
        let value = values.get_mut(operation.index as usize).ok_or(SemaphoreError::Range)?;
        if operation.delta == 0 {
            return (*value == 0).then_some(()).ok_or(SemaphoreError::Again);
        }
        let next = i32::from(*value)
            .checked_add(operation.delta)
            .ok_or(SemaphoreError::Range)?;
        if next < 0 {
            return Err(SemaphoreError::Again);
        }
        if next > i32::from(self.limits.maximum_value) {
            return Err(SemaphoreError::Range);
        }
        *value = next as u16;
        Ok(())
    }

    fn apply_undo(
        undo: &mut BTreeMap<(u32, SemaphoreId, u16), i32>,
        pid: u32,
        id: SemaphoreId,
        operation: SemaphoreOperation,
    ) -> Result<(), SemaphoreError> {
        if operation.flags & SEM_UNDO == 0 {
            return Ok(());
        }
        let key = (pid, id, operation.index);
        let adjustment = undo.entry(key).or_default();
        *adjustment = adjustment.checked_sub(operation.delta).ok_or(SemaphoreError::Range)?;
        if *adjustment == 0 {
            undo.remove(&key);
        }
        Ok(())
    }
}
