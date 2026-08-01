//! Typed resource ids (guest-assigned in the IR) + a host-side generational resource table.
//!
//! The guest producer allocates ids from its own monotonically-increasing counters and names them in
//! every command; the host executor keeps the id → live-object mapping. A [`ResourceTable`] gives each
//! backend a uniform way to enforce lifecycle — rejecting a duplicate create, a use-after-free, or a
//! double-free — turning what would be UB on a real driver into a typed [`GpuError`].

use crate::protocol::model::error::{GpuError, Result};
use std::collections::HashMap;

macro_rules! def_id {
    ($(#[$m:meta])* $name:ident, $kind:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        pub struct $name(pub u32);
        impl $name {
            /// Human-readable resource-kind tag, used in error messages and the mock log.
            pub const KIND: &'static str = $kind;
            pub fn raw(self) -> u32 {
                self.0
            }
        }
    };
}

def_id!(/// GPU buffer (vertex/index/uniform/storage, or a device allocation).
    BufferId, "buffer");
def_id!(/// Image / render target / sampled texture.
    TextureId, "texture");
def_id!(/// Texture sampler state.
    SamplerId, "sampler");
def_id!(/// A shader module — SPIR-V words (the committed shader ABI).
    ShaderId, "shader");
def_id!(/// A compiled render or compute pipeline (state + shader entry points).
    PipelineId, "pipeline");
def_id!(/// A bound set of resources (WebGPU bind group / Vulkan descriptor set).
    BindGroupId, "bind_group");
def_id!(/// A presentable output surface (one HLP `Surface`).
    SurfaceId, "surface");
def_id!(/// A timeline fence for host↔guest synchronization.
    FenceId, "fence");

/// A generational slot: `gen` is a globally-unique allocation stamp, so a reference captured against
/// one allocation of an id can be told apart from a later reuse of the same id (stale-ref detection).
struct Slot<T> {
    gen: u32,
    val: T,
    /// Set only on a resource shared across connections. `None` — the overwhelmingly common case — costs
    /// one branch on the resolution path and no lookup at all.
    guard: Option<Access>,
}

/// The exclusive-use guard on a shared resource, as seen from ONE session's table.
///
/// `state` is shared with the export registry: `0` means unmapped, any other value is `holder + 1`. It is
/// an atomic rather than a lock because this is read on the hottest path in the service — a resolution
/// that took a mutex would tax every command in every session to serve the rare shared one.
#[derive(Clone)]
pub struct Access {
    state: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The session this table belongs to, pre-encoded as `id + 1` so the check is one compare.
    me: u64,
}

impl Access {
    pub fn new(state: std::sync::Arc<std::sync::atomic::AtomicU64>, session: u64) -> Self {
        Self {
            state,
            me: session + 1,
        }
    }

    /// Whether this session may touch the resource right now.
    fn check(&self, kind: &'static str, id: u32) -> Result<()> {
        let holder = self.state.load(std::sync::atomic::Ordering::Acquire);
        if holder == 0 || holder == self.me {
            return Ok(());
        }
        Err(GpuError::MappedElsewhere { kind, id })
    }
}

/// One recorded undo action for a table's in-flight transaction ([`ResourceTable::begin`]). The journal
/// is replayed in reverse on [`rollback`](ResourceTable::rollback) so a whole batch of inserts/removes
/// reverts as a unit — the executor-side half of "a NACKed frame leaves the tables EXACTLY as before".
enum Undo<T> {
    /// `id` was inserted during the transaction; undo removes it (dropping the created native).
    Inserted(u32),
    /// `id` was removed during the transaction; undo restores its saved slot — the native it OWNED, kept
    /// alive in the journal instead of dropped, so a rolled-back destroy is truly reversible.
    Removed(u32, Slot<T>),
}

/// Host-side id → object map with lifecycle checking. One per resource kind per backend.
///
/// Generations come from a single monotonic counter rather than a per-id map, so destroy/recreate
/// churn does **not** leak an unbounded table of freed ids: live state shrinks to exactly the live
/// resources, while each fresh allocation still gets a distinct stamp for stale-reference checks.
pub struct ResourceTable<T> {
    kind: &'static str,
    live: HashMap<u32, Slot<T>>,
    /// Monotonic allocation counter; every `insert` consumes the next value as the slot's generation.
    next_gen: u32,
    /// When `Some`, a transaction is in flight and every insert/remove is journaled so the batch can be
    /// rolled back atomically on an executor NACK. `None` outside a transaction (the common, zero-overhead
    /// path). A removed slot's native is parked in the journal (not dropped) until the transaction commits.
    journal: Option<Vec<Undo<T>>>,
}

impl<T> ResourceTable<T> {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            live: HashMap::new(),
            next_gen: 1,
            journal: None,
        }
    }

    /// Begin a transaction: subsequent inserts/removes are journaled so [`rollback`](Self::rollback) can
    /// revert them as a unit and [`commit`](Self::commit) can finalize them. Idempotent-ish — a `begin`
    /// while one is already open discards the prior (uncommitted) journal, matching one-transaction-per-frame.
    pub fn begin(&mut self) {
        self.journal = Some(Vec::new());
    }

    /// Commit the in-flight transaction: drop the journal so every parked (removed) native is freed for
    /// real. A no-op if no transaction is open.
    pub fn commit(&mut self) {
        self.journal = None;
    }

    /// Roll back the in-flight transaction: replay the journal in REVERSE so the table is byte-for-byte
    /// what it was at [`begin`](Self::begin) — inserted natives are dropped, removed natives restored. The
    /// monotonic `next_gen` is intentionally NOT rewound (a consumed stamp is harmless; gens only need to
    /// stay distinct). A no-op if no transaction is open.
    pub fn rollback(&mut self) {
        if let Some(journal) = self.journal.take() {
            for undo in journal.into_iter().rev() {
                match undo {
                    Undo::Inserted(id) => {
                        self.live.remove(&id);
                    }
                    Undo::Removed(id, slot) => {
                        self.live.insert(id, slot);
                    }
                }
            }
        }
    }

    /// Insert a freshly-created resource. Errors if `id` is already live (a duplicate create).
    pub fn insert(&mut self, id: u32, val: T) -> Result<()> {
        if self.live.contains_key(&id) {
            return Err(GpuError::DuplicateId {
                kind: self.kind,
                id,
            });
        }
        let gen = self.next_gen;
        self.next_gen = self.next_gen.wrapping_add(1).max(1);
        self.live.insert(id, Slot { gen, val, guard: None });
        if let Some(journal) = &mut self.journal {
            journal.push(Undo::Inserted(id));
        }
        Ok(())
    }

    /// Look up a live resource. Errors (`UnknownId`) if it was never created or was freed.
    pub fn get(&self, id: u32) -> Result<&T> {
        let kind = self.kind;
        let slot = self.live.get(&id).ok_or(GpuError::UnknownId { kind, id })?;
        if let Some(access) = &slot.guard {
            access.check(kind, id)?;
        }
        Ok(&slot.val)
    }

    pub fn get_mut(&mut self, id: u32) -> Result<&mut T> {
        let kind = self.kind;
        let slot = self.live.get_mut(&id).ok_or(GpuError::UnknownId { kind, id })?;
        if let Some(access) = &slot.guard {
            access.check(kind, id)?;
        }
        Ok(&mut slot.val)
    }

    /// Attach an exclusive-use guard to a live resource. Set when a resource becomes shared.
    pub fn set_guard(&mut self, id: u32, guard: Access) -> Result<()> {
        let kind = self.kind;
        let slot = self.live.get_mut(&id).ok_or(GpuError::UnknownId { kind, id })?;
        slot.guard = Some(guard);
        Ok(())
    }

    /// Remove (destroy) a live resource. Errors on double-free / use-after-free (`UnknownId`).
    ///
    /// Outside a transaction the removed native is dropped immediately (freed). Inside a transaction the
    /// slot is parked in the journal — kept alive, NOT dropped — so [`rollback`](Self::rollback) can restore
    /// it if the frame NACKs; [`commit`](Self::commit) then frees it. Returns `()` (all call sites discard
    /// the native), which is what lets a journaled removal retain ownership of the native for rollback.
    pub fn remove(&mut self, id: u32) -> Result<()> {
        let slot = self.live.remove(&id).ok_or(GpuError::UnknownId {
            kind: self.kind,
            id,
        })?;
        match &mut self.journal {
            Some(journal) => journal.push(Undo::Removed(id, slot)),
            None => drop(slot),
        }
        Ok(())
    }

    pub fn contains(&self, id: u32) -> bool {
        self.live.contains_key(&id)
    }

    /// Current generation of a live id (for diagnostics / fence correlation).
    pub fn generation(&self, id: u32) -> Option<u32> {
        self.live.get(&id).map(|s| s.gen)
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

}
