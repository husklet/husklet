use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{SemaphoreError, SemaphoreId, SemaphoreNamespace};

type UndoKey = (u32, SemaphoreId, u16);

/// Mutation-free exec guard proving that process-owned `SEM_UNDO` is preserved.
pub struct PreparedSemaphoreExec {
    namespace: Arc<SemaphoreNamespace>,
    process: u32,
    expected: BTreeMap<UndoKey, i32>,
}

impl PreparedSemaphoreExec {
    pub fn commit(self) -> Result<(), SemaphoreError> {
        let state = self
            .namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = state
            .undo
            .iter()
            .filter(|((process, _, _), _)| *process == self.process)
            .map(|(key, value)| (*key, *value))
            .collect::<BTreeMap<_, _>>();
        if current != self.expected {
            return Err(SemaphoreError::InvalidArgument);
        }
        Ok(())
    }
}

impl SemaphoreNamespace {
    #[must_use]
    pub fn prepare_exec(self: &Arc<Self>, process: u32) -> PreparedSemaphoreExec {
        let expected = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .undo
            .iter()
            .filter(|((owner, _, _), _)| *owner == process)
            .map(|(key, value)| (*key, *value))
            .collect();
        PreparedSemaphoreExec {
            namespace: Arc::clone(self),
            process,
            expected,
        }
    }
}
