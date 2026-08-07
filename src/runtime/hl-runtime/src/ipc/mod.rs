//! SysV IPC syscall composition and cross-domain lifecycle transactions.

mod blocking;
mod control;
mod dispatch;
mod error_projection;
mod exec;
mod exit;
mod lifecycle;
mod posix;
mod segment;
mod syscalls;
mod wait;

pub use exec::{EmptyIpcExec, ExecParticipant};
pub use exit::ExitHandler;
pub use lifecycle::RuntimeIpcLifecycle;
pub use segment::{
    CommittedBindingSet, CommittedFork, ForkBinding, MappingError, MemoryBinding, MemoryLifecycle, MemoryMappings,
    MemoryPort, PreparedBindingSet, PreparedFork,
};
pub(crate) use segment::{OwnedCommittedFork, OwnedPreparedFork};
pub use syscalls::RuntimeIpcSyscalls;
pub use wait::BlockingWait;

#[cfg(test)]
mod control_test;
#[cfg(test)]
mod lifecycle_test;
#[cfg(test)]
mod syscalls_test;
#[cfg(test)]
pub(crate) mod test_support;
