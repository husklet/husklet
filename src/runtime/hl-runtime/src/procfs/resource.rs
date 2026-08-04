use std::sync::Arc;

use hl_descriptor::DescriptorTable;
use hl_task::ProcessId;
use hl_vfs::ProcfsError;

use crate::WorkingDirectory;

/// Consumer-owned access to one live process's filesystem resources.
pub trait ResourcePort: Send + Sync {
    fn descriptors(&self, process: ProcessId) -> Result<Arc<DescriptorTable>, ProcfsError>;
    fn working(&self, process: ProcessId) -> Result<Arc<WorkingDirectory>, ProcfsError>;
}
