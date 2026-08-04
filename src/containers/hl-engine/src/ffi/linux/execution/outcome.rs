use super::GuestExecutor;
use crate::engine::{EngineError, EngineExit, ExitKind};

impl GuestExecutor {
    pub(super) const fn thread_error(_: hl_runtime::RuntimeThreadError) -> EngineError {
        EngineError::WaitFailed
    }

    pub(super) const fn code(status: i32) -> EngineExit {
        EngineExit {
            kind: ExitKind::Code,
            guest_status: status,
            detail: 0,
            fault: None,
        }
    }

    pub(super) const fn signal(signal: i32) -> EngineExit {
        EngineExit {
            kind: ExitKind::Signal,
            guest_status: signal,
            detail: 0,
            fault: None,
        }
    }
}
