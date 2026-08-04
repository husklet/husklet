use hl_descriptor::OperationLease;
use hl_linux::{AdvicePlan, LockAllPlan, MemoryRangePlan, MsyncPlan};
use hl_memory::Backing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMemoryError {
    Invalid,
    NoMemory,
    Exists,
    BadDescriptor,
    Permission,
    Busy,
    Unsupported,
    Failed,
}

/// Composition-owned services that are not part of the virtual mapping ledger.
pub trait RuntimeMemoryHost: std::fmt::Debug + Send + Sync {
    fn advise(&self, plan: AdvicePlan) -> Result<(), RuntimeMemoryError>;
    fn residency(&self, plan: MemoryRangePlan) -> Result<Vec<bool>, RuntimeMemoryError>;
    fn lock(&self, plan: Option<MemoryRangePlan>, on_fault: bool) -> Result<(), RuntimeMemoryError>;
    fn unlock(&self, plan: Option<MemoryRangePlan>) -> Result<(), RuntimeMemoryError>;
    fn lock_all(&self, plan: LockAllPlan) -> Result<(), RuntimeMemoryError>;
    fn unlock_all(&self) -> Result<(), RuntimeMemoryError>;
    fn sync(&self, plan: MsyncPlan) -> Result<(), RuntimeMemoryError>;
}

/// Converts one pinned open description into a stable mapping backing.
pub trait DescriptorMappingSource: std::fmt::Debug + Send + Sync {
    fn backing(
        &self,
        descriptor: &OperationLease,
        offset: u64,
        length: u64,
        shared: bool,
        writable: bool,
    ) -> Result<Backing, RuntimeMemoryError>;
}
