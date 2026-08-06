use crate::{ProcessId, ThreadId};

pub const ROBUST_LIST_HEAD_SIZE: u64 = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RobustListRegistration {
    pub head: u64,
}

impl RobustListRegistration {
    #[must_use]
    pub const fn new(head: u64) -> Self {
        Self { head }
    }

    #[must_use]
    pub const fn length(self) -> u64 {
        ROBUST_LIST_HEAD_SIZE
    }
}

/// The execution/runtime consumer must perform the actual guest-memory
/// `OWNER_DIED` transition and futex wake. Missing integration is not success.
pub trait RobustExitCleanup {
    type Error;

    fn cleanup(
        &self,
        process: ProcessId,
        thread: ThreadId,
        registration: RobustListRegistration,
    ) -> Result<(), Self::Error>;
}
