//! Runtime event sources, operation admission, and syscall integration.

mod catalog;
mod checkpoint;
mod epoll;
mod errno;
mod operations;
mod sources;
mod syscalls;
mod timer;

#[cfg(test)]
pub(crate) use catalog::CatalogBoundEvent;
pub use operations::{OperationError, OperationRegistry};
pub use sources::{SignalEventSource, SourceError, TimerEventSource, WatchEventSource};
pub use syscalls::RuntimeEventSyscalls;
