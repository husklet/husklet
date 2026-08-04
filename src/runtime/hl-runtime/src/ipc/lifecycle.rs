use std::sync::Arc;

use hl_ipc::IpcCatalog;

use crate::{MappingError, MemoryLifecycle, MemoryPort};

/// SysV process-lifecycle capability installed inside fork/exec/exit transactions.
pub struct RuntimeIpcLifecycle {
    catalog: Arc<IpcCatalog>,
    shared: MemoryLifecycle,
}

impl RuntimeIpcLifecycle {
    #[must_use]
    pub fn new(catalog: Arc<IpcCatalog>, mappings: Arc<dyn MemoryPort>) -> Self {
        Self {
            shared: MemoryLifecycle::new(catalog.clone(), mappings),
            catalog,
        }
    }

    pub fn fork(&self, parent: u32, child: u32, now: u64, child_mappings: &dyn MemoryPort) -> Result<(), MappingError> {
        self.shared.fork(parent, child, now, child_mappings)?;
        self.catalog.with_semaphores(|namespace| namespace.fork(parent, child));
        Ok(())
    }

    pub fn exec(&self, process: u32, now: u64) -> Result<(), MappingError> {
        self.shared.detach_process(process, now)?;
        self.catalog.with_semaphores(|namespace| namespace.exec(process));
        Ok(())
    }

    pub fn exit(&self, process: u32, now: u64) -> Result<(), MappingError> {
        self.shared.detach_process(process, now)?;
        self.catalog.with_semaphores(|namespace| namespace.exit(process, now));
        Ok(())
    }
}
