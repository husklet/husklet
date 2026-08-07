use crate::{
    AdvisoryLockSnapshot, FlockMode, FlockOwnerToken, FlockSnapshot, Identity, LockCancellation, LockError, LockRange,
    ProcessLockOwner, RangeConflict, RangeLockKind, RangeLockSnapshot,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Condvar, Mutex};
pub(crate) const LOCK_MAXIMUM: usize = 16_384;
const WAITER_MAXIMUM: usize = 4_096;
#[derive(Clone, Copy)]
pub(crate) struct FlockRecord {
    pub(crate) owner: FlockOwnerToken,
    mode: FlockMode,
}
#[derive(Clone, Copy)]
pub(crate) struct RangeRecord {
    pub(crate) owner: ProcessLockOwner,
    pub(crate) kind: RangeLockKind,
    pub(crate) range: LockRange,
}

#[derive(Clone)]
pub(crate) enum WaitKind {
    Flock,
    Range {
        owner: ProcessLockOwner,
        blockers: Vec<ProcessLockOwner>,
    },
}

#[derive(Clone)]
pub(crate) struct Waiter {
    pub(crate) ticket: u64,
    pub(crate) file: Identity,
    pub(crate) kind: WaitKind,
}

#[derive(Default)]
pub(crate) struct LockState {
    pub(crate) flocks: HashMap<Identity, Vec<FlockRecord>>,
    pub(crate) ranges: HashMap<Identity, Vec<RangeRecord>>,
    pub(crate) waiters: VecDeque<Waiter>,
    next_ticket: u64,
    pub(crate) owner_versions: HashMap<ProcessLockOwner, u64>,
    pub(crate) exit_reservations: usize,
    pub(crate) frozen_owners: HashSet<ProcessLockOwner>,
}

/// Per-runtime advisory-lock namespace.
pub struct LockCoordinator {
    state: Mutex<LockState>,
    pub(crate) changed: Condvar,
}

impl LockCoordinator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LockState {
                flocks: HashMap::new(),
                ranges: HashMap::new(),
                waiters: VecDeque::new(),
                next_ticket: 1,
                owner_versions: HashMap::new(),
                exit_reservations: 0,
                frozen_owners: HashSet::new(),
            }),
            changed: Condvar::new(),
        }
    }

    pub fn set_flock(
        &self,
        file: Identity,
        owner: FlockOwnerToken,
        mode: Option<FlockMode>,
        blocking: bool,
        cancellation: &LockCancellation,
    ) -> Result<(), LockError> {
        let mut state = self.lock_state();
        if mode.is_none() {
            Self::remove_flock(&mut state, file, owner);
            self.changed.notify_all();
            return Ok(());
        }
        let mode = mode.expect("checked above");
        if Self::flock_blocked(&state, file, owner, mode).is_none() {
            Self::replace_flock(&mut state, file, owner, mode)?;
            return Ok(());
        }
        if !blocking {
            return Err(LockError::WouldBlock);
        }
        let ticket = Self::enqueue(&mut state, file, WaitKind::Flock)?;
        loop {
            if let Some(error) = cancellation.failure() {
                Self::remove_waiter(&mut state, ticket);
                self.changed.notify_all();
                return Err(error);
            }
            if Self::eligible(&state, ticket) && Self::flock_blocked(&state, file, owner, mode).is_none() {
                Self::remove_waiter(&mut state, ticket);
                Self::replace_flock(&mut state, file, owner, mode)?;
                self.changed.notify_all();
                return Ok(());
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub fn set_range(
        &self,
        file: Identity,
        owner: ProcessLockOwner,
        kind: Option<RangeLockKind>,
        range: LockRange,
        blocking: bool,
        cancellation: &LockCancellation,
    ) -> Result<(), LockError> {
        let mut state = self.lock_state();
        if state.frozen_owners.contains(&owner) {
            return Err(LockError::ConcurrentMutation);
        }
        if kind.is_none() {
            Self::rewrite_owner_range(&mut state, file, owner, range, None)?;
            state.bump_owner(owner);
            self.changed.notify_all();
            return Ok(());
        }
        let kind = kind.expect("checked above");
        let blockers = Self::range_blockers(&state, file, owner, kind, range);
        if blockers.is_empty() {
            Self::rewrite_owner_range(&mut state, file, owner, range, Some(kind))?;
            state.bump_owner(owner);
            return Ok(());
        }
        if !blocking {
            return Err(LockError::WouldBlock);
        }
        if Self::would_deadlock(&state, owner, &blockers) {
            return Err(LockError::Deadlock);
        }
        let ticket = Self::enqueue(&mut state, file, WaitKind::Range { owner, blockers })?;
        loop {
            if let Some(error) = cancellation.failure() {
                Self::remove_waiter(&mut state, ticket);
                self.changed.notify_all();
                return Err(error);
            }
            let blockers = Self::range_blockers(&state, file, owner, kind, range);
            if Self::eligible(&state, ticket) && blockers.is_empty() {
                Self::remove_waiter(&mut state, ticket);
                Self::rewrite_owner_range(&mut state, file, owner, range, Some(kind))?;
                state.bump_owner(owner);
                self.changed.notify_all();
                return Ok(());
            }
            Self::update_waiter_blockers(&mut state, ticket, blockers);
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Marks one blocking operation interrupted while holding the wait mutex,
    /// preventing a notification from being lost between its failure check and wait.
    pub fn interrupt(&self, cancellation: &LockCancellation) {
        let _state = self.lock_state();
        cancellation.interrupt();
        self.changed.notify_all();
    }

    #[must_use]
    pub fn query_range(
        &self,
        file: Identity,
        owner: ProcessLockOwner,
        kind: RangeLockKind,
        range: LockRange,
    ) -> Option<RangeConflict> {
        self.lock_state()
            .ranges
            .get(&file)?
            .iter()
            .find(|record| {
                record.owner != owner
                    && record.range.overlaps(range)
                    && (kind == RangeLockKind::Write || record.kind == RangeLockKind::Write)
            })
            .map(|record| RangeConflict {
                owner: record.owner,
                kind: record.kind,
                range: record.range,
            })
    }

    /// BSD locks disappear at final OFD close, including after dup/fork.
    pub fn close_ofd(&self, owner: FlockOwnerToken) {
        let mut state = self.lock_state();
        for records in state.flocks.values_mut() {
            records.retain(|record| record.owner != owner);
        }
        state.flocks.retain(|_, records| !records.is_empty());
        let range_owner = ProcessLockOwner::open_file(owner);
        for records in state.ranges.values_mut() {
            records.retain(|record| record.owner != range_owner);
        }
        state.ranges.retain(|_, records| !records.is_empty());
        state.bump_owner(range_owner);
        self.changed.notify_all();
    }

    /// POSIX locks disappear when this process closes any fd for `file`.
    pub fn close_process_file(&self, file: Identity, owner: ProcessLockOwner) -> Result<(), LockError> {
        let mut state = self.lock_state();
        if state.frozen_owners.contains(&owner) {
            return Err(LockError::ConcurrentMutation);
        }
        if let Some(records) = state.ranges.get_mut(&file) {
            records.retain(|record| record.owner != owner);
            if records.is_empty() {
                state.ranges.remove(&file);
            }
        }
        state.bump_owner(owner);
        self.changed.notify_all();
        Ok(())
    }

    /// Wakes waiters after runtime marks a cancellation or signal interruption.
    pub fn wake_waiters(&self) {
        self.changed.notify_all();
    }

    #[must_use]
    pub fn waiting(&self) -> usize {
        self.lock_state().waiters.len()
    }

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
    pub fn restore(&self, snapshot: &AdvisoryLockSnapshot) -> Result<(), LockError> {
        if snapshot.flocks.len() + snapshot.ranges.len() > LOCK_MAXIMUM {
            return Err(LockError::ResourceLimit);
        }
        let mut replacement = LockState::default();
        replacement.next_ticket = 1;
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
        *state = replacement;
        self.changed.notify_all();
        Ok(())
    }

    fn flock_blocked(
        state: &LockState,
        file: Identity,
        owner: FlockOwnerToken,
        mode: FlockMode,
    ) -> Option<FlockOwnerToken> {
        state.flocks.get(&file)?.iter().find_map(|record| {
            (record.owner != owner && (mode == FlockMode::Exclusive || record.mode == FlockMode::Exclusive))
                .then_some(record.owner)
        })
    }

    fn replace_flock(
        state: &mut LockState,
        file: Identity,
        owner: FlockOwnerToken,
        mode: FlockMode,
    ) -> Result<(), LockError> {
        if Self::lock_count(state) == LOCK_MAXIMUM && !Self::has_flock_owner(state, file, owner) {
            return Err(LockError::ResourceLimit);
        }
        Self::remove_flock(state, file, owner);
        state.flocks.entry(file).or_default().push(FlockRecord { owner, mode });
        Ok(())
    }

    fn remove_flock(state: &mut LockState, file: Identity, owner: FlockOwnerToken) {
        if let Some(records) = state.flocks.get_mut(&file) {
            records.retain(|record| record.owner != owner);
            if records.is_empty() {
                state.flocks.remove(&file);
            }
        }
    }

    fn range_blockers(
        state: &LockState,
        file: Identity,
        owner: ProcessLockOwner,
        kind: RangeLockKind,
        range: LockRange,
    ) -> Vec<ProcessLockOwner> {
        state
            .ranges
            .get(&file)
            .into_iter()
            .flatten()
            .filter(|record| {
                record.owner != owner
                    && record.range.overlaps(range)
                    && (kind == RangeLockKind::Write || record.kind == RangeLockKind::Write)
            })
            .map(|record| record.owner)
            .collect()
    }

    fn rewrite_owner_range(
        state: &mut LockState,
        file: Identity,
        owner: ProcessLockOwner,
        range: LockRange,
        kind: Option<RangeLockKind>,
    ) -> Result<(), LockError> {
        let records = state.ranges.remove(&file).unwrap_or_default();
        let mut rewritten = Vec::new();
        for record in records {
            if record.owner != owner || !record.range.overlaps(range) {
                rewritten.push(record);
                continue;
            }
            Self::retain_range_sides(record, range, &mut rewritten);
        }
        if let Some(kind) = kind {
            rewritten.push(RangeRecord { owner, kind, range });
        }
        Self::coalesce(&mut rewritten);
        if Self::other_lock_count(state) + rewritten.len() > LOCK_MAXIMUM {
            return Err(LockError::ResourceLimit);
        }
        if !rewritten.is_empty() {
            state.ranges.insert(file, rewritten);
        }
        Ok(())
    }

    fn enqueue(state: &mut LockState, file: Identity, kind: WaitKind) -> Result<u64, LockError> {
        if state.waiters.len() == WAITER_MAXIMUM {
            return Err(LockError::ResourceLimit);
        }
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1).max(1);
        state.waiters.push_back(Waiter { ticket, file, kind });
        Ok(ticket)
    }

    fn eligible(state: &LockState, ticket: u64) -> bool {
        let Some(waiter) = state.waiters.iter().find(|item| item.ticket == ticket) else {
            return false;
        };
        !state
            .waiters
            .iter()
            .take_while(|item| item.ticket != ticket)
            .any(|earlier| earlier.file == waiter.file)
    }

    fn remove_waiter(state: &mut LockState, ticket: u64) {
        state.waiters.retain(|waiter| waiter.ticket != ticket);
    }

    pub(crate) fn lock_state(&self) -> std::sync::MutexGuard<'_, LockState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for LockCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
