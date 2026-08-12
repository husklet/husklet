use super::passthrough::{LockDomain, SharedLockRegistry};
use crate::{
    FlockMode, FlockOwnerToken, Identity, LockCancellation, LockError, LockRange, ProcessLockOwner, RangeConflict,
    RangeLockKind,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
pub(crate) const LOCK_MAXIMUM: usize = 16_384;
const WAITER_MAXIMUM: usize = 4_096;
#[derive(Clone, Copy)]
pub(crate) struct FlockRecord {
    pub(crate) owner: FlockOwnerToken,
    pub(crate) mode: FlockMode,
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
    pub(crate) next_ticket: u64,
    pub(crate) owner_versions: HashMap<ProcessLockOwner, u64>,
    pub(crate) exit_reservations: usize,
    pub(crate) frozen_owners: HashSet<ProcessLockOwner>,
    /// Lower identity to the upper one a copy-up published, so both name one lock file.
    pub(crate) aliases: HashMap<Identity, Identity>,
}

/// Per-runtime advisory-lock namespace, tier 1 of two.
///
/// This table owns every byte-range lock the container holds, along with its
/// waits, deadlock detection, per-container limit and checkpoint snapshot. Files
/// whose resolving mount came from outside the image are additionally mirrored
/// into the daemon-global tier-2 registry, because those inodes are reachable
/// from other containers in the same daemon process.
pub struct LockCoordinator {
    state: Mutex<LockState>,
    pub(crate) changed: Arc<Condvar>,
    pub(crate) shared: &'static SharedLockRegistry,
    pub(crate) domain: LockDomain,
    /// Set once this container has ever mirrored a lock, so containers that never
    /// touch a shared mount never reach the global registry at all.
    pub(crate) shared_active: std::sync::atomic::AtomicBool,
}

impl LockCoordinator {
    #[must_use]
    pub fn new() -> Self {
        let changed = Arc::new(Condvar::new());
        let shared = SharedLockRegistry::global();
        let domain = shared.attach(Arc::clone(&changed));
        Self {
            state: Mutex::new(LockState {
                flocks: HashMap::new(),
                ranges: HashMap::new(),
                waiters: VecDeque::new(),
                next_ticket: 1,
                owner_versions: HashMap::new(),
                exit_reservations: 0,
                frozen_owners: HashSet::new(),
                aliases: HashMap::new(),
            }),
            changed,
            shared,
            domain,
            shared_active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Binds the lower identity a copy-up read from to the upper one it published.
    ///
    /// Locks already held through the lower move to the upper, and a descriptor
    /// opened before the copy-up keeps stating the lower, so the translation is
    /// consulted on every later lock rather than only at this point.
    pub fn unify(&self, lower: Identity, upper: Identity) {
        if lower == upper {
            return;
        }
        let mut state = self.lock_state();
        // A freshly published upper inherits nothing: any alias on that number is recycled.
        state.aliases.remove(&upper);
        for target in state.aliases.values_mut() {
            if *target == lower {
                *target = upper;
            }
        }
        if let Some(records) = state.flocks.remove(&lower) {
            state.flocks.entry(upper).or_default().extend(records);
        }
        if let Some(records) = state.ranges.remove(&lower) {
            state.ranges.entry(upper).or_default().extend(records);
        }
        for waiter in &mut state.waiters {
            if waiter.file == lower {
                waiter.file = upper;
            }
        }
        state.aliases.insert(lower, upper);
        // Records just moved between identities, so tier 2 has to follow them.
        // Copy-up only ever targets image layers, but a shared mount must never
        // be left with a record filed under an identity nothing consults again.
        self.resync_shared(&state, lower);
        self.resync_shared(&state, upper);
        self.changed.notify_all();
    }

    /// Drops translations naming an inode whose last link is going away, so the
    /// number is safe for the host to reuse.
    pub fn forget(&self, file: Identity) {
        let mut state = self.lock_state();
        state.aliases.remove(&file);
        state.aliases.retain(|_, target| *target != file);
    }

    fn translate(state: &LockState, file: Identity) -> Identity {
        state.aliases.get(&file).copied().unwrap_or(file)
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
        let file = Self::translate(&state, file);
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

    /// `shared` is true only when the file's resolving mount is a host-passthrough
    /// bind or volume. It is the single bit that selects tier 2; when false this
    /// runs exactly the tier-1 path it always did.
    pub fn set_range(
        &self,
        file: Identity,
        owner: ProcessLockOwner,
        kind: Option<RangeLockKind>,
        range: LockRange,
        blocking: bool,
        shared: bool,
        cancellation: &LockCancellation,
    ) -> Result<(), LockError> {
        let mut state = self.lock_state();
        if state.frozen_owners.contains(&owner) {
            return Err(LockError::ConcurrentMutation);
        }
        let file = Self::translate(&state, file);
        if kind.is_none() {
            Self::rewrite_owner_range(&mut state, file, owner, range, None)?;
            state.bump_owner(owner);
            self.mirror(&state, file, shared);
            self.changed.notify_all();
            return Ok(());
        }
        let kind = kind.expect("checked above");
        let blockers = self.all_blockers(&state, file, owner, kind, range, shared);
        if blockers.is_empty() {
            Self::rewrite_owner_range(&mut state, file, owner, range, Some(kind))?;
            state.bump_owner(owner);
            self.mirror(&state, file, shared);
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
            let blockers = self.all_blockers(&state, file, owner, kind, range, shared);
            if Self::eligible(&state, ticket) && blockers.is_empty() {
                Self::remove_waiter(&mut state, ticket);
                Self::rewrite_owner_range(&mut state, file, owner, range, Some(kind))?;
                state.bump_owner(owner);
                self.mirror(&state, file, shared);
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
        shared: bool,
    ) -> Option<RangeConflict> {
        let state = self.lock_state();
        // Tier 2 is consulted on the translated identity, so a copy-up cannot
        // leave the two tiers disagreeing about which inode is being locked.
        let file = Self::translate(&state, file);
        let local = state
            .ranges
            .get(&file)
            .and_then(|records| {
                records.iter().find(|record| {
                    record.owner != owner
                        && record.range.overlaps(range)
                        && (kind == RangeLockKind::Write || record.kind == RangeLockKind::Write)
                })
            })
            .map(|record| RangeConflict {
                owner: record.owner,
                kind: record.kind,
                range: record.range,
            });
        if local.is_some() || !shared {
            return local;
        }
        self.shared.conflict(file, self.domain, kind, range)
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
        self.release_shared_owner(range_owner);
        self.changed.notify_all();
    }

    /// POSIX locks disappear when this process closes any fd for `file`.
    pub fn close_process_file(&self, file: Identity, owner: ProcessLockOwner, shared: bool) -> Result<(), LockError> {
        let mut state = self.lock_state();
        if state.frozen_owners.contains(&owner) {
            return Err(LockError::ConcurrentMutation);
        }
        let file = Self::translate(&state, file);
        if let Some(records) = state.ranges.get_mut(&file) {
            records.retain(|record| record.owner != owner);
            if records.is_empty() {
                state.ranges.remove(&file);
            }
        }
        state.bump_owner(owner);
        self.mirror(&state, file, shared);
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

    pub(crate) fn flock_blocked(
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

    pub(crate) fn range_blockers(
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

impl std::fmt::Debug for LockCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LockCoordinator")
    }
}

impl Default for LockCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LockCoordinator {
    fn drop(&mut self) {
        self.shared.detach(self.domain);
    }
}
