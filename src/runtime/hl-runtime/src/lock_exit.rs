use std::sync::Arc;

use hl_task::{ProcessId, ThreadId};
use hl_vfs::{AdvisoryLockCoordinator, PreparedLockExit, ProcessLockOwner};

use crate::{ExitParticipant, ExitRuntimeError, PreparedExitParticipant};

/// Reversibly removes process-owned POSIX locks during runtime exit.
pub struct VfsLockExit {
    locks: Arc<AdvisoryLockCoordinator>,
}

impl VfsLockExit {
    #[must_use]
    pub fn new(locks: Arc<AdvisoryLockCoordinator>) -> Self {
        Self { locks }
    }

    fn owner(process: ProcessId) -> ProcessLockOwner {
        let identity = process.fork_identity();
        ProcessLockOwner {
            identity: u64::from(identity.slot),
            generation: identity.generation,
        }
    }
}

impl ExitParticipant for VfsLockExit {
    fn prepare(
        &self,
        process: ProcessId,
        _: &[ThreadId],
    ) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError> {
        self.locks
            .prepare_exit(Self::owner(process))
            .map(|prepared| Box::new(prepared) as Box<dyn PreparedExitParticipant>)
            .map_err(|_| ExitRuntimeError::Failed)
    }
}

impl PreparedExitParticipant for PreparedLockExit {
    fn publish(&mut self) -> Result<(), ExitRuntimeError> {
        PreparedLockExit::publish(self).map_err(|_| ExitRuntimeError::Failed)
    }

    fn rollback(&mut self) {
        PreparedLockExit::rollback(self);
    }

    fn finish(&mut self) {
        PreparedLockExit::finish(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
    use hl_vfs::{Identity, LockCancellation, LockRange, RangeLockKind};

    fn fixture() -> (Arc<AdvisoryLockCoordinator>, ProcessId) {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        (Arc::new(AdvisoryLockCoordinator::new()), process)
    }

    fn populate(locks: &AdvisoryLockCoordinator, process: ProcessId) {
        locks
            .set_range(
                Identity { device: 1, inode: 2 },
                VfsLockExit::owner(process),
                Some(RangeLockKind::Write),
                LockRange { start: 0, end: None },
                false,
                &LockCancellation::default(),
            )
            .unwrap();
    }

    #[test]
    fn rollback_restores_locks() {
        let (locks, process) = fixture();
        populate(&locks, process);
        let participant = VfsLockExit::new(Arc::clone(&locks));
        let mut prepared = participant.prepare(process, &[]).unwrap();
        prepared.publish().unwrap();
        assert!(locks.snapshot().ranges.is_empty());
        prepared.rollback();
        assert_eq!(locks.snapshot().ranges.len(), 1);
    }

    #[test]
    fn finish_releases_admission() {
        let (locks, process) = fixture();
        populate(&locks, process);
        let participant = VfsLockExit::new(Arc::clone(&locks));
        let mut prepared = participant.prepare(process, &[]).unwrap();
        prepared.publish().unwrap();
        prepared.finish();
        populate(&locks, process);
        assert_eq!(locks.snapshot().ranges.len(), 1);
    }
}
