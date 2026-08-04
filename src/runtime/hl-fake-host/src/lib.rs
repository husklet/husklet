//! Deterministic, host-neutral adapters for runtime and differential tests.

#![forbid(unsafe_code)]

mod clock;
mod mapping;
mod page;
mod process;
mod state;
mod storage;
mod transport;
mod vfs;

pub use clock::VirtualClock;
pub use mapping::MappingAdapter;
pub use page::{GuestPageStore, PAGE_SIZE, Protection as PageProtection, WriteReservation};
pub use process::{ProcessAdapter, ProcessExit, ProcessToken};
pub use state::{Call, FakeHost, FakeHostError, Fault, ResourceCounts, ResourceKind};
pub use storage::{FileToken, StorageAdapter};
pub use transport::{ProviderEndpoint, SocketAdapter, SocketToken};
pub use vfs::{InodeIdentity, NodeMetadata, Tree as VfsTree, WatchEvent};

#[cfg(test)]
mod host_test;
