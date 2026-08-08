use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use super::coordinator::{LockCoordinator, LockState};
use crate::{Identity, LockRange, ProcessLockOwner, RangeConflict, RangeLockKind};

/// One container's byte-range lock namespace within the daemon process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LockDomain(u64);

#[derive(Clone, Copy)]
struct SharedRecord {
    domain: LockDomain,
    owner: ProcessLockOwner,
    kind: RangeLockKind,
    range: LockRange,
}

/// Daemon-global byte-range lock registry for host-passthrough files.
///
/// Every container runs inside one daemon process, so a bind mount or volume
/// shared by two containers is one host inode that the per-container tier-1
/// coordinators cannot see across. This tier mirrors only the records whose
/// resolving mount came from outside the image, and is consulted only for those.
pub struct SharedLockRegistry {
    files: Mutex<HashMap<Identity, Vec<SharedRecord>>>,
    wakeups: Mutex<HashMap<LockDomain, Arc<Condvar>>>,
    next_domain: AtomicU64,
}

static REGISTRY: OnceLock<SharedLockRegistry> = OnceLock::new();

impl SharedLockRegistry {
    /// The process-wide registry. Host inodes are a process-wide resource, so the
    /// tier-2 namespace is too; no daemon plumbing can make it narrower and stay correct.
    pub fn global() -> &'static Self {
        REGISTRY.get_or_init(|| Self {
            files: Mutex::new(HashMap::new()),
            wakeups: Mutex::new(HashMap::new()),
            next_domain: AtomicU64::new(1),
        })
    }

    pub(crate) fn attach(&self, wakeup: Arc<Condvar>) -> LockDomain {
        let domain = LockDomain(self.next_domain.fetch_add(1, Ordering::Relaxed));
        self.wakeup_state().insert(domain, wakeup);
        domain
    }

    /// Drops a torn-down container's records and its wakeup registration.
    pub(crate) fn detach(&self, domain: LockDomain) {
        self.wakeup_state().remove(&domain);
        let mut files = self.file_state();
        files.retain(|_, records| {
            records.retain(|record| record.domain != domain);
            !records.is_empty()
        });
        drop(files);
        self.notify();
    }

    /// Owners of conflicting records held by *other* containers.
    pub(crate) fn blockers(
        &self,
        file: Identity,
        domain: LockDomain,
        kind: RangeLockKind,
        range: LockRange,
    ) -> Vec<ProcessLockOwner> {
        self.file_state()
            .get(&file)
            .into_iter()
            .flatten()
            .filter(|record| Self::conflicts(record, domain, kind, range))
            .map(|_| ProcessLockOwner::foreign())
            .collect()
    }

    /// First conflicting record held by another container, for `F_GETLK`.
    pub(crate) fn conflict(
        &self,
        file: Identity,
        domain: LockDomain,
        kind: RangeLockKind,
        range: LockRange,
    ) -> Option<RangeConflict> {
        self.file_state()
            .get(&file)?
            .iter()
            .find(|record| Self::conflicts(record, domain, kind, range))
            .map(|record| RangeConflict {
                // A foreign holder has no meaningful pid in this container's namespace,
                // so it is reported the way an OFD holder is.
                owner: ProcessLockOwner::foreign(),
                kind: record.kind,
                range: record.range,
            })
    }

    /// Replaces this domain's records for one file.
    ///
    /// `create` is the caller's shared-mount bit: without it an absent bucket stays
    /// absent, so removal paths that never learned the bit remain correct no-ops.
    pub(crate) fn publish(
        &self,
        file: Identity,
        domain: LockDomain,
        records: &[(ProcessLockOwner, RangeLockKind, LockRange)],
        create: bool,
    ) {
        let mut files = self.file_state();
        let Some(bucket) = files.get_mut(&file) else {
            if !create || records.is_empty() {
                return;
            }
            files.insert(file, Self::rows(domain, records));
            drop(files);
            self.notify();
            return;
        };
        bucket.retain(|record| record.domain != domain);
        bucket.extend(Self::rows(domain, records));
        if bucket.is_empty() {
            files.remove(&file);
        }
        drop(files);
        self.notify();
    }

    /// Drops every record one owner holds in this domain, across all files.
    pub(crate) fn release_owner(&self, domain: LockDomain, owner: ProcessLockOwner) {
        let mut files = self.file_state();
        files.retain(|_, records| {
            records.retain(|record| record.domain != domain || record.owner != owner);
            !records.is_empty()
        });
        drop(files);
        self.notify();
    }

    fn rows(domain: LockDomain, records: &[(ProcessLockOwner, RangeLockKind, LockRange)]) -> Vec<SharedRecord> {
        records
            .iter()
            .map(|(owner, kind, range)| SharedRecord {
                domain,
                owner: *owner,
                kind: *kind,
                range: *range,
            })
            .collect()
    }

    fn conflicts(record: &SharedRecord, domain: LockDomain, kind: RangeLockKind, range: LockRange) -> bool {
        record.domain != domain
            && record.range.overlaps(range)
            && (kind == RangeLockKind::Write || record.kind == RangeLockKind::Write)
    }

    /// Wakes every container's waiters; a release here can unblock any of them.
    fn notify(&self) {
        for wakeup in self.wakeup_state().values() {
            wakeup.notify_all();
        }
    }

    fn file_state(&self) -> std::sync::MutexGuard<'_, HashMap<Identity, Vec<SharedRecord>>> {
        self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wakeup_state(&self) -> std::sync::MutexGuard<'_, HashMap<LockDomain, Arc<Condvar>>> {
        self.wakeups.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The coordinator's half of the two-tier split.
impl LockCoordinator {
    /// Tier-1 blockers, plus tier-2 blockers when the file is on a shared mount.
    pub(crate) fn all_blockers(
        &self,
        state: &LockState,
        file: Identity,
        owner: ProcessLockOwner,
        kind: RangeLockKind,
        range: LockRange,
        shared: bool,
    ) -> Vec<ProcessLockOwner> {
        let mut blockers = Self::range_blockers(state, file, owner, kind, range);
        if shared {
            blockers.extend(self.shared.blockers(file, self.domain, kind, range));
        }
        blockers
    }

    /// Republishes this domain's tier-1 records for one file into tier 2.
    ///
    /// Returns immediately when the file is not on a shared mount: the bit is a
    /// property of the inode's mount, so a file that never entered tier 2 can
    /// never need removing from it, and the global registry stays untouched.
    pub(crate) fn mirror(&self, state: &LockState, file: Identity, shared: bool) {
        if !shared {
            return;
        }
        self.shared_active.store(true, std::sync::atomic::Ordering::Release);
        let records = state
            .ranges
            .get(&file)
            .into_iter()
            .flatten()
            .map(|record| (record.owner, record.kind, record.range))
            .collect::<Vec<_>>();
        self.shared.publish(file, self.domain, &records, shared);
    }

    /// Republishes one file's tier-1 records into tier 2 without needing the mount
    /// bit, by refusing to create a bucket that does not already exist. Exit
    /// publish/rollback use this so a reversible removal stays reversible in tier 2.
    pub(crate) fn resync_shared(&self, state: &LockState, file: Identity) {
        if !self.shared_active.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let records = state
            .ranges
            .get(&file)
            .into_iter()
            .flatten()
            .map(|record| (record.owner, record.kind, record.range))
            .collect::<Vec<_>>();
        self.shared.publish(file, self.domain, &records, false);
    }

    /// Drops one owner's tier-2 records. The file is not known here, so this is
    /// keyed by owner; it is skipped outright for containers that never mirrored.
    pub(crate) fn release_shared_owner(&self, owner: ProcessLockOwner) {
        if !self.shared_active.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        self.shared.release_owner(self.domain, owner);
    }
}
