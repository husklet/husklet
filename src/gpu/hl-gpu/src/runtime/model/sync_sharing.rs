//! Process-global ownership for synchronization objects shared across sessions.
//!
//! This is deliberately separate from resource [`super::sharing::Exports`]. A synchronization object
//! has no resident byte length and must not invent one merely to reuse memory accounting. This registry
//! owns only process-lifetime identity and reference lifetime; it defines no wire encoding, fd ABI,
//! CUDA/Vulkan spelling, signal/wait semantics, or advertised capability.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::sharing::SessionId;

/// A process-global synchronization export identity. Values are monotonic and never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SyncExportId {
    serial: u64,
    authenticity: u128,
}

impl SyncExportId {
    pub fn from_parts(serial: u64, authenticity: u128) -> Self {
        Self {
            serial,
            authenticity,
        }
    }

    pub fn serial(self) -> u64 {
        self.serial
    }

    pub fn authenticity(self) -> u128 {
        self.authenticity
    }
}

/// A type-erased synchronization object retained by every live owner/import reference.
pub type SharedSync = Arc<dyn Any + Send + Sync>;

struct Entry {
    owner: SessionId,
    object: SharedSync,
    importers: HashSet<SessionId>,
    owner_released: bool,
}

impl Entry {
    fn is_dead(&self) -> bool {
        self.owner_released && self.importers.is_empty()
    }
}

struct Registry {
    next: u64,
    entries: HashMap<SyncExportId, Entry>,
}

/// One process-global synchronization export registry. Clones address the same table.
#[derive(Clone)]
pub struct SyncExports {
    inner: Arc<Mutex<Registry>>,
}

impl Default for SyncExports {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncExports {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Registry {
                next: 1,
                entries: HashMap::new(),
            })),
        }
    }

    /// Mint a never-reused identity retaining `object` for `owner`.
    pub fn export(&self, owner: SessionId, object: SharedSync) -> Result<SyncExportId> {
        let mut registry = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut authenticity = [0u8; 16];
        while authenticity == [0; 16] {
            getrandom::fill(&mut authenticity)
                .map_err(|_| GpuError::ResourceLimit("synchronization export authenticity"))?;
        }
        let id = SyncExportId::from_parts(registry.next, u128::from_le_bytes(authenticity));
        registry.next = registry
            .next
            .checked_add(1)
            .ok_or(GpuError::ResourceLimit("synchronization export ids"))?;
        registry.entries.insert(
            id,
            Entry {
                owner,
                object,
                importers: HashSet::new(),
                owner_released: false,
            },
        );
        Ok(id)
    }

    /// Attach one importing session and return an alias to the authoritative object.
    pub fn import(&self, importer: SessionId, id: SyncExportId) -> Result<SharedSync> {
        let mut registry = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("stale synchronization export"))?;
        if entry.owner_released {
            return Err(GpuError::Invalid("stale synchronization export"));
        }
        if entry.owner == importer {
            return Err(GpuError::Invalid("synchronization self-import"));
        }
        if !entry.importers.insert(importer) {
            return Err(GpuError::Invalid("duplicate synchronization import"));
        }
        Ok(Arc::clone(&entry.object))
    }

    /// Release the owner's reference. Live imports retain the object and continue to resolve.
    pub fn owner_release(&self, owner: SessionId, id: SyncExportId) -> Result<()> {
        let mut registry = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("stale synchronization export"))?;
        if entry.owner != owner || entry.owner_released {
            return Err(GpuError::Invalid(
                "synchronization export is not owned by caller",
            ));
        }
        entry.owner_released = true;
        if entry.is_dead() {
            registry.entries.remove(&id);
        }
        Ok(())
    }

    /// Release one import held by `importer`.
    pub fn release_import(&self, importer: SessionId, id: SyncExportId) -> Result<()> {
        let mut registry = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = registry
            .entries
            .get_mut(&id)
            .ok_or(GpuError::Invalid("stale synchronization export"))?;
        if !entry.importers.remove(&importer) {
            return Err(GpuError::Invalid(
                "synchronization import is not held by caller",
            ));
        }
        if entry.is_dead() {
            registry.entries.remove(&id);
        }
        Ok(())
    }

    /// Drop every owner and importer reference belonging to a departed session.
    pub fn forget_session(&self, session: SessionId) {
        let mut registry = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        registry.entries.retain(|_, entry| {
            if entry.owner == session {
                entry.owner_released = true;
            }
            entry.importers.remove(&session);
            !entry.is_dead()
        });
    }

    pub fn is_live(&self, id: SyncExportId) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entries
            .contains_key(&id)
    }
}
