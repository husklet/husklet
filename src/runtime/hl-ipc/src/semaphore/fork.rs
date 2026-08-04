use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{SemaphoreError, SemaphoreId};

use super::{SemaphoreNamespace, State};

type UndoKey = (u32, SemaphoreId, u16);

/// Mutation-free child SEM_UNDO reset plan.
pub struct PreparedSemaphoreFork {
    namespace: Arc<SemaphoreNamespace>,
    child: u32,
    expected: BTreeMap<UndoKey, i32>,
}

pub struct CommittedSemaphoreFork {
    namespace: Arc<SemaphoreNamespace>,
    previous: State,
    published: State,
}

impl PreparedSemaphoreFork {
    pub fn commit(self) -> Result<CommittedSemaphoreFork, SemaphoreError> {
        let mut state = self.namespace.lock();
        let current = state
            .undo
            .iter()
            .filter(|((process, _, _), _)| *process == self.child)
            .map(|(key, value)| (*key, *value))
            .collect::<BTreeMap<_, _>>();
        if current != self.expected {
            return Err(SemaphoreError::InvalidArgument);
        }
        let previous = state.clone();
        let mut published = previous.clone();
        published.undo.retain(|(process, _, _), _| *process != self.child);
        *state = published.clone();
        drop(state);
        Ok(CommittedSemaphoreFork {
            namespace: self.namespace,
            previous,
            published,
        })
    }
}

impl CommittedSemaphoreFork {
    pub fn rollback(self) -> Result<(), SemaphoreError> {
        let mut state = self.namespace.lock();
        if *state != self.published {
            return Err(SemaphoreError::InvalidArgument);
        }
        *state = self.previous;
        Ok(())
    }

    pub fn finish(self) {}
}

impl SemaphoreNamespace {
    #[must_use]
    pub fn prepare_fork_child(self: &Arc<Self>, child: u32) -> PreparedSemaphoreFork {
        let expected = self
            .lock()
            .undo
            .iter()
            .filter(|((process, _, _), _)| *process == child)
            .map(|(key, value)| (*key, *value))
            .collect();
        PreparedSemaphoreFork {
            namespace: Arc::clone(self),
            child,
            expected,
        }
    }
}
