//! Epoll ownership graph, descriptor integration, and exec staging.

mod control;
mod exec;
mod graph;
mod range;

pub(crate) use control::RuntimeEpollBatch;
pub use control::{Control, ControlError, DescriptorTableId, RuntimeDescriptorTable};
pub(crate) use exec::PreparedEpollExec;
pub use graph::{EdgeSnapshot, GraphError, GraphSnapshot, OwnershipGraph};

#[cfg(test)]
mod control_test;
#[cfg(test)]
mod graph_test;
