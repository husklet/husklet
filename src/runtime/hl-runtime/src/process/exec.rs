use hl_linux::{Errno, ExecPlan, GuestMemory, LinuxResult, ProcessAbi};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_task::{ProcessId, ThreadId};

use crate::{RuntimeProcessSyscalls, TimerRegistry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeExecError {
    Unsupported,
    NotFound,
    BadDescriptor,
    Access,
    Loop,
    Invalid,
    NameTooLong,
    Format,
    TooBig,
    TextBusy,
    NoMemory,
    Failed,
}

pub trait PreparedExec: Send {
    fn commit(self: Box<Self>) -> Result<(), RuntimeExecError>;
}

struct TimerExec {
    prepared: Box<dyn PreparedExec>,
    timers: Arc<TimerRegistry>,
}

impl PreparedExec for TimerExec {
    fn commit(self: Box<Self>) -> Result<(), RuntimeExecError> {
        self.prepared.commit()?;
        self.timers.clear();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExecKey {
    pub thread: ThreadId,
    pub generation: u64,
}

struct QueueState {
    next: u64,
    pending: BTreeMap<ExecKey, Box<dyn PreparedExec>>,
}

pub struct ExecQueue {
    state: Mutex<QueueState>,
}

impl Default for ExecQueue {
    fn default() -> Self {
        Self {
            state: Mutex::new(QueueState {
                next: 1,
                pending: BTreeMap::new(),
            }),
        }
    }
}

impl ExecQueue {
    pub fn stage(&self, thread: ThreadId, prepared: Box<dyn PreparedExec>) -> Result<ExecKey, RuntimeExecError> {
        let mut state = self.state.lock().map_err(|_| RuntimeExecError::Failed)?;
        if state.pending.keys().any(|key| key.thread == thread) {
            return Err(RuntimeExecError::Failed);
        }
        let key = ExecKey {
            thread,
            generation: state.next,
        };
        state.next = state.next.wrapping_add(1).max(1);
        state.pending.insert(key, prepared);
        Ok(key)
    }

    pub fn current(&self, thread: ThreadId) -> Option<ExecKey> {
        self.state
            .lock()
            .ok()?
            .pending
            .keys()
            .find(|key| key.thread == thread)
            .copied()
    }

    pub fn take(&self, key: ExecKey) -> Option<Box<dyn PreparedExec>> {
        self.state.lock().ok()?.pending.remove(&key)
    }
}

/// Composition-root exec transaction.
///
/// Implementations validate and load through hl-loader, stage the new CPU/TLS
/// image, and atomically coordinate CLOEXEC, signal reset, seccomp
/// preservation and `TaskRegistry` exec state. Any failure must roll back every
/// staged domain and leave the old image runnable.
pub trait RuntimeExecPort: Send + Sync {
    fn validate(&self, _process: ProcessId, _thread: ThreadId, _plan: &ExecPlan) -> Result<(), RuntimeExecError> {
        Ok(())
    }

    fn prepare(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: ExecPlan,
    ) -> Result<Box<dyn PreparedExec>, RuntimeExecError>;

    fn exec(&self, process: ProcessId, thread: ThreadId, plan: ExecPlan) -> Result<(), RuntimeExecError> {
        self.prepare(process, thread, plan)?.commit()
    }
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn execve(&self, path: u64, arguments: u64, environment: u64) -> LinuxResult {
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let plan = match abi.exec_path(None, path, 0) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if let Err(error) = self.validate_exec(&plan) {
            return error;
        }
        let plan = match abi.exec_vectors(plan, arguments, environment) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.perform_exec(plan)
    }

    pub(crate) fn execveat(
        &self,
        directory: i32,
        path: u64,
        arguments: u64,
        environment: u64,
        flags: u32,
    ) -> LinuxResult {
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let plan = match abi.exec_path(Some(directory), path, flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if let Err(error) = self.validate_exec(&plan) {
            return error;
        }
        let plan = match abi.exec_vectors(plan, arguments, environment) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.perform_exec(plan)
    }

    fn validate_exec(&self, plan: &ExecPlan) -> Result<(), LinuxResult> {
        let Some(port) = &self.exec else {
            return Err(LinuxResult::Error(Errno::ENOSYS));
        };
        port.validate(self.process, self.thread, plan).map_err(Self::exec_error)
    }

    fn perform_exec(&self, plan: ExecPlan) -> LinuxResult {
        let Some(port) = &self.exec else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        // `comm` is a fixed 16-byte array, so naming the image costs no allocation
        // on the path where logging is disabled.
        let comm = plan.comm();
        let (arguments, flags) = (plan.arguments.len(), plan.flags);
        let result = port.prepare(self.process, self.thread, plan).and_then(|prepared| {
            let prepared = match &self.timers {
                Some(timers) => Box::new(TimerExec {
                    prepared,
                    timers: Arc::clone(timers),
                }) as Box<dyn PreparedExec>,
                None => prepared,
            };
            match &self.exec_queue {
                Some(queue) => queue.stage(self.thread, prepared).map(drop),
                None => prepared.commit(),
            }
        });
        match result {
            Ok(()) => {
                hl_log::hl_info!(
                    hl_log::tag::TASK,
                    "process image replaced process={} thread={} name={} arguments={arguments} flags={flags:#x}",
                    self.process.number(),
                    self.thread.number(),
                    String::from_utf8_lossy(comm.split(|byte| *byte == 0).next().unwrap_or_default()),
                );
                LinuxResult::Value(0)
            }
            Err(error) => {
                hl_log::hl_debug!(
                    hl_log::tag::TASK,
                    "process exec refused process={} name={} error={error:?}",
                    self.process.number(),
                    String::from_utf8_lossy(comm.split(|byte| *byte == 0).next().unwrap_or_default()),
                );
                Self::exec_error(error)
            }
        }
    }

    fn exec_error(error: RuntimeExecError) -> LinuxResult {
        match error {
            RuntimeExecError::Unsupported => LinuxResult::Error(Errno::ENOSYS),
            RuntimeExecError::NotFound => LinuxResult::Error(Errno::ENOENT),
            RuntimeExecError::BadDescriptor => LinuxResult::Error(Errno::EBADF),
            RuntimeExecError::Access => LinuxResult::Error(Errno::EACCES),
            RuntimeExecError::Loop => LinuxResult::Error(Errno::ELOOP),
            RuntimeExecError::Invalid => LinuxResult::Error(Errno::EINVAL),
            RuntimeExecError::NameTooLong => LinuxResult::Error(Errno::ENAMETOOLONG),
            RuntimeExecError::Format => LinuxResult::Error(Errno::from_raw(8)),
            RuntimeExecError::TooBig => LinuxResult::Error(Errno::E2BIG),
            RuntimeExecError::TextBusy => LinuxResult::Error(Errno::from_raw(26)),
            RuntimeExecError::NoMemory => LinuxResult::Error(Errno::ENOMEM),
            RuntimeExecError::Failed => LinuxResult::Error(Errno::EIO),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{PreparedExec, RuntimeExecError, TimerExec};
    use crate::TimerRegistry;
    use hl_linux::ClockIdentity;

    struct ResultExec(Result<(), RuntimeExecError>);

    impl PreparedExec for ResultExec {
        fn commit(self: Box<Self>) -> Result<(), RuntimeExecError> {
            self.0
        }
    }

    #[test]
    fn posix_timers_clear_only_after_successful_commit() {
        let timers = Arc::new(TimerRegistry::default());
        assert_eq!(timers.allocate_for_test(ClockIdentity::Monotonic), Some(0));

        let failed = Box::new(TimerExec {
            prepared: Box::new(ResultExec(Err(RuntimeExecError::Failed))),
            timers: Arc::clone(&timers),
        });
        assert_eq!(failed.commit(), Err(RuntimeExecError::Failed));
        assert_eq!(timers.allocated_for_test(), 1);

        let committed = Box::new(TimerExec {
            prepared: Box::new(ResultExec(Ok(()))),
            timers: Arc::clone(&timers),
        });
        assert_eq!(committed.commit(), Ok(()));
        assert_eq!(timers.allocated_for_test(), 0);
    }
}
