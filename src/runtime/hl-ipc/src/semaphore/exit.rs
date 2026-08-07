use std::sync::Arc;

use crate::{SemaphoreError, SemaphoreNamespace};

use super::State;

pub struct PreparedSemaphoreExit {
    namespace: Arc<SemaphoreNamespace>,
    previous: State,
    published: State,
}

pub struct CommittedSemaphoreExit {
    namespace: Arc<SemaphoreNamespace>,
    previous: State,
    published: State,
}

impl PreparedSemaphoreExit {
    pub fn commit(self) -> Result<CommittedSemaphoreExit, SemaphoreError> {
        let mut state = self
            .namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state != self.previous {
            return Err(SemaphoreError::InvalidArgument);
        }
        *state = self.published.clone();
        drop(state);
        self.namespace.changed.notify_all();
        Ok(CommittedSemaphoreExit {
            namespace: self.namespace,
            previous: self.previous,
            published: self.published,
        })
    }
}

impl CommittedSemaphoreExit {
    pub fn rollback(self) -> Result<(), SemaphoreError> {
        let mut state = self
            .namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state != self.published {
            return Err(SemaphoreError::InvalidArgument);
        }
        *state = self.previous;
        drop(state);
        self.namespace.changed.notify_all();
        Ok(())
    }

    pub fn finish(self) {}
}

impl SemaphoreNamespace {
    pub fn prepare_exit(self: &Arc<Self>, process: u32, now: u64) -> Result<PreparedSemaphoreExit, SemaphoreError> {
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut published = previous.clone();
        let adjustments = published
            .undo
            .iter()
            .filter(|((owner, _, _), _)| *owner == process)
            .map(|(key, value)| (*key, *value))
            .collect::<Vec<_>>();
        published.undo.retain(|(owner, _, _), _| *owner != process);
        for ((_, id, index), adjustment) in adjustments {
            let slot = published
                .slots
                .get_mut(id.slot as usize)
                .ok_or(SemaphoreError::NotFound)?;
            if slot.generation != id.generation {
                return Err(SemaphoreError::NotFound);
            }
            let set = slot.set.as_mut().ok_or(SemaphoreError::Removed)?;
            let value = set.values.get_mut(index as usize).ok_or(SemaphoreError::Range)?;
            *value = (i32::from(*value) + adjustment).clamp(0, i32::from(self.limits.maximum_value)) as u16;
            set.last_pids[index as usize] = process;
            set.metadata.last_pid = process;
            set.metadata.operated_at = Some(now);
        }
        Ok(PreparedSemaphoreExit {
            namespace: Arc::clone(self),
            previous,
            published,
        })
    }
}
