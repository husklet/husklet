//! Stable process entry for the production retained-C execution worker.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedWorkerError {
    Unsupported,
    Descriptor,
    Plan,
    Control,
    Create,
    Start,
}

impl RetainedWorkerError {
    #[must_use]
    pub const fn status(self) -> i32 {
        match self {
            Self::Unsupported => 69,
            Self::Descriptor | Self::Plan => 65,
            Self::Control | Self::Create | Self::Start => 125,
        }
    }
}

/// Runs one isolated retained-C engine using an inherited launch-plan descriptor and control socket.
///
/// The descriptors become owned by this one-shot worker and must not be used again by the caller.
pub fn run(plan_descriptor: i32, control_descriptor: i32) -> Result<i32, RetainedWorkerError> {
    #[cfg(hl_retained_c)]
    {
        crate::execution::worker::run(plan_descriptor, control_descriptor).map_err(|error| match error {
            crate::execution::worker::WorkerError::Descriptor => RetainedWorkerError::Descriptor,
            crate::execution::worker::WorkerError::Plan => RetainedWorkerError::Plan,
            crate::execution::worker::WorkerError::Control => RetainedWorkerError::Control,
            crate::execution::worker::WorkerError::Create => RetainedWorkerError::Create,
            crate::execution::worker::WorkerError::Start => RetainedWorkerError::Start,
        })
    }
    #[cfg(not(hl_retained_c))]
    {
        let _ = (plan_descriptor, control_descriptor);
        Err(RetainedWorkerError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::RetainedWorkerError;

    #[cfg(not(hl_retained_c))]
    #[test]
    fn unsupported_build_rejects_worker_deterministically() {
        assert_eq!(super::run(3, 4), Err(RetainedWorkerError::Unsupported));
    }

    #[test]
    fn statuses_are_bounded_process_outcomes() {
        for error in [
            RetainedWorkerError::Unsupported,
            RetainedWorkerError::Descriptor,
            RetainedWorkerError::Plan,
            RetainedWorkerError::Control,
            RetainedWorkerError::Create,
            RetainedWorkerError::Start,
        ] {
            assert!((1..=125).contains(&error.status()));
        }
    }
}
