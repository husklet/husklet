//! Descriptor lifecycle integration for exec and process exit.

mod exec;
mod exit;

pub use exec::{Exec, ImageSlot, PreparedDescriptorExec};
pub use exit::Exit;

#[cfg(test)]
mod exec_test;
