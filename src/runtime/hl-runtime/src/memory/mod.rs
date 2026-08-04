//! Runtime memory syscalls, policies, and lifecycle integration.

mod brk;
pub(crate) mod charge;
mod errno;
mod exit;
mod ports;
mod remap;
mod syscalls;

pub use brk::{AnonymousMemoryAccount, BRK_BACKING_IDENTITY, BrkRegion, BrkSnapshot};
pub use charge::AnonymousMemoryLease;
pub use exit::Exit;
pub use ports::{DescriptorMappingSource, RuntimeMemoryError, RuntimeMemoryHost};
pub use syscalls::RuntimeMemorySyscalls;
