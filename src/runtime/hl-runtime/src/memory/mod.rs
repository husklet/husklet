//! Runtime memory syscalls, policies, and lifecycle integration.

mod brk;
mod errno;
mod exit;
mod ports;
mod remap;
mod syscalls;

pub use brk::{BRK_BACKING_IDENTITY, BrkAccount, BrkRegion, BrkSnapshot};
pub use exit::Exit;
pub use ports::{DescriptorMappingSource, RuntimeMemoryError, RuntimeMemoryHost};
pub use syscalls::RuntimeMemorySyscalls;
