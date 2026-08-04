use hl_task::ProcessId;
use hl_vfs::{ProcfsError, ProcfsMemoryView};

use super::TaskProcfs;

/// Consumer capability for sampling one live guest address space.
pub trait MemoryPort: Send + Sync {
    fn sample(&self, process: ProcessId) -> Result<ProcfsMemoryView, ProcfsError>;

    fn address_space(&self, _process: ProcessId) -> Result<hl_vfs::ProcfsAddressSpaceView, ProcfsError> {
        Err(ProcfsError::NotFound)
    }

    fn environment(&self, _process: ProcessId) -> Result<Vec<u8>, ProcfsError> {
        Err(ProcfsError::NotFound)
    }
}

impl TaskProcfs {
    pub(super) fn memory_view(&self, process: ProcessId) -> Result<Option<ProcfsMemoryView>, ProcfsError> {
        match (&self.memory, self.current) {
            (Some(memory), Some(current)) if current == process => memory.sample(process).map(Some),
            _ => Ok(None),
        }
    }
}
