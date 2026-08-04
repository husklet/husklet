//! Process and open-file-description advisory lock coordination.

mod algorithm;
mod coordinator;
mod exit;
mod model;

pub use coordinator::LockCoordinator;
pub use exit::PreparedLockExit;
pub use model::*;

#[cfg(test)]
mod lock_test;
