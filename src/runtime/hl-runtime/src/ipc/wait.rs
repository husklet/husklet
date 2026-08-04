use std::sync::Arc;

use hl_sync::Interruption;

/// Supplies the interruption state belonging to the calling guest thread.
///
/// One runtime syscall object may be reused only for the thread represented by
/// this capability. Interrupting another thread must not wake this wait.
pub trait BlockingWait: Send + Sync {
    fn interruption(&self) -> Arc<Interruption>;
}
