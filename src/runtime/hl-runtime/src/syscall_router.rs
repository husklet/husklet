use std::sync::{Arc, Mutex};

use hl_execution::ExecutionCpuSnapshot;
use hl_isa::GuestArchitecture;
use hl_linux::{
    AioSyscalls, DescriptorIoSyscalls, EventSyscalls, FilesystemSyscalls, GuestMemory, IpcSyscalls, MemorySyscalls,
    NetworkSyscalls, SeccompSyscalls, SyscallDispatcher, SyscallDisposition, SyscallFrameDecoder,
    TaskSignalTimeSyscalls,
};

#[path = "syscall_dispatch.rs"]
mod dispatch;

use dispatch::CpuRegisters;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalBoundaryOutcome {
    None,
    Handled,
    Stop { control_epoch: u64 },
    Continue,
    Terminate { signal: u8, dumped_core: bool },
    Trace { event: hl_task::TraceEvent, signal: u8 },
}

pub trait SignalBoundaryPort: Send {
    fn deliver(&mut self) -> Result<SignalBoundaryOutcome, ()>;
    fn restore(&mut self) -> Result<(), ()>;
    fn resolve_trace(&mut self, _signal: Option<u32>) -> Result<(), ()> {
        Err(())
    }
    fn queue(&mut self, _signal: u8, _code: i32, _address: u64) -> Result<(), ()> {
        Err(())
    }
    fn terminate(&mut self, _signal: u8, _dumped_core: bool) -> Result<(), ()> {
        Err(())
    }
    fn kill(&mut self, _scope: hl_linux::SeccompKillScope, _signal: u8) -> Result<(), ()> {
        Err(())
    }
    fn seccomp(&mut self, plan: hl_linux::SeccompTrapPlan) -> Result<(), ()> {
        self.queue(plan.signal, plan.code, plan.call_address)
    }
}

pub struct RouterDependencies {
    pub aio: Box<dyn AioSyscalls + Send>,
    pub architecture_memory: Box<dyn GuestMemory + Send>,
    pub process_fork: Option<Box<dyn crate::ProcessForkTrap>>,
    pub thread_clone: Option<Box<dyn crate::ThreadCloneTrapPort>>,
    pub filesystem: Box<dyn FilesystemSyscalls + Send>,
    pub descriptor_io: Box<dyn DescriptorIoSyscalls + Send>,
    pub event: Box<dyn EventSyscalls + Send>,
    pub memory: Box<dyn MemorySyscalls + Send>,
    pub network: Box<dyn NetworkSyscalls + Send>,
    pub task_signal_time: Box<dyn TaskSignalTimeSyscalls + Send>,
    pub ipc: Box<dyn IpcSyscalls + Send>,
    pub seccomp: Box<dyn SeccompSyscalls + Send>,
}

pub struct RuntimeSyscallRouter {
    ports: Mutex<RouterDependencies>,
    task_identity: Option<(hl_task::ProcessId, hl_task::ThreadId)>,
    trace: Option<Mutex<SyscallTrace>>,
    terminal: Mutex<Option<RuntimeTerminal>>,
    signal_boundary: Option<Mutex<Box<dyn SignalBoundaryPort>>>,
    exec: Option<(hl_task::ThreadId, Arc<crate::ExecQueue>)>,
    ptrace: Option<Arc<crate::RuntimeSafepoint>>,
    seccomp_control: Option<Arc<crate::SeccompControl>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTerminal {
    Thread(i32),
    Group(i32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyscallRecord {
    pub architecture: GuestArchitecture,
    pub number: u64,
    pub name: &'static str,
    pub arguments: [u64; 6],
    pub result: u64,
    pub pc: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyscallTrace {
    capacity: usize,
    next: usize,
    wrapped: bool,
    records: Vec<SyscallRecord>,
}

impl RuntimeSyscallRouter {
    #[must_use]
    pub fn new(dependencies: RouterDependencies) -> Self {
        Self {
            ports: Mutex::new(dependencies),
            task_identity: None,
            trace: None,
            terminal: Mutex::new(None),
            signal_boundary: None,
            exec: None,
            ptrace: None,
            seccomp_control: None,
        }
    }

    /// Supplies the policy owner used to prove that identity-only syscalls
    /// cannot yet be observed by seccomp.
    #[must_use]
    pub fn with_seccomp_control(mut self, control: Arc<crate::SeccompControl>) -> Self {
        self.seccomp_control = Some(control);
        self
    }

    /// Publishes the immutable Linux task identity owned by this per-thread
    /// router. Identity-only syscalls can then avoid entering the mutable
    /// process port after seccomp has admitted the call.
    #[must_use]
    pub fn with_task_identity(mut self, process: hl_task::ProcessId, thread: hl_task::ThreadId) -> Self {
        self.task_identity = Some((process, thread));
        self
    }

    fn task_identity_result(&self, disposition: SyscallDisposition) -> Option<hl_linux::LinuxResult> {
        let (process, thread) = self.task_identity?;
        let SyscallDisposition::Operation(operation) = disposition else {
            return None;
        };
        match operation.name {
            "getpid" => Some(hl_linux::LinuxResult::Value(u64::from(process.number()))),
            "gettid" => Some(hl_linux::LinuxResult::Value(u64::from(thread.number()))),
            _ => None,
        }
    }

    #[must_use]
    pub fn with_ptrace(mut self, ptrace: Arc<crate::RuntimeSafepoint>) -> Self {
        self.ptrace = Some(ptrace);
        self
    }

    #[must_use]
    pub fn ptrace(&self) -> Option<Arc<crate::RuntimeSafepoint>> {
        self.ptrace.clone()
    }

    #[must_use]
    pub fn with_exec_queue(mut self, thread: hl_task::ThreadId, queue: Arc<crate::ExecQueue>) -> Self {
        self.exec = Some((thread, queue));
        self
    }

    pub fn take_exec(&self, generation: u64) -> Option<Box<dyn crate::PreparedExec>> {
        let (thread, queue) = self.exec.as_ref()?;
        queue.take(crate::ExecKey {
            thread: *thread,
            generation,
        })
    }

    #[must_use]
    pub fn with_signal_boundary(mut self, port: Box<dyn SignalBoundaryPort>) -> Self {
        self.signal_boundary = Some(Mutex::new(port));
        self
    }

    pub fn deliver_signal(&self) -> Result<SignalBoundaryOutcome, ()> {
        match &self.signal_boundary {
            Some(port) => port.lock().map_err(|_| ())?.deliver(),
            None => Ok(SignalBoundaryOutcome::None),
        }
    }

    pub fn resolve_trace_signal(&self, signal: Option<u32>) -> Result<(), ()> {
        self.signal_boundary
            .as_ref()
            .ok_or(())?
            .lock()
            .map_err(|_| ())?
            .resolve_trace(signal)
    }

    pub fn restore_signal(&self) -> Result<(), ()> {
        self.signal_boundary
            .as_ref()
            .ok_or(())?
            .lock()
            .map_err(|_| ())?
            .restore()
    }

    pub fn queue_signal(&self, signal: u8, code: i32, address: u64) -> Result<(), ()> {
        self.signal_boundary
            .as_ref()
            .ok_or(())?
            .lock()
            .map_err(|_| ())?
            .queue(signal, code, address)
    }

    pub fn terminate_signal(&self, signal: u8, dumped_core: bool) -> Result<(), ()> {
        self.signal_boundary
            .as_ref()
            .ok_or(())?
            .lock()
            .map_err(|_| ())?
            .terminate(signal, dumped_core)
    }

    pub fn kill_seccomp(&self, scope: hl_linux::SeccompKillScope, signal: u8) -> Result<(), ()> {
        self.signal_boundary
            .as_ref()
            .ok_or(())?
            .lock()
            .map_err(|_| ())?
            .kill(scope, signal)
    }

    pub fn queue_seccomp(&self, plan: hl_linux::SeccompTrapPlan) -> Result<(), ()> {
        self.signal_boundary
            .as_ref()
            .ok_or(())?
            .lock()
            .map_err(|_| ())?
            .seccomp(plan)
    }

    #[must_use]
    pub fn with_trace(mut self, capacity: usize) -> Self {
        let capacity = capacity.clamp(1, 32);
        self.trace = Some(Mutex::new(SyscallTrace {
            capacity,
            next: 0,
            wrapped: false,
            records: Vec::with_capacity(capacity),
        }));
        self
    }

    #[cfg(test)]
    pub(crate) fn with_clone(mut self, port: Box<dyn crate::ThreadCloneTrapPort>) -> Self {
        self.ports
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .thread_clone = Some(port);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_fork(mut self, port: Box<dyn crate::ProcessForkTrap>) -> Self {
        self.ports
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .process_fork = Some(port);
        self
    }

    #[must_use]
    pub fn trace(&self) -> Option<Vec<SyscallRecord>> {
        self.trace.as_ref().map(|trace| {
            trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ordered()
        })
    }

    pub fn take_terminal(&self) -> Option<RuntimeTerminal> {
        self.terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[must_use]
    pub fn filesystem_may_block(&self, architecture: GuestArchitecture, cpu: &mut ExecutionCpuSnapshot) -> bool {
        let Ok(frame) = SyscallFrameDecoder::decode(architecture, &CpuRegisters(cpu)) else {
            return false;
        };
        let SyscallDisposition::Operation(operation) =
            SyscallDispatcher::route(architecture, frame.raw_number).disposition
        else {
            return false;
        };
        if operation.family != hl_linux::SyscallFamily::Filesystem {
            return false;
        }
        self.ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .filesystem
            .may_block(operation, frame.arguments)
    }

    #[must_use]
    pub fn descriptor_may_block(&self, architecture: GuestArchitecture, cpu: &mut ExecutionCpuSnapshot) -> bool {
        let Ok(frame) = SyscallFrameDecoder::decode(architecture, &CpuRegisters(cpu)) else {
            return true;
        };
        let SyscallDisposition::Operation(operation) =
            SyscallDispatcher::route(architecture, frame.raw_number).disposition
        else {
            return true;
        };
        if operation.family != hl_linux::SyscallFamily::DescriptorIo {
            return true;
        }
        self.ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .descriptor_io
            .may_block(operation, frame.arguments)
    }

    fn clone_result(
        dependencies: &RouterDependencies,
        cpu: &ExecutionCpuSnapshot,
        frame: hl_linux::SyscallFrame,
        clone3: bool,
    ) -> hl_linux::LinuxResult {
        let abi = hl_linux::ProcessAbi::new(dependencies.architecture_memory.as_ref(), frame.architecture);
        let plan = if clone3 {
            abi.clone3(frame.arguments[0], frame.arguments[1] as usize)
        } else {
            abi.clone_legacy(
                frame.arguments[0],
                frame.arguments[1],
                frame.arguments[2],
                frame.arguments[3],
                frame.arguments[4],
            )
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => return hl_linux::LinuxResult::Error(error.errno()),
        };
        if plan.flags & 0x0001_0000 != 0 {
            let Some(port) = dependencies.thread_clone.as_ref() else {
                return hl_linux::LinuxResult::Error(hl_linux::Errno::ENOSYS);
            };
            crate::ThreadCloneTrapPort::clone(port.as_ref(), cpu, plan)
        } else {
            let Some(port) = dependencies.process_fork.as_ref() else {
                return hl_linux::LinuxResult::Error(hl_linux::Errno::ENOSYS);
            };
            crate::ProcessForkTrap::fork(port.as_ref(), cpu, plan)
        }
    }

    fn fork_result(
        dependencies: &RouterDependencies,
        cpu: &ExecutionCpuSnapshot,
        architecture: GuestArchitecture,
        vfork: bool,
    ) -> hl_linux::LinuxResult {
        let Some(port) = dependencies.process_fork.as_ref() else {
            return hl_linux::LinuxResult::Error(hl_linux::Errno::ENOSYS);
        };
        let abi = hl_linux::ProcessAbi::new(dependencies.architecture_memory.as_ref(), architecture);
        let plan = if vfork { abi.vfork() } else { abi.fork() };
        crate::ProcessForkTrap::fork(port.as_ref(), cpu, plan)
    }

    fn trace_identity(
        architecture: GuestArchitecture,
        number: u64,
        disposition: SyscallDisposition,
    ) -> (&'static str, u64) {
        if architecture == GuestArchitecture::X86_64 && number == 158 {
            return ("arch_prctl", number);
        }
        match disposition {
            SyscallDisposition::Operation(operation) => (operation.name, operation.canonical_number as u64),
            _ => ("unsupported", number),
        }
    }

    fn seccomp_result(
        &self,
        decision: hl_linux::SeccompDecision,
    ) -> Result<Option<(hl_linux::LinuxResult, Option<u8>)>, ()> {
        let value = match decision {
            hl_linux::SeccompDecision::Continue => return Ok(None),
            hl_linux::SeccompDecision::ReturnErrno(errno) => (
                hl_linux::LinuxResult::Error(hl_linux::Errno::from_raw(i32::from(errno))),
                None,
            ),
            hl_linux::SeccompDecision::Trace { .. } | hl_linux::SeccompDecision::UserNotification { .. } => {
                (hl_linux::LinuxResult::Error(hl_linux::Errno::ENOSYS), None)
            }
            hl_linux::SeccompDecision::Trap(plan) => {
                self.queue_seccomp(plan)?;
                (hl_linux::LinuxResult::Error(hl_linux::Errno::ENOSYS), None)
            }
            hl_linux::SeccompDecision::Kill { scope, signal } => {
                self.kill_seccomp(scope, signal)?;
                let status = 128 + i32::from(signal);
                let terminal = match scope {
                    hl_linux::SeccompKillScope::Thread => RuntimeTerminal::Thread(status),
                    hl_linux::SeccompKillScope::Process => RuntimeTerminal::Group(status),
                };
                *self.terminal.lock().map_err(|_| ())? = Some(terminal);
                (hl_linux::LinuxResult::Error(hl_linux::Errno::EPERM), Some(signal))
            }
        };
        Ok(Some(value))
    }
}

impl SyscallTrace {
    fn push(&mut self, record: SyscallRecord) {
        if self.records.len() < self.capacity {
            self.records.push(record);
            self.next = self.records.len() % self.capacity;
            return;
        }
        self.records[self.next] = record;
        self.next = (self.next + 1) % self.capacity;
        self.wrapped = true;
    }

    fn ordered(&self) -> Vec<SyscallRecord> {
        if !self.wrapped {
            return self.records.clone();
        }
        self.records[self.next..]
            .iter()
            .chain(&self.records[..self.next])
            .cloned()
            .collect()
    }
}
