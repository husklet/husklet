//! Task signal queues, frame publication, and syscall-boundary integration.

mod boundary;
mod frame;
mod queue;
mod send;
mod state;
mod wait;

pub use frame::{FramePort, PreparedFramePublication};
pub use queue::TaskSignalQueue;

#[cfg(test)]
mod queue_test;
