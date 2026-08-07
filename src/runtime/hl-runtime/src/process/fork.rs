use hl_execution::ExecutionCpuSnapshot;
use hl_linux::{ClonePlan, Errno, GuestAccess, GuestMemory, LinuxResult, ProcessAbi};
use hl_task::{ProcessId, ThreadId};

use crate::RuntimeProcessSyscalls;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeForkError {
    Unsupported,
    Invalid,
    Again,
    NoMemory,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeForkResult {
    pub process: ProcessId,
    pub thread: ThreadId,
}

/// Composition-root fork capability.
///
/// Implementations own the complete `RuntimeForkCoordinator` transaction,
/// including task, descriptor, memory, provider and execution participants.
pub trait RuntimeForkPort: Send + Sync {
    fn fork(&self, parent: ProcessId, thread: ThreadId, plan: ClonePlan)
    -> Result<RuntimeForkResult, RuntimeForkError>;
}

/// Execution-bound process fork trap.
///
/// The syscall router supplies the live CPU snapshot while its machine state
/// is already locked, so implementations never relock the execution machine.
pub trait Trap: Send + Sync {
    fn fork(&self, cpu: &ExecutionCpuSnapshot, plan: ClonePlan) -> LinuxResult;
}

/// Explicitly configured fork boundary that rejects execution.
///
/// Useful to prove composition wiring independently of a process runtime.
pub struct RejectingForkPort;

impl RuntimeForkPort for RejectingForkPort {
    fn fork(&self, _: ProcessId, _: ThreadId, _: ClonePlan) -> Result<RuntimeForkResult, RuntimeForkError> {
        Err(RuntimeForkError::Unsupported)
    }
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn fork_plain(&self, vfork: bool) -> LinuxResult {
        let Some(port) = &self.fork else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let plan = if vfork { abi.vfork() } else { abi.fork() };
        self.perform_fork(port.as_ref(), plan)
    }

    pub(crate) fn clone_legacy(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = match ProcessAbi::new(&self.memory, self.architecture).clone_legacy(
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
            arguments[4],
        ) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if let Err(error) = self.probe_parent_outputs(&plan) {
            return LinuxResult::Error(error);
        }
        let Some(port) = &self.fork else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        self.perform_fork(port.as_ref(), plan)
    }

    pub(crate) fn clone3(&self, address: u64, size: usize) -> LinuxResult {
        let plan = match ProcessAbi::new(&self.memory, self.architecture).clone3(address, size) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if let Err(error) = self.probe_parent_outputs(&plan) {
            return LinuxResult::Error(error);
        }
        let Some(port) = &self.fork else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        self.perform_fork(port.as_ref(), plan)
    }

    fn probe_parent_outputs(&self, plan: &ClonePlan) -> Result<(), Errno> {
        const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
        const CLONE_PIDFD: u64 = 0x0000_1000;
        let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
        for pointer in [
            (plan.flags & CLONE_PARENT_SETTID != 0).then_some(plan.parent_tid),
            (plan.flags & CLONE_PIDFD != 0).then_some(plan.pidfd),
        ]
        .into_iter()
        .flatten()
        {
            match marshaller.probe(pointer, 4, GuestAccess::Write) {
                Ok(4) => {}
                _ => return Err(Errno::EFAULT),
            }
        }
        Ok(())
    }

    fn perform_fork(&self, port: &dyn RuntimeForkPort, plan: ClonePlan) -> LinuxResult {
        match port.fork(self.process, self.thread, plan) {
            Ok(child) => {
                self.system.observe_fork();
                hl_log::hl_info!(
                    hl_log::tag::TASK,
                    "process forked parent={} child={} thread={} flags={:#x}",
                    self.process.number(),
                    child.process.number(),
                    child.thread.number(),
                    plan.flags,
                );
                LinuxResult::Value(child.process.number() as u64)
            }
            Err(error) => {
                hl_log::hl_debug!(
                    hl_log::tag::TASK,
                    "process fork refused parent={} flags={:#x} error={error:?}",
                    self.process.number(),
                    plan.flags,
                );
                Self::fork_error(error)
            }
        }
    }

    const fn fork_error(error: RuntimeForkError) -> LinuxResult {
        match error {
            RuntimeForkError::Unsupported => LinuxResult::Error(Errno::ENOSYS),
            RuntimeForkError::Invalid => LinuxResult::Error(Errno::EINVAL),
            RuntimeForkError::Again => LinuxResult::Error(Errno::EAGAIN),
            RuntimeForkError::NoMemory => LinuxResult::Error(Errno::ENOMEM),
            RuntimeForkError::Failed => LinuxResult::Error(Errno::EIO),
        }
    }
}
