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
use std::sync::{Arc, Mutex};

use crate::protocol::model::error::{GpuError, Result};

/// A connection. Sessions are distinct per transport connection; two drivers in one guest workspace get
/// two of these.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SessionId(pub u64);

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
    state: MapState,
    /// The owner destroyed its id while importers remained. The storage is retained; see `release`.
    owner_released: bool,
}

impl Entry {
    /// Who the retained bytes are charged to. The rule is "the charge follows the last live reference":
    /// while the owner holds the resource it pays, and once the owner has released, the importers keeping
    /// it alive pay. That is what stops a deferred release being a leak nobody can see — it lands in the
    /// accounting that already exists, bounded by the budget of whoever is actually keeping it alive.
    fn charged_to(&self) -> Vec<SessionId> {
        if self.owner_released {
            self.importers.clone()
        } else {
            vec![self.key.session]
        }
    }

    fn is_dead(&self) -> bool {
        self.owner_released && self.importers.is_empty()
    }
}

/// The process-global export table. Cloning shares the same registry, exactly as [`GlobalLedger`] does.
///
/// [`GlobalLedger`]: super::resources::GlobalLedger
#[derive(Clone, Default)]
pub struct Exports {
    inner: Arc<Mutex<Registry>>,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<ExportId, Entry>,
    by_resource: HashMap<ResourceKey, ExportId>,
    /// Monotonic. Never rewound, never recycled — see [`ExportId`].
    next: u64,
}

impl Exports {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a resource for import. Idempotent: re-exporting an already-exported resource returns the
    /// existing [`ExportId`] rather than minting a second entry for it.
    pub fn export(&self, key: ResourceKey, resource: Shared, bytes: u64) -> Result<ExportId> {
        // The sibling creation paths refuse a zero or absurd size; the export path must not become the
        // one that does not. That exact asymmetry was found and fixed in `hl-vulkan`.
        if bytes == 0 {
            return Err(GpuError::Invalid("export: a zero-length resource"));
        }
        let mut registry = self.inner.lock().unwrap();
        if let Some(existing) = registry.by_resource.get(&key) {
            return Ok(*existing);
        }
        registry.next += 1;
        let id = ExportId(registry.next);
        registry.entries.insert(
            id,
            Entry {
                key,
                resource,
                bytes,
                importers: Vec::new(),
                state: MapState::Unmapped,
                owner_released: false,
            },
        );
        registry.by_resource.insert(key, id);
        Ok(id)
    }

    /// Take a reference to an exported resource. Returns the native object and its authoritative length.
    ///
    /// Refuses a stale or unknown id with a typed error rather than a default — "could not reach the
    /// subject" must never be indistinguishable from "here is your buffer".
    pub fn import(&self, importer: SessionId, id: ExportId) -> Result<(Shared, u64)> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("import: no such export, or it is no longer live"))?;
        if entry.key.session == importer {
            return Err(GpuError::Invalid(
                "import: a session cannot import its own export",
            ));
        }
        if entry.importers.contains(&importer) {
            return Err(GpuError::Invalid("import: already imported by this session"));
        }
        entry.importers.push(importer);
        Ok((Arc::clone(&entry.resource), entry.bytes))
    }

    /// Drop an importer's reference. Frees the resource if it was the last one and the owner had released.
    pub fn release_import(&self, importer: SessionId, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("release: no such export"))?;
        // Leaving "unregister while mapped" undefined is how a resource ends up permanently MappedBy a
        // session that has gone. It is defined here as an implicit unmap by the holder, and refused for
        // anyone else.
        if entry.state == MapState::MappedBy(importer) {
            entry.state = MapState::Unmapped;
        }
        let before = entry.importers.len();
        entry.importers.retain(|s| *s != importer);
        if entry.importers.len() == before {
            return Err(GpuError::Invalid("release: not an importer of this export"));
        }
        registry.collect(id);
        Ok(())
    }

    /// The owner destroyed its id. The destroy SUCCEEDS and the storage is retained while importers
    /// remain; the charge moves to them.
    ///
    /// Refusing the destroy was considered and rejected — deleting a buffer is legal application
    /// behaviour and an application that gets an error there has no recourse. Silent retention was also
    /// rejected: that is an invisible leak.
    pub fn owner_release(&self, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("owner release: no such export"))?;
        entry.owner_released = true;
        let key = entry.key;
        registry.by_resource.remove(&key);
        registry.collect(id);
        Ok(())
    }

    /// Claim exclusive use. Refuses if already mapped by anyone, including the caller.
    pub fn map(&self, session: SessionId, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("map: no such export"))?;
        if !entry.importers.contains(&session) && entry.key.session != session {
            return Err(GpuError::Invalid("map: not a party to this export"));
        }
        match entry.state {
            MapState::Unmapped => {
                entry.state = MapState::MappedBy(session);
                Ok(())
            }
            MapState::MappedBy(_) => Err(GpuError::Invalid("map: already mapped")),
        }
    }

    /// Release an exclusive claim. Only the holder may.
    pub fn unmap(&self, session: SessionId, id: ExportId) -> Result<()> {
        let mut registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("unmap: no such export"))?;
        if entry.state != MapState::MappedBy(session) {
            return Err(GpuError::Invalid("unmap: not held by this session"));
        }
        entry.state = MapState::Unmapped;
        Ok(())
    }

    /// **The guard.** Whether `session` may touch this resource right now.
    ///
    /// This is the predicate the single resource-resolution point calls. While a resource is mapped, use
    /// by any session other than the holder is refused. Slice 1 defines and tests the predicate; wiring
    /// it into `ResourceTable::get`/`get_mut` — the one place every command resolves an id to its native
    /// object — is the next slice, and is the gate the capability must not ship without.
    pub fn check_access(&self, session: SessionId, id: ExportId) -> Result<()> {
        let registry = self.inner.lock().unwrap();
        let entry = registry
            .entries
            .get(&id)
            .ok_or(GpuError::Invalid("access: no such export"))?;
        match entry.state {
            MapState::Unmapped => Ok(()),
            MapState::MappedBy(holder) if holder == session => Ok(()),
            MapState::MappedBy(_) => Err(GpuError::Invalid(
                "access: the resource is mapped by another session",
            )),
        }
    }

    /// Retained bytes currently charged to `session` by this registry.
    pub fn bytes_charged_to(&self, session: SessionId) -> u64 {
        let registry = self.inner.lock().unwrap();
        registry
            .entries
            .values()
            .filter(|e| e.charged_to().contains(&session))
            .map(|e| e.bytes)
            .sum()
    }

    pub fn is_live(&self, id: ExportId) -> bool {
        self.inner.lock().unwrap().entries.contains_key(&id)
    }

    /// Drop every reference held by a departing session, in both directions.
    pub fn forget_session(&self, session: SessionId) {
        let mut registry = self.inner.lock().unwrap();
        let ids: Vec<ExportId> = registry.entries.keys().copied().collect();
        for id in ids {
            if let Some(entry) = registry.entries.get_mut(&id) {
                if entry.state == MapState::MappedBy(session) {
                    entry.state = MapState::Unmapped;
                }
                entry.importers.retain(|s| *s != session);
                if entry.key.session == session {
                    entry.owner_released = true;
                    let key = entry.key;
                    registry.by_resource.remove(&key);
                }
            }
            registry.collect(id);
        }
    }
}

impl Registry {
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
