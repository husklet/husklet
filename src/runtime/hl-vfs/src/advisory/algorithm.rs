use std::collections::HashSet;

use super::coordinator::{LockCoordinator, LockState, RangeRecord, WaitKind};
use crate::{FlockOwnerToken, Identity, LockRange, ProcessLockOwner, RangeLockKind, RangeLockSnapshot};

impl LockCoordinator {
    pub(crate) fn range_restore_conflict(existing: &[RangeRecord], record: &RangeLockSnapshot) -> bool {
        existing.iter().any(|item| {
            item.range.overlaps(record.range)
                && (item.owner == record.owner
                    || item.kind == RangeLockKind::Write
                    || record.kind == RangeLockKind::Write)
        })
    }

    pub(crate) fn update_waiter_blockers(state: &mut LockState, ticket: u64, blockers: Vec<ProcessLockOwner>) {
        let Some(waiter) = state.waiters.iter_mut().find(|waiter| waiter.ticket == ticket) else {
            return;
        };
        let WaitKind::Range { blockers: stored, .. } = &mut waiter.kind else {
            return;
        };
        *stored = blockers;
    }

    pub(crate) fn has_flock_owner(state: &LockState, file: Identity, owner: FlockOwnerToken) -> bool {
        let Some(records) = state.flocks.get(&file) else {
            return false;
        };
        records.iter().any(|record| record.owner == owner)
    }

    pub(crate) fn retain_range_sides(record: RangeRecord, removed: LockRange, output: &mut Vec<RangeRecord>) {
        if record.range.start < removed.start {
            output.push(RangeRecord {
                range: LockRange {
                    start: record.range.start,
                    end: Some(removed.start),
                },
                ..record
            });
        }
        let Some(removed_end) = removed.end else {
            return;
        };
        if record.range.end.is_none_or(|end| removed_end < end) {
            output.push(RangeRecord {
                range: LockRange {
                    start: removed_end,
                    end: record.range.end,
                },
                ..record
            });
        }
    }

    pub(crate) fn coalesce(records: &mut Vec<RangeRecord>) {
        records.sort_by_key(|record| (record.owner, record.kind as u8, record.range.start, record.range.end));
        let mut merged: Vec<RangeRecord> = Vec::new();
        for record in records.drain(..) {
            if Self::merge_last(&mut merged, record) {
                continue;
            }
            merged.push(record);
        }
        *records = merged;
    }

    fn merge_last(merged: &mut [RangeRecord], record: RangeRecord) -> bool {
        let Some(previous) = merged.last_mut() else {
            return false;
        };
        if previous.owner != record.owner
            || previous.kind != record.kind
            || previous.range.end != Some(record.range.start)
        {
            return false;
        }
        previous.range.end = record.range.end;
        true
    }

    pub(crate) fn would_deadlock(
        state: &LockState,
        requester: ProcessLockOwner,
        blockers: &[ProcessLockOwner],
    ) -> bool {
        let mut pending = blockers.to_vec();
        let mut visited = HashSet::new();
        while let Some(owner) = pending.pop() {
            if owner == requester {
                return true;
            }
            if !visited.insert(owner) {
                continue;
            }
            Self::append_wait_dependencies(state, owner, &mut pending);
        }
        false
    }

    fn append_wait_dependencies(state: &LockState, owner: ProcessLockOwner, pending: &mut Vec<ProcessLockOwner>) {
        for waiter in &state.waiters {
            let WaitKind::Range {
                owner: waiting,
                blockers,
            } = &waiter.kind
            else {
                continue;
            };
            if *waiting == owner {
                pending.extend(blockers.iter().copied());
            }
        }
    }

    pub(crate) fn lock_count(state: &LockState) -> usize {
        Self::other_lock_count(state) + state.flocks.values().map(Vec::len).sum::<usize>()
    }

    pub(crate) fn other_lock_count(state: &LockState) -> usize {
        state.ranges.values().map(Vec::len).sum::<usize>() + state.exit_reservations
    }
}
