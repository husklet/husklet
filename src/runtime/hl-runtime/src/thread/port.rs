use crate::cpu::ExecutionSnapshot;
use hl_task::ThreadId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeThreadError {
    Duplicate,
    Missing,
    Invalid,
    Capacity,
}

pub trait PreparedThread: Send {
    /// Publishes a reservation whose fallible construction has completed.
    fn publish(self: Box<Self>);
}

/// Consumer-owned boundary for publishing runnable execution state.
///
/// Task creation and rollback remain runtime responsibilities. Implementations
/// only own runnable machines indexed by an already-published task identity.
pub trait RuntimeThreadPort: Send + Sync {
    fn stage(
        &self,
        thread: ThreadId,
        snapshot: ExecutionSnapshot,
    ) -> Result<Box<dyn PreparedThread>, RuntimeThreadError>;

    fn terminate(&self, thread: ThreadId) -> Result<(), RuntimeThreadError>;
}
