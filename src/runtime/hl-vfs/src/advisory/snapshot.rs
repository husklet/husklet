use super::coordinator::{FlockRecord, LOCK_MAXIMUM, LockCoordinator, LockState, RangeRecord};
use crate::{AdvisoryLockSnapshot, FlockSnapshot, LockError, RangeLockSnapshot};

impl LockCoordinator {
    #[must_use]
    pub fn snapshot(&self) -> AdvisoryLockSnapshot {
        let state = self.lock_state();
        let mut snapshot = AdvisoryLockSnapshot::default();
        for (file, records) in &state.flocks {
            snapshot.flocks.extend(records.iter().map(|record| FlockSnapshot {
                file: *file,
                owner: record.owner,
                mode: record.mode,
            }));
        }
        for (file, records) in &state.ranges {
            snapshot.ranges.extend(records.iter().map(|record| RangeLockSnapshot {
                file: *file,
                owner: record.owner,
                kind: record.kind,
                range: record.range,
            }));
        }
        snapshot.flocks.sort_by_key(|record| {
            (
                record.file.device,
                record.file.inode,
                record.owner.identity,
                record.owner.generation,
            )
        });
        snapshot.ranges.sort_by_key(|record| {
            (
                record.file.device,
                record.file.inode,
                record.range.start,
                record.owner.identity,
                record.owner.generation,
            )
        });
        snapshot
    }

    /// Restores only into an empty coordinator and validates conflicts before
    /// publishing the replacement state.
    ///
    /// Snapshot and restore stay tier-1 only. A globalized table would make this
    /// emptiness check fail whenever any *other* container held a lock, and would
    /// let one container's checkpoint capture its neighbours' records.
    pub fn restore(&self, snapshot: &AdvisoryLockSnapshot) -> Result<(), LockError> {
        if snapshot.flocks.len() + snapshot.ranges.len() > LOCK_MAXIMUM {
            return Err(LockError::ResourceLimit);
        }
        let mut replacement = LockState {
            next_ticket: 1,
            ..LockState::default()
        };
        for record in &snapshot.flocks {
            if Self::flock_blocked(&replacement, record.file, record.owner, record.mode).is_some()
                || Self::has_flock_owner(&replacement, record.file, record.owner)
            {
                return Err(LockError::InvalidArgument);
            }
            replacement.flocks.entry(record.file).or_default().push(FlockRecord {
                owner: record.owner,
                mode: record.mode,
            });
        }
        for record in &snapshot.ranges {
            let existing = replacement.ranges.entry(record.file).or_default();
            if Self::range_restore_conflict(existing, record) {
                return Err(LockError::InvalidArgument);
            }
            existing.push(RangeRecord {
                owner: record.owner,
                kind: record.kind,
                range: record.range,
            });
            replacement.bump_owner(record.owner);
        }
        let mut state = self.lock_state();
        if !state.flocks.is_empty()
            || !state.ranges.is_empty()
            || !state.waiters.is_empty()
            || state.exit_reservations != 0
            || !state.frozen_owners.is_empty()
        {
            return Err(LockError::InvalidArgument);
        }
        // Copy-up translations describe the live filesystem, not the checkpoint.
        replacement.aliases = std::mem::take(&mut state.aliases);
        *state = replacement;
        self.changed.notify_all();
        Ok(())
    }
}
