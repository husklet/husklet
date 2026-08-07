use std::sync::atomic::{AtomicU64, Ordering};
use std::{error::Error, fmt};

use crate::{CancellationSink, ProcessId, SignalPendingSink, ThreadId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryConfig {
    pub max_processes: usize,
    pub max_threads: usize,
    pub max_groups: usize,
    pub max_pending_signals: usize,
    pub online_cpus: usize,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_processes: 1024,
            max_threads: 4096,
            max_groups: 32,
            max_pending_signals: 1024,
            online_cpus: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessLifecycle {
    Starting,
    Running,
    Stopped,
    Exiting,
    Zombie,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadLifecycle {
    Starting,
    Runnable,
    Blocked,
    Exiting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitStatus {
    Code(u8),
    Signal { signal: u8, dumped_core: bool },
}

impl ExitStatus {
    #[must_use]
    pub const fn wait_status(self) -> u32 {
        match self {
            Self::Code(code) => (code as u32) << 8,
            Self::Signal { signal, dumped_core } => signal as u32 | if dumped_core { 0x80 } else { 0 },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuUsage {
    pub self_nanoseconds: u64,
    pub children_nanoseconds: u64,
}

/// Process-lifetime CPU counter retained by admitted executors. A PID slot
/// reuse installs a different account, so late charges remain generation-safe.
#[derive(Debug, Default)]
pub struct CpuAccount {
    nanoseconds: AtomicU64,
}

impl CpuAccount {
    pub(crate) fn restored(nanoseconds: u64) -> Self {
        Self {
            nanoseconds: AtomicU64::new(nanoseconds),
        }
    }

    pub fn charge(&self, nanoseconds: u64) {
        let _ = self
            .nanoseconds
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(nanoseconds))
            });
    }

    #[must_use]
    pub fn nanoseconds(&self) -> u64 {
        self.nanoseconds.load(Ordering::Relaxed)
    }
}

impl CpuUsage {
    #[must_use]
    pub const fn total_nanoseconds(self) -> u64 {
        self.self_nanoseconds.saturating_add(self.children_nanoseconds)
    }
}

#[derive(Debug)]
#[must_use = "a clone plan must be committed or rolled back"]
pub struct CloneThreadPlan {
    pub(crate) source: ThreadId,
    pub(crate) thread: ThreadId,
    pub(crate) transaction: u64,
}

impl CloneThreadPlan {
    #[must_use]
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    #[must_use]
    pub const fn source(&self) -> ThreadId {
        self.source
    }
}

#[derive(Debug)]
#[must_use = "a fork plan must be committed or rolled back"]
pub struct ForkProcessPlan {
    pub(crate) parent: ProcessId,
    pub(crate) process: ProcessId,
    pub(crate) thread: ThreadId,
    pub(crate) transaction: u64,
}

impl ForkProcessPlan {
    #[must_use]
    pub const fn parent(&self) -> ProcessId {
        self.parent
    }

    #[must_use]
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    #[must_use]
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancellationEvent {
    thread: ThreadId,
}

impl CancellationEvent {
    pub(crate) const fn new(thread: ThreadId) -> Self {
        Self { thread }
    }

    pub fn deliver<S: CancellationSink>(self, sink: &S) -> Result<(), S::Error> {
        sink.request_cancellation(self.thread)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalPendingEvent {
    thread: ThreadId,
    pending: bool,
}

impl SignalPendingEvent {
    pub(crate) const fn new(thread: ThreadId, pending: bool) -> Self {
        Self { thread, pending }
    }

    pub fn deliver<S: SignalPendingSink>(self, sink: &S) -> Result<(), S::Error> {
        sink.pending_changed(self.thread, self.pending)
    }
}

/// Names the authority a denied operation lacked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Denial {
    Capability(u64),
    NamespaceNotVisible(crate::NamespaceId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskError {
    InvalidCapacity,
    ProcessLimit,
    ThreadLimit,
    SignalQueueLimit,
    GroupLimit,
    InvalidProcess(ProcessId),
    InvalidThread,
    InvalidSession,
    InvalidProcessGroup,
    WrongProcess,
    InvalidLifecycle,
    ProcessExeced,
    SessionLeader,
    InvalidPlan,
    InvalidLimit,
    HasChildren,
    NoChildren,
    NotWaitable,
    InitExited,
    InvalidSnapshot,
    PermissionDenied(Denial),
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "task registry failure: {self:?}")
    }
}

impl Error for TaskError {}

#[cfg(test)]
mod cpu_account_test {
    use std::sync::Arc;

    use super::CpuAccount;

    #[test]
    fn retired_account_isolated() {
        let retired = Arc::new(CpuAccount::default());
        let admitted = Arc::clone(&retired);
        let reused = Arc::new(CpuAccount::default());

        admitted.charge(17);

        assert_eq!(retired.nanoseconds(), 17);
        assert_eq!(reused.nanoseconds(), 0);
    }

    #[test]
    fn charge_saturates() {
        let account = CpuAccount::restored(u64::MAX - 1);
        account.charge(2);
        assert_eq!(account.nanoseconds(), u64::MAX);
    }
}
