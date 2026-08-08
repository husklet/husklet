use std::sync::Arc;

use super::coordinator::{LockCoordinator, LockState, RangeRecord};

use crate::{LockError, ProcessLockOwner, RangeLockSnapshot};

/// Owner-scoped POSIX-lock removal with rollback capacity and admission held.
pub struct PreparedLockExit {
    coordinator: Arc<LockCoordinator>,
    owner: ProcessLockOwner,
    prepared_version: u64,
    committed: bool,
    records: Vec<RangeLockSnapshot>,
}

impl LockCoordinator {
    /// Freezes one owner and captures exactly its current POSIX locks.
    pub fn prepare_exit(self: &Arc<Self>, owner: ProcessLockOwner) -> Result<PreparedLockExit, LockError> {
        let mut state = self.lock_state();
        if !state.frozen_owners.insert(owner) {
            return Err(LockError::ConcurrentMutation);
        }
        let mut records = Vec::new();
        for (file, ranges) in &state.ranges {
            records.extend(
                ranges
                    .iter()
                    .filter(|record| record.owner == owner)
                    .map(|record| RangeLockSnapshot {
                        file: *file,
                        owner,
                        kind: record.kind,
                        range: record.range,
                    }),
            );
        }
        Ok(PreparedLockExit {
            coordinator: Arc::clone(self),
            owner,
            prepared_version: state.owner_version(owner),
            committed: false,
            records,
        })
    }
}

impl PreparedLockExit {
    /// Removes the frozen owner's locks after checking its prepare generation.
    pub fn publish(&mut self) -> Result<(), LockError> {
        let mut state = self.coordinator.lock_state();
        if self.committed || state.owner_version(self.owner) != self.prepared_version {
            return Err(LockError::ConcurrentMutation);
        }
        for records in state.ranges.values_mut() {
            records.retain(|record| record.owner != self.owner);
        }
        state.ranges.retain(|_, records| !records.is_empty());
        state.exit_reservations += self.records.len();
        state.bump_owner(self.owner);
        self.resync_shared(&state);
        self.committed = true;
        self.coordinator.changed.notify_all();
        Ok(())
    }

    /// Restores captured locks. Owner admission makes this infallible.
    pub fn rollback(&mut self) {
        if !self.committed {
            self.release_owner();
            return;
        }
        let mut state = self.coordinator.lock_state();
        state.exit_reservations -= self.records.len();
        for record in &self.records {
            state.ranges.entry(record.file).or_default().push(RangeRecord {
                owner: record.owner,
                kind: record.kind,
                range: record.range,
            });
        }
        for records in state.ranges.values_mut() {
            LockCoordinator::coalesce(records);
        }
        state.bump_owner(self.owner);
        state.frozen_owners.remove(&self.owner);
        self.resync_shared(&state);
        self.committed = false;
        self.coordinator.changed.notify_all();
    }

    /// Keeps tier 2 in step with the tier-1 records this transaction moved.
    fn resync_shared(&self, state: &LockState) {
        let mut seen = Vec::new();
        for record in &self.records {
            if seen.contains(&record.file) {
                continue;
            }
            seen.push(record.file);
            self.coordinator.resync_shared(state, record.file);
        }
    }

    /// Releases rollback ownership after exit becomes irreversible.
    pub fn finish(&mut self) {
        let mut state = self.coordinator.lock_state();
        if self.committed {
            state.exit_reservations -= self.records.len();
            self.committed = false;
        }
        state.frozen_owners.remove(&self.owner);
    }

    fn release_owner(&mut self) {
        self.coordinator.lock_state().frozen_owners.remove(&self.owner);
    }
}

impl LockState {
    pub(crate) fn owner_version(&self, owner: ProcessLockOwner) -> u64 {
        self.owner_versions.get(&owner).copied().unwrap_or(0)
    }

    pub(crate) fn bump_owner(&mut self, owner: ProcessLockOwner) -> u64 {
        let version = self.owner_version(owner).wrapping_add(1).max(1);
        self.owner_versions.insert(owner, version);
        version
    }
}

impl Drop for PreparedLockExit {
    fn drop(&mut self) {
        if self.committed {
            self.rollback();
        } else {
            self.release_owner();
        }
    }
}
