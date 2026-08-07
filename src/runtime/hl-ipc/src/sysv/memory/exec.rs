use std::sync::Arc;

use hl_memory::SharedObjectId;

use crate::{SharedMemoryError, SharedMemoryNamespace};

use super::NamespaceState;

/// Mutation-free plan for detaching every `SysV` mapping owned by one process.
pub struct PreparedMemoryExec {
    namespace: Arc<SharedMemoryNamespace>,
    previous: NamespaceState,
    published: NamespaceState,
    retired: Vec<SharedObjectId>,
    attachments: Vec<u64>,
}

pub struct CommittedMemoryExec {
    namespace: Arc<SharedMemoryNamespace>,
    previous: NamespaceState,
    published: NamespaceState,
    retired: Vec<SharedObjectId>,
}

impl PreparedMemoryExec {
    #[must_use]
    pub fn attachments(&self) -> &[u64] {
        &self.attachments
    }

    pub fn commit(self) -> Result<CommittedMemoryExec, SharedMemoryError> {
        let mut state = self
            .namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state != self.previous {
            return Err(SharedMemoryError::InvalidArgument);
        }
        *state = self.published.clone();
        drop(state);
        Ok(CommittedMemoryExec {
            namespace: self.namespace,
            previous: self.previous,
            published: self.published,
            retired: self.retired,
        })
    }
}

impl CommittedMemoryExec {
    pub fn rollback(self) -> Result<(), SharedMemoryError> {
        let mut state = self
            .namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state != self.published {
            return Err(SharedMemoryError::InvalidArgument);
        }
        *state = self.previous;
        Ok(())
    }

    pub fn finish(self) -> Result<(), SharedMemoryError> {
        for backing in self.retired {
            self.namespace.memory.remove(backing)?;
        }
        Ok(())
    }
}

impl SharedMemoryNamespace {
    pub fn prepare_exec(self: &Arc<Self>, process: u32, now: u64) -> Result<PreparedMemoryExec, SharedMemoryError> {
        let previous = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut published = previous.clone();
        let detached = published
            .attachments
            .iter()
            .filter(|(_, attachment)| attachment.pid == process)
            .map(|(token, attachment)| (*token, attachment.segment))
            .collect::<Vec<_>>();
        let mut retired = Vec::new();
        let attachments = detached.iter().map(|(token, _)| *token).collect();
        for (token, id) in detached {
            published.attachments.remove(&token);
            let slot = published
                .slots
                .get_mut(id.slot as usize)
                .ok_or(SharedMemoryError::NotFound)?;
            if slot.generation != id.generation {
                return Err(SharedMemoryError::NotFound);
            }
            let segment = slot.segment.as_mut().ok_or(SharedMemoryError::NotFound)?;
            segment.metadata.attaches = segment
                .metadata
                .attaches
                .checked_sub(1)
                .ok_or(SharedMemoryError::InvalidArgument)?;
            segment.metadata.last_pid = process;
            segment.metadata.detached_at = Some(now);
            if segment.metadata.marked_for_removal && segment.metadata.attaches == 0 {
                let removed = slot.segment.take().ok_or(SharedMemoryError::NotFound)?;
                published.allocated = published
                    .allocated
                    .checked_sub(removed.metadata.size)
                    .ok_or(SharedMemoryError::InvalidArgument)?;
                slot.generation = slot.generation.wrapping_add(1).max(1);
                retired.push(removed.metadata.backing);
            }
        }
        Ok(PreparedMemoryExec {
            namespace: Arc::clone(self),
            previous,
            published,
            retired,
            attachments,
        })
    }
}
