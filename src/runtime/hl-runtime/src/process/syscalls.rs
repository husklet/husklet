use std::sync::{Arc, Mutex};

use crate::{ExecQueue, RuntimeExecPort, RuntimeForkPort};
use hl_descriptor::DescriptorTable;
use hl_linux::{GuestArchitecture, GuestMemory};
use hl_task::{ProcessId, TaskRegistry, ThreadId};

pub trait RuntimeReapPort: Send + Sync {
    fn remove(&self, process: ProcessId);
}

pub struct RuntimeProcessSyscalls<M: GuestMemory> {
    pub(crate) tasks: Arc<TaskRegistry>,
    pub(crate) process: ProcessId,
    pub(crate) thread: ThreadId,
    pub(crate) memory: M,
    pub(crate) architecture: GuestArchitecture,
    pub(crate) fs_context: Arc<crate::FsContext>,
    pub(crate) fork: Option<Arc<dyn RuntimeForkPort>>,
    pub(crate) exec: Option<Arc<dyn RuntimeExecPort>>,
    pub(crate) exec_queue: Option<Arc<ExecQueue>>,
    pub(crate) clock: Option<Arc<dyn hl_time::Clock>>,
    pub(crate) cpu_clock: Option<Arc<dyn crate::CpuClockPort>>,
    pub(crate) sleep: Option<Arc<dyn crate::RuntimeSleepPort>>,
    pub(crate) blocking_wait: Option<Arc<dyn crate::BlockingWait>>,
    pub(crate) futex: Option<Arc<dyn crate::RuntimeFutexPort>>,
    pub(crate) robust_exit: Option<Arc<dyn crate::RobustExitPort>>,
    pub(crate) exit_runtime: Option<Arc<crate::ExitRuntime>>,
    pub(crate) signal_frames: Option<Arc<dyn crate::FramePort>>,
    pub(crate) scheduler: Option<Arc<dyn crate::RuntimeYieldPort>>,
    pub(crate) descriptors: Option<Arc<DescriptorTable>>,
    pub(crate) handles: Option<Arc<crate::ProcessHandleRegistry>>,
    pub(crate) namespace_handles: Option<Arc<crate::NamespaceHandleRegistry>>,
    pub(crate) alarms: Option<Arc<crate::AlarmRegistry>>,
    pub(crate) timers: Option<Arc<crate::TimerRegistry>>,
    pub(crate) seccomp: Option<Arc<dyn crate::SeccompPrctlPort>>,
    pub(crate) ptrace: Option<Arc<dyn crate::PtracePort>>,
    pub(crate) reap: Option<Arc<dyn RuntimeReapPort>>,
    pub(crate) system: Arc<crate::SystemAuthority>,
    pub(crate) trace_signal_pass: Mutex<Option<u32>>,
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn blocking_interruption(&self) -> Arc<hl_sync::Interruption> {
        self.blocking_wait
            .as_ref()
            .map_or_else(|| Arc::new(hl_sync::Interruption::new()), |wait| wait.interruption())
    }

    #[must_use]
    pub fn new(
        tasks: Arc<TaskRegistry>,
        process: ProcessId,
        thread: ThreadId,
        memory: M,
        architecture: GuestArchitecture,
    ) -> Self {
        Self {
            tasks,
            process,
            thread,
            memory,
            architecture,
            fs_context: Arc::new(crate::FsContext::default()),
            fork: None,
            exec: None,
            exec_queue: None,
            clock: None,
            cpu_clock: None,
            sleep: None,
            blocking_wait: None,
            futex: None,
            robust_exit: None,
            exit_runtime: None,
            signal_frames: None,
            scheduler: None,
            descriptors: None,
            handles: None,
            namespace_handles: None,
            alarms: None,
            timers: None,
            seccomp: None,
            ptrace: None,
            reap: None,
            system: Arc::new(crate::SystemAuthority::default()),
            trace_signal_pass: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn with_system(mut self, system: Arc<crate::SystemAuthority>) -> Self {
        self.system = system;
        self
    }

    #[must_use]
    pub fn with_fs_context(mut self, context: Arc<crate::FsContext>) -> Self {
        self.fs_context = context;
        self
    }

    #[must_use]
    pub fn with_ptrace(mut self, port: Arc<dyn crate::PtracePort>) -> Self {
        self.ptrace = Some(port);
        self
    }

    #[must_use]
    pub fn with_reap_port(mut self, port: Arc<dyn RuntimeReapPort>) -> Self {
        self.reap = Some(port);
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn hl_time::Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    #[must_use]
    pub fn with_cpu_clock(mut self, clock: Arc<dyn crate::CpuClockPort>) -> Self {
        self.cpu_clock = Some(clock);
        self
    }

    #[must_use]
    pub fn with_process_handles(
        mut self,
        descriptors: Arc<DescriptorTable>,
        handles: Arc<crate::ProcessHandleRegistry>,
    ) -> Self {
        handles.register_files(self.process, &descriptors);
        self.descriptors = Some(descriptors);
        self.handles = Some(handles);
        self
    }

    #[must_use]
    pub fn with_sleep_port(mut self, sleep: Arc<dyn crate::RuntimeSleepPort>) -> Self {
        self.sleep = Some(sleep);
        self
    }

    #[must_use]
    pub fn with_blocking_wait(mut self, wait: Arc<dyn crate::BlockingWait>) -> Self {
        self.blocking_wait = Some(wait);
        self
    }

    #[must_use]
    pub fn with_futex_port(mut self, futex: Arc<dyn crate::RuntimeFutexPort>) -> Self {
        self.futex = Some(futex);
        self
    }

    #[must_use]
    pub fn with_robust_exit(mut self, cleanup: Arc<dyn crate::RobustExitPort>) -> Self {
        self.robust_exit = Some(cleanup);
        self
    }

    #[must_use]
    pub fn with_exit_runtime(mut self, runtime: Arc<crate::ExitRuntime>) -> Self {
        self.exit_runtime = Some(runtime);
        self
    }

    #[must_use]
    pub fn with_signal_frame(mut self, port: Arc<dyn crate::FramePort>) -> Self {
        self.signal_frames = Some(port);
        self
    }

    #[must_use]
    pub fn with_fork_port(mut self, fork: Arc<dyn RuntimeForkPort>) -> Self {
        self.fork = Some(fork);
        self
    }

    pub fn with_assembly_fork(mut self, assembly: &crate::RuntimeAssembly) -> Self {
        self.fork = assembly.fork();
        self
    }

    #[must_use]
    pub fn with_exec_port(mut self, exec: Arc<dyn RuntimeExecPort>) -> Self {
        self.exec = Some(exec);
        self
    }

    #[must_use]
    pub fn with_exec_queue(mut self, queue: Arc<ExecQueue>) -> Self {
        self.exec_queue = Some(queue);
        self
    }

    #[must_use]
    pub fn with_yield_port(mut self, scheduler: Arc<dyn crate::RuntimeYieldPort>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    #[must_use]
    pub fn with_seccomp(mut self, seccomp: Arc<dyn crate::SeccompPrctlPort>) -> Self {
        self.seccomp = Some(seccomp);
        self
    }

    pub fn with_assembly_exec(mut self, assembly: &crate::RuntimeAssembly) -> Self {
        self.exec = assembly.exec();
        self
    }
}

#[cfg(test)]
#[path = "../signal/frame_test.rs"]
mod signal_frame_tests;
#[cfg(test)]
#[path = "syscalls_test.rs"]
mod tests;
