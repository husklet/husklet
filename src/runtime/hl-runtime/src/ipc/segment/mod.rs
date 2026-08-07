//! `SysV` shared-memory segments, mappings, and transactional lifecycle.

mod binding;
mod lifecycle;
mod mapping;
mod memory;
mod syscalls;

pub use binding::{CommittedBindingSet, PreparedBindingSet};
pub(crate) use binding::{OwnedCommittedBindings, OwnedPreparedBindings};
pub use lifecycle::{CommittedFork, MemoryLifecycle, PreparedFork};
pub(crate) use lifecycle::{OwnedCommittedFork, OwnedPreparedFork};
pub(crate) use memory::Mapping;
pub use memory::{ForkBinding, MappingError, MemoryBinding, MemoryMappings, MemoryPort};

#[cfg(test)]
mod binding_test;
#[cfg(test)]
mod syscalls_test;
