//! Cross-connection resource sharing — the export registry.
//!
//! Design: `src/gpu/hl-gpu/SHARING.md`. Slice 1 of tier 1: the registry, its identity rules and its
//! refcounted lifetime, with no protocol command and no executor wiring yet. It is defensible on its own
//! because every invariant it owns is testable without either.
//!
//! ## Why this exists
//!
//! [`SessionResources`](super::resources::SessionResources) is per-connection, and [`GlobalLedger`] is
//! shared but holds residency totals only — it confers accounting, not addressability. The guest CUDA
//! and GL drivers are separate connections to one executor process, so a `BufferId` minted by one is
//! meaningless in the other. Both objects are already host-side, so this is aliasing inside a process:
//! no memfd, no IOSurface, no copy.
//!
//! ## What this is NOT
//!
//! Read `SHARING.md`'s closing section before extending this. In short: buffers only and not a partial
//! image-interop delivery, no guest memory, single process, **not a security boundary** (an [`ExportId`]
//! is a capability token and the sessions involved are mutually trusting), and it converts a data race
//! into a REFUSAL rather than into a guarantee.
//!
//! [`GlobalLedger`]: super::resources::GlobalLedger

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

#[cfg(all(debug_assertions, not(doc)))]
use std::cell::Cell;

use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::Access;
use crate::runtime::model::resources::{Account, GlobalLedger, KIND_BUFFER};

/// A connection. Sessions are distinct per transport connection; two drivers in one guest workspace get
/// two of these.
///
/// Mint one with [`SessionId::next`] rather than constructing it: identity is what every rule in this
/// module keys on, and two connections sharing an id would each be able to unmap the other's claim.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// Source of [`SessionId`]s. Starts at 1 so no real session is `0`; monotonic and never rewound, for the
/// same reason [`ExportId`]s are not reused — a recycled connection id would let a departed session's
/// stale claim be mistaken for the new occupant's.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl SessionId {
    /// The next unused connection identity in this process.
    pub fn next() -> Self {
        Self(NEXT_SESSION.fetch_add(1, Ordering::Relaxed))
    }

    /// The guard encoding: `holder + 1`, so `0` stays free to mean "unmapped" and session `0` — were one
    /// ever constructed directly — is still visible. See [`Access`].
    fn encoded(self) -> u64 {
        self.0.saturating_add(1)
    }
}

/// The lock-free mirror of one entry's [`MapState`], shared with every [`Access`] guard watching it.
///
/// It is the SINGLE representation of that fact rather than a cache beside a `MapState` field: two
/// representations of one state drift, and the drift would be invisible because the guard reads only one
/// of them. Every write happens under the registry mutex; the atomic exists so the resolution path — the
/// hottest path in the service — can read it without taking that lock.
type StateCell = Arc<AtomicU64>;

fn load_state(cell: &StateCell) -> MapState {
    match cell.load(Ordering::Acquire) {
        0 => MapState::Unmapped,
        holder => MapState::MappedBy(SessionId(holder - 1)),
    }
}

fn store_state(cell: &StateCell, state: MapState) {
    let encoded = match state {
        MapState::Unmapped => 0,
        MapState::MappedBy(session) => session.encoded(),
    };
    cell.store(encoded, Ordering::Release);
}

/// A process-global handle to a shared resource.
///
/// **Never reused.** This is load-bearing rather than hygiene: it is the only thing that makes a stale
/// handle distinguishable from a live one. With recycled ids an import naming a dead export would
/// silently succeed against an unrelated resource — a wrong answer where this module owes an error.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ExportId(pub u64);

/// The resource an export refers to, in its owner's terms. Used to keep export idempotent: one entry per
/// resource, because two entries mean two refcounts and one of them will be wrong.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ResourceKey {
    pub session: SessionId,
    pub kind: &'static str,
    pub id: u32,
}

/// Whether a resource is currently claimed for exclusive use, and by whom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapState {
    Unmapped,
    MappedBy(SessionId),
}

/// The native object, type-erased exactly as [`super::resources::Native`] is, but shareable.
pub type Shared = Arc<dyn Any + Send + Sync>;

struct Entry {
    key: ResourceKey,
    resource: Shared,
    /// The authoritative byte length. The importer sees this and cannot widen it: a size the two sides
    /// disagree about is an out-of-bounds kernel that no bounds check catches.
    bytes: u64,
    importers: Vec<SessionId>,
    party_access: HashMap<SessionId, Arc<AtomicBool>>,
    accounts: HashMap<SessionId, (u32, Account)>,
    owner_account: Option<Account>,
    /// The exclusive-use claim, as the guards see it. See [`StateCell`].
    state: StateCell,
    /// The owner destroyed its id while importers remained. The storage is retained; see `release`.
    owner_released: bool,
    pending: Option<Pending>,
    payer: Option<SessionId>,
}

impl Entry {
    fn revoke(&mut self, session: SessionId) {
        if let Some(active) = self.party_access.remove(&session) {
            active.store(false, Ordering::Release);
        }
    }

    fn is_party(&self, session: SessionId) -> bool {
        self.importers.contains(&session) || (self.key.session == session && !self.owner_released)
    }

    fn state(&self) -> MapState {
        load_state(&self.state)
    }

    fn set_state(&self, state: MapState) {
        store_state(&self.state, state);
    }

    /// Who the retained bytes are charged to. The rule is "the charge follows the last live reference":
    /// while the owner holds the resource it pays, and once the owner has released, the importers keeping
    /// it alive pay. That is what stops a deferred release being a leak nobody can see — it lands in the
    /// accounting that already exists, bounded by the budget of whoever is actually keeping it alive.
    fn charged_to(&self) -> Option<SessionId> {
        if self.owner_released {
            self.payer.or_else(|| self.importers.iter().copied().min())
        } else {
            Some(self.key.session)
        }
    }

    fn is_dead(&self) -> bool {
        self.owner_released && self.importers.is_empty()
    }
}

/// The process-global export table. Cloning shares the same registry, exactly as [`GlobalLedger`] does.
///
/// [`GlobalLedger`]: super::resources::GlobalLedger
#[derive(Clone)]
pub struct Exports {
    inner: Arc<Mutex<Registry>>,
    changed: Arc<Condvar>,
    operation: Arc<Mutex<()>>,
}

/// One-shot transaction interruption points used to prove release failure atomicity.
#[cfg(all(debug_assertions, not(doc)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseFailpoint {
    None = 0,
    OwnerToPayer = 1,
    OwnerFinalRefund = 2,
    PayerToNext = 3,
    FinalPayerRefund = 4,
    NonPayerRelease = 5,
}

#[cfg(all(debug_assertions, not(doc)))]
thread_local! {
    static RELEASE_FAILPOINT: Cell<u8> = const { Cell::new(0) };
}

#[cfg(all(debug_assertions, not(doc)))]
fn trip_release_failpoint(point: ReleaseFailpoint) {
    RELEASE_FAILPOINT.with(|armed| {
        if armed.replace(0) == point as u8 {
            panic!("release transaction failpoint: {point:?}");
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransitionPhase {
    Prepared,
    Committing,
}

#[derive(Clone, Copy)]
struct Pending {
    token: u64,
    phase: TransitionPhase,
    authority: Option<SessionId>,
}

struct CommitLease {
    exports: Exports,
    id: ExportId,
    token: u64,
    irreversible: bool,
    complete: bool,
}

impl CommitLease {
    fn irreversible(&mut self) {
        self.irreversible = true;
    }
    fn complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for CommitLease {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        let mut registry = self
            .exports
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = registry.entries.get_mut(&self.id) {
            if entry
                .pending
                .is_some_and(|pending| pending.token == self.token)
            {
                if self.irreversible {
                    entry.pending = None;
                } else if let Some(pending) = entry.pending.as_mut() {
                    pending.phase = TransitionPhase::Prepared;
                }
            }
        }
        drop(registry);
        self.exports.changed.notify_all();
    }
}

impl Default for Exports {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Registry::default())),
            changed: Arc::new(Condvar::new()),
            operation: Arc::new(Mutex::new(())),
        }
    }
}

impl Exports {
    /// Serialize command execution with map-state transitions across every session sharing this registry.
    pub(crate) fn operation(&self) -> MutexGuard<'_, ()> {
        self.operation.lock().unwrap_or_else(|error| error.into_inner())
    }
}

mod release;
pub(crate) use release::ImportPlan;
#[cfg(all(debug_assertions, not(doc)))]
pub use release::{DebugImportPlan, DebugImportReleasePlan, DebugOwnerReleasePlan};
#[derive(Default)]
struct Registry {
    entries: HashMap<ExportId, Entry>,
    by_resource: HashMap<ResourceKey, ExportId>,
    /// Monotonic. Never rewound, never recycled — see [`ExportId`].
    next: u64,
    next_pending: u64,
    authorities: HashMap<SessionId, Account>,
    global: Option<GlobalLedger>,
}

mod import;
mod lifecycle;
#[cfg(test)]
mod transition_lease_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn commit_wins_race_cannot_be_cancelled_after_marking_committing() {
        let exports = Exports::new();
        let global = GlobalLedger::unbounded();
        let owner = Account::new();
        let id = exports
            .export_accounted(
                ResourceKey {
                    session: SessionId(100),
                    kind: "buffer",
                    id: 1,
                },
                Arc::new(1u32),
                4,
                owner.clone(),
                &global,
            )
            .unwrap();
        let plan = exports.prepare_owner_release(SessionId(100), id).unwrap();
        let owner_lock = owner.operation();
        let commit = std::thread::spawn(move || plan.commit());
        std::thread::sleep(Duration::from_millis(2));
        let waiter_exports = exports.clone();
        let (tx, rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            waiter_exports.settle_transition(id, Duration::from_millis(1));
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(5)).is_err(),
            "Committing lease must be waited out, not deadline-cancelled"
        );
        drop(owner_lock);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        commit.join().unwrap();
        waiter.join().unwrap();
        assert!(!exports.is_live(id));
    }

    #[test]
    fn panicked_commit_lease_never_leaves_committing_stuck() {
        let exports = Exports::new();
        let global = GlobalLedger::unbounded();
        let id = exports
            .export_accounted(
                ResourceKey {
                    session: SessionId(101),
                    kind: "buffer",
                    id: 1,
                },
                Arc::new(1u32),
                4,
                Account::new(),
                &global,
            )
            .unwrap();
        let plan = exports.prepare_owner_release(SessionId(101), id).unwrap();
        let token = plan.token;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = exports.begin_commit(id, token).unwrap();
            panic!("before account mutation");
        }));
        exports.settle_transition(id, Duration::ZERO);
        drop(plan);
        assert!(
            !exports
                .inner
                .lock()
                .unwrap()
                .entries
                .get(&id)
                .unwrap()
                .pending
                .is_some()
        );

        let plan = exports.prepare_owner_release(SessionId(101), id).unwrap();
        let token = plan.token;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut lease = exports.begin_commit(id, token).unwrap();
            lease.irreversible();
            panic!("after account mutation began");
        }));
        exports.settle_transition(id, Duration::from_millis(1));
        drop(plan);
        assert!(
            !exports
                .inner
                .lock()
                .unwrap()
                .entries
                .get(&id)
                .unwrap()
                .pending
                .is_some()
        );
    }

    #[test]
    fn completed_session_churn_reclaims_authority_accounts() {
        let exports = Exports::new();
        let global = GlobalLedger::unbounded();
        for raw in 1..=128 {
            let session = SessionId(1_000 + raw);
            let account = Account::new();
            let weak = Arc::downgrade(&account.inner);
            let id = exports
                .export_accounted(
                    ResourceKey {
                        session,
                        kind: "buffer",
                        id: raw as u32,
                    },
                    Arc::new(raw),
                    4,
                    account,
                    &global,
                )
                .unwrap();
            exports.prepare_owner_release(session, id).unwrap().commit();
            assert!(
                weak.upgrade().is_none(),
                "completed session {raw} retained its Account"
            );
        }
        assert!(exports.inner.lock().unwrap().authorities.is_empty());
    }
}

impl Registry {
    fn has_authority_claim(&self, session: SessionId) -> bool {
        self.entries.values().any(|entry| {
            (entry.key.session == session && !entry.owner_released)
                || entry.importers.contains(&session)
                || entry
                    .pending
                    .is_some_and(|pending| pending.authority == Some(session))
        })
    }

    /// Free an entry once nothing references it. Its `ExportId` is NOT returned to circulation.
    fn collect(&mut self, id: ExportId) {
        if self.entries.get(&id).is_some_and(Entry::is_dead) {
            let entry = self.entries.remove(&id);
            if let Some(entry) = entry {
                self.by_resource.remove(&entry.key);
            }
        }
    }
}
