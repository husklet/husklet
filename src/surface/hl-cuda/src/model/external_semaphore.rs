//! Imported cross-API timeline semaphore handles.

use std::collections::HashMap;

use hl_gpu::SyncExportId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalSemaphore(pub u64);

pub struct ExternalSemaphores {
    live: HashMap<ExternalSemaphore, SyncExportId>,
    next: u64,
}

impl ExternalSemaphores {
    pub fn new() -> Self {
        Self {
            live: HashMap::new(),
            next: 1,
        }
    }

    pub fn insert(&mut self, export: SyncExportId) -> ExternalSemaphore {
        let handle = ExternalSemaphore(self.next);
        self.next += 1;
        self.live.insert(handle, export);
        handle
    }

    pub fn export(&self, handle: ExternalSemaphore) -> Option<SyncExportId> {
        self.live.get(&handle).copied()
    }

    pub fn remove(&mut self, handle: ExternalSemaphore) -> Option<SyncExportId> {
        self.live.remove(&handle)
    }
}

impl Default for ExternalSemaphores {
    fn default() -> Self {
        Self::new()
    }
}
