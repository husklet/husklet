use super::{
    SemaphoreError, SemaphoreLimits, SemaphoreNamespace, SemaphoreSetSnapshot, SemaphoreSnapshot, Set, Slot, State,
};

impl SemaphoreNamespace {
    pub fn snapshot(&self) -> SemaphoreSnapshot {
        let state = self.lock();
        SemaphoreSnapshot {
            generations: state.slots.iter().map(|slot| slot.generation).collect(),
            sets: state
                .slots
                .iter()
                .filter_map(|slot| {
                    let set = slot.set.as_ref()?;
                    Some(SemaphoreSetSnapshot {
                        metadata: set.metadata.clone(),
                        values: set.values.clone(),
                        last_pids: set.last_pids.clone(),
                    })
                })
                .collect(),
            undo: state
                .undo
                .iter()
                .map(|((pid, id, index), value)| (*pid, *id, *index, *value))
                .collect(),
        }
    }

    pub fn restore(limits: SemaphoreLimits, snapshot: SemaphoreSnapshot) -> Result<Self, SemaphoreError> {
        let namespace = Self::new(limits)?;
        let mut state = namespace.lock();
        if snapshot.generations.len() > limits.sets || snapshot.generations.contains(&0) {
            return Err(SemaphoreError::ResourceLimit);
        }
        state.slots = snapshot
            .generations
            .iter()
            .map(|generation| Slot {
                generation: *generation,
                set: None,
            })
            .collect();
        for item in snapshot.sets {
            namespace.restore_set(&mut state, item)?;
        }
        for (pid, id, index, adjustment) in snapshot.undo {
            if state.undo.len() >= limits.undo_entries
                || adjustment == 0
                || usize::from(index) >= Self::set(&state, id)?.values.len()
                || state.undo.insert((pid, id, index), adjustment).is_some()
            {
                return Err(SemaphoreError::InvalidArgument);
            }
        }
        drop(state);
        Ok(namespace)
    }

    fn restore_set(&self, state: &mut State, item: SemaphoreSetSnapshot) -> Result<(), SemaphoreError> {
        let index = item.metadata.id.slot as usize;
        if index >= self.limits.sets
            || item.metadata.id.generation == 0
            || state.slots.get(index).map(|slot| slot.generation) != Some(item.metadata.id.generation)
            || item.values.is_empty()
            || item.values.len() > self.limits.set_semaphores
            || item.values.len() != item.last_pids.len()
            || item.values.iter().any(|value| *value > self.limits.maximum_value)
            || item.metadata.mode & !0o777 != 0
            || state
                .semaphores
                .checked_add(item.values.len())
                .is_none_or(|value| value > self.limits.total_semaphores)
            || item.metadata.key.is_some_and(|key| Self::key_id(state, key).is_some())
        {
            return Err(SemaphoreError::InvalidArgument);
        }
        if state.slots[index].set.is_some() {
            return Err(SemaphoreError::Exists);
        }
        state.semaphores += item.values.len();
        state.slots[index] = Slot {
            generation: state.slots[index].generation,
            set: Some(Set {
                metadata: item.metadata,
                decrement_waiters: vec![0; item.values.len()],
                zero_waiters: vec![0; item.values.len()],
                values: item.values,
                last_pids: item.last_pids,
            }),
        };
        Ok(())
    }
}
