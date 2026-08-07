use super::{
    Arc, Attachment, NamespaceState, Segment, SharedMemoryError, SharedMemoryLimits, SharedMemoryMetadata,
    SharedMemoryNamespace, SharedMemorySnapshot, Slot,
};

impl SharedMemoryNamespace {
    pub fn snapshot(&self) -> SharedMemorySnapshot {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        SharedMemorySnapshot {
            generations: state.slots.iter().map(|slot| slot.generation).collect(),
            segments: state
                .slots
                .iter()
                .filter_map(|slot| slot.segment.as_ref().map(|segment| segment.metadata))
                .collect(),
            attachments: state
                .attachments
                .iter()
                .map(|(token, attachment)| (*token, attachment.segment, attachment.pid))
                .collect(),
            next_attachment: state.next_attachment,
        }
    }

    pub fn restore(
        memory: Arc<dyn crate::SharedBackingAccess>,
        limits: SharedMemoryLimits,
        snapshot: SharedMemorySnapshot,
    ) -> Result<Self, SharedMemoryError> {
        let namespace = Self::new(memory, limits)?;
        let mut state = namespace
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if snapshot.generations.len() > limits.segments || snapshot.generations.contains(&0) {
            return Err(SharedMemoryError::ResourceLimit);
        }
        state.slots = snapshot
            .generations
            .iter()
            .map(|generation| Slot {
                generation: *generation,
                segment: None,
            })
            .collect();
        state.next_attachment = snapshot.next_attachment;
        for metadata in snapshot.segments {
            namespace.restore_segment(&mut state, metadata)?;
        }
        for (token, segment, pid) in snapshot.attachments {
            if state.attachments.len() >= limits.attachments || token >= state.next_attachment {
                return Err(SharedMemoryError::ResourceLimit);
            }
            if token == 0 || state.attachments.contains_key(&token) {
                return Err(SharedMemoryError::InvalidArgument);
            }
            Self::segment(&state, segment)?;
            state.attachments.insert(token, Attachment { segment, pid });
        }
        Self::validate_attach_counts(&state)?;
        drop(state);
        Ok(namespace)
    }

    fn restore_segment(
        &self,
        state: &mut NamespaceState,
        metadata: SharedMemoryMetadata,
    ) -> Result<(), SharedMemoryError> {
        if metadata.id.slot as usize >= self.limits.segments {
            return Err(SharedMemoryError::ResourceLimit);
        }
        self.validate_restored(state, metadata)?;
        let index = metadata.id.slot as usize;
        if state.slots.get(index).is_none()
            || state.slots[index].generation != metadata.id.generation
            || state.slots[index].segment.is_some()
        {
            return Err(SharedMemoryError::InvalidArgument);
        }
        state.allocated += metadata.size;
        state.slots[index] = Slot {
            generation: state.slots[index].generation,
            segment: Some(Segment { metadata }),
        };
        Ok(())
    }
}
