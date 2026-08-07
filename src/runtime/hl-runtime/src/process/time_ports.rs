//! Port traits and value types for the process time and futex syscalls.

use hl_linux::{ClockIdentity, Errno, FutexPlan, FutexWaitVector, LinuxResult, ResourceUsage};
use hl_sync::{FutexDeadline, Interruption};
use hl_time::Timespec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSleepOutcome {
    Completed,
    Interrupted { remaining: Timespec },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceUsageScope {
    Process,
    Children,
}

pub trait RuntimeSleepPort: Send + Sync {
    fn sleep(
        &self,
        clock: ClockIdentity,
        absolute: bool,
        requested: Timespec,
        interruption: &Interruption,
    ) -> Result<RuntimeSleepOutcome, ()>;
}

/// Supplies CPU clocks for the calling guest process and thread.
///
/// The application adapter owns the host mechanism. Runtime keeps the two
/// Linux clock identities distinct so process-wide and caller-thread
/// accounting cannot be accidentally aliased.
pub trait CpuClockPort: Send + Sync {
    fn aggregate(&self) -> Result<Timespec, hl_time::ClockError>;

    fn user(&self) -> Result<Timespec, hl_time::ClockError> {
        self.aggregate()
    }

    fn current(&self) -> Result<Timespec, hl_time::ClockError>;

    fn resource_usage(
        &self,
        _process: hl_task::ProcessId,
        scope: ResourceUsageScope,
    ) -> Result<ResourceUsage, hl_time::ClockError> {
        let usage = match scope {
            ResourceUsageScope::Process => self.aggregate()?,
            ResourceUsageScope::Children => Timespec::ZERO,
        };
        Ok(ResourceUsage {
            user_seconds: usage.seconds() as i64,
            user_microseconds: i64::from(usage.subsecond_nanoseconds() / 1_000),
            ..ResourceUsage::default()
        })
    }
}

pub trait RuntimeFutexPort: Send + Sync {
    fn execute(&self, process: hl_task::ProcessId, thread: hl_task::ThreadId, plan: FutexPlan) -> LinuxResult;

    fn owner_exit(&self, _thread: hl_task::ThreadId) {}

    fn clear_tid_wake(&self, process: hl_task::ProcessId, thread: hl_task::ThreadId, address: u64) {
        for private in [true, false] {
            let _ = self.execute(
                process,
                thread,
                FutexPlan {
                    operation: hl_linux::FutexOperation::Wake,
                    address,
                    private,
                    value: 1,
                    secondary_address: 0,
                    secondary_count: 0,
                    secondary_value: 0,
                    bitset: u32::MAX,
                    deadline: None,
                    timeout_absolute: false,
                },
            );
        }
    }

    fn checkpoint_quiescent(&self) -> bool {
        true
    }

    fn wait_multiple(
        &self,
        _thread: hl_task::ThreadId,
        _vectors: &[FutexWaitVector],
        _deadline: Option<FutexDeadline>,
    ) -> LinuxResult {
        LinuxResult::Error(Errno::ENOSYS)
    }
}

pub trait RobustExitPort: hl_task::RobustExitCleanup<Error = ()> + Send + Sync {}

impl<T> RobustExitPort for T where T: hl_task::RobustExitCleanup<Error = ()> + Send + Sync {}
