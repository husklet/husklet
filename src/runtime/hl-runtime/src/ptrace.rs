use std::sync::{Arc, Mutex};

use crate::cpu::{ExecutionCpuSnapshot, StoppedRegisterImage, StoppedRegisters, TraceSafepointPort};
use hl_linux::{
    Errno, GuestMarshaller, GuestMemory, LinuxResult, PtraceOptions, PtracePlan, PtraceRequest, PtraceResume,
};
use hl_task::{ProcessId, TaskRegistry, TraceError, TraceEvent, TraceLinkId, TracePermission, TraceResume, TraceStop};

use crate::RuntimeProcessSyscalls;

mod catalog;
pub use catalog::{Catalog as PtraceCatalog, TraceExchange};

#[cfg(test)]
mod test;

pub trait PtracePort: Send + Sync {
    fn attached(&self, link: TraceLinkId, tracee: ProcessId) -> Result<(), TraceError>;
    fn permission(&self, tracer: ProcessId, tracee: ProcessId) -> TracePermission;
    fn registers(&self, link: TraceLinkId) -> Result<StoppedRegisterImage, TraceError>;
    fn set_registers(&self, link: TraceLinkId, image: StoppedRegisterImage) -> Result<(), TraceError>;
    fn options(&self, link: TraceLinkId, options: PtraceOptions) -> Result<(), TraceError>;
    fn event_message(&self, link: TraceLinkId) -> Result<u64, TraceError>;
    fn read(&self, link: TraceLinkId, address: u64, bytes: &mut [u8]) -> Result<(), TraceError>;
    fn write(&self, link: TraceLinkId, address: u64, bytes: &[u8]) -> Result<(), TraceError>;
    fn wait_status(&self, event: TraceEvent) -> u32;
    fn resumed(&self, link: TraceLinkId);
}

pub trait TraceWake: Send + Sync {
    fn wake(&self);
}

pub struct RuntimeSafepoint {
    tasks: Arc<TaskRegistry>,
    process: ProcessId,
    exchange: Arc<dyn TraceSafepointPort>,
    pending: Mutex<Option<PendingStop>>,
}

#[derive(Clone, Copy)]
struct PendingStop {
    event: TraceEvent,
    kind: PendingKind,
}

#[derive(Clone, Copy)]
enum PendingKind {
    Entry,
    Exit,
    Signal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceBoundary {
    Continue,
    Dispatch,
    Park,
    Kill,
    Signal(Option<u32>),
}

impl RuntimeSafepoint {
    #[must_use]
    pub const fn new(tasks: Arc<TaskRegistry>, process: ProcessId, exchange: Arc<dyn TraceSafepointPort>) -> Self {
        Self {
            tasks,
            process,
            exchange,
            pending: Mutex::new(None),
        }
    }

    pub fn syscall_boundary(
        &self,
        cpu: &mut ExecutionCpuSnapshot,
        original: u64,
        exit: bool,
    ) -> Result<TraceBoundary, TraceError> {
        if self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return Ok(TraceBoundary::Park);
        }
        let event = match self.tasks.trace_syscall_stop(self.process, exit) {
            Ok(Some(event)) => event,
            Ok(None) | Err(TraceError::InvalidLink(_)) => return Ok(TraceBoundary::Continue),
            Err(error) => return Err(error),
        };
        self.publish(cpu, original)?;
        let kind = if exit { PendingKind::Exit } else { PendingKind::Entry };
        *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PendingStop { event, kind });
        Ok(TraceBoundary::Park)
    }

    pub fn signal_boundary(
        &self,
        cpu: &ExecutionCpuSnapshot,
        original: u64,
        event: TraceEvent,
    ) -> Result<TraceBoundary, TraceError> {
        if self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return Err(TraceError::AlreadyStopped(event.link));
        }
        self.publish(cpu, original)?;
        *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PendingStop {
            event,
            kind: PendingKind::Signal,
        });
        Ok(TraceBoundary::Park)
    }

    pub fn resume_boundary(&self, cpu: &mut ExecutionCpuSnapshot) -> Result<TraceBoundary, TraceError> {
        let pending = *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(pending) = pending else {
            return Ok(TraceBoundary::Continue);
        };
        let Some(command) = self.tasks.trace_take_resume(self.process, pending.event.link)? else {
            return Ok(TraceBoundary::Park);
        };
        self.apply(cpu)?;
        *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(match command {
            TraceResume::Kill => TraceBoundary::Kill,
            TraceResume::Continue(signal) | TraceResume::Syscall(signal) | TraceResume::Detach(signal)
                if matches!(pending.kind, PendingKind::Signal) =>
            {
                TraceBoundary::Signal(signal)
            }
            _ if matches!(pending.kind, PendingKind::Entry) => TraceBoundary::Dispatch,
            _ => TraceBoundary::Continue,
        })
    }

    pub fn syscall(
        &self,
        cpu: &mut ExecutionCpuSnapshot,
        original: u64,
        exit: bool,
    ) -> Result<Option<TraceResume>, TraceError> {
        let event = match self.tasks.trace_syscall_stop(self.process, exit) {
            Ok(Some(event)) => event,
            Ok(None) | Err(TraceError::InvalidLink(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        self.exchange(cpu, original, event)
    }

    pub fn stop(
        &self,
        cpu: &mut ExecutionCpuSnapshot,
        original: u64,
        stop: TraceStop,
    ) -> Result<Option<TraceResume>, TraceError> {
        let event = match self.tasks.trace_stop(self.process, stop) {
            Ok(event) => event,
            Err(TraceError::InvalidLink(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        self.exchange(cpu, original, event)
    }

    fn exchange(
        &self,
        cpu: &mut ExecutionCpuSnapshot,
        original: u64,
        event: TraceEvent,
    ) -> Result<Option<TraceResume>, TraceError> {
        self.publish(cpu, original)?;
        let command = self.tasks.trace_await_resume(self.process, event.link)?;
        self.apply(cpu)?;
        Ok(Some(command))
    }

    fn publish(&self, cpu: &ExecutionCpuSnapshot, original: u64) -> Result<(), TraceError> {
        let registers = match cpu {
            ExecutionCpuSnapshot::X86_64(cpu) => StoppedRegisters::X86(crate::cpu::X86Prstatus::capture(cpu, original)),
            ExecutionCpuSnapshot::Aarch64(cpu) => StoppedRegisters::Aarch64(crate::cpu::Aarch64Prstatus::capture(cpu)),
        };
        self.exchange
            .publish(StoppedRegisterImage::new(registers))
            .map_err(|_| TraceError::InvalidSnapshot)
    }

    fn apply(&self, cpu: &mut ExecutionCpuSnapshot) -> Result<(), TraceError> {
        let changed = self
            .exchange
            .restore()
            .map_err(|_| TraceError::InvalidSnapshot)?
            .restore()
            .map_err(|_| TraceError::InvalidSnapshot)?;
        match (cpu, changed) {
            (ExecutionCpuSnapshot::X86_64(cpu), StoppedRegisters::X86(registers)) => {
                registers.apply(cpu);
            }
            (ExecutionCpuSnapshot::Aarch64(cpu), StoppedRegisters::Aarch64(registers)) => {
                registers.apply(cpu);
            }
            _ => return Err(TraceError::InvalidSnapshot),
        }
        Ok(())
    }
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub fn ptrace(&self, arguments: [u64; 6]) -> LinuxResult {
        let Some(port) = self.ptrace.as_ref() else {
            return LinuxResult::Error(Errno::EPERM);
        };
        let PtraceRequest::Supported(plan) = PtraceRequest::decode(arguments) else {
            return LinuxResult::Error(Errno::EIO);
        };
        self.execute_ptrace(plan, port.as_ref())
    }

    fn execute_ptrace(&self, plan: PtracePlan, port: &dyn PtracePort) -> LinuxResult {
        if matches!(
            plan,
            PtracePlan::GetRegisterSet { note, .. }
                | PtracePlan::SetRegisterSet { note, .. }
                if note != hl_linux::NT_PRSTATUS
        ) {
            return LinuxResult::Error(Errno::EIO);
        }
        match self.execute_trace_plan(plan, port) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(Self::trace_errno(error)),
        }
    }

    fn execute_trace_plan(&self, plan: PtracePlan, port: &dyn PtracePort) -> Result<(), TraceError> {
        match plan {
            PtracePlan::TraceMe => {
                let link = self.tasks.trace_me(self.process)?;
                port.attached(link, self.process)
            }
            PtracePlan::Attach { process } => {
                let tracee = self.tasks.process_number(process)?;
                let link = self
                    .tasks
                    .trace_attach(self.process, tracee, port.permission(self.process, tracee))?;
                port.attached(link, tracee)
            }
            PtracePlan::Seize { process, options } => {
                let tracee = self.tasks.process_number(process)?;
                let link = self
                    .tasks
                    .trace_seize(self.process, tracee, port.permission(self.process, tracee))?;
                port.attached(link, tracee)?;
                port.options(link, options)
            }
            PtracePlan::Detach { process, signal } => {
                self.resume(process, TraceResume::Detach(Self::injection(signal)))
            }
            PtracePlan::Resume { process, signal, mode } => {
                let signal = Self::injection(signal);
                let command = match mode {
                    PtraceResume::Continue => TraceResume::Continue(signal),
                    PtraceResume::Syscall => TraceResume::Syscall(signal),
                };
                self.resume(process, command)
            }
            PtracePlan::Kill { process } => self.resume(process, TraceResume::Kill),
            PtracePlan::SetOptions { process, options } => {
                let link = self.link(process, false)?;
                port.options(link, options)
            }
            PtracePlan::GetEventMessage { process, destination } => {
                let link = self.link(process, false)?;
                let message = port.event_message(link)?;
                self.copy_out(destination, &message.to_le_bytes())
            }
            PtracePlan::GetRegisters { process, destination } => {
                let link = self.link(process, true)?;
                let image = port.registers(link)?;
                let bytes = Self::register_bytes(image)?;
                self.copy_out(destination, &bytes)
            }
            PtracePlan::SetRegisters { process, source } => {
                let link = self.link(process, true)?;
                let image = self.copy_registers(source)?;
                port.set_registers(link, image)
            }
            PtracePlan::GetRegisterSet { process, iovec, .. } => {
                let link = self.link(process, true)?;
                let (destination, capacity) = self.copy_iovec(iovec)?;
                let image = port.registers(link)?;
                let bytes = Self::register_bytes(image)?;
                self.copy_out(destination, &bytes[..capacity.min(bytes.len())])?;
                self.copy_out(iovec + 8, &(bytes.len() as u64).to_le_bytes())
            }
            PtracePlan::SetRegisterSet { process, iovec, .. } => {
                let link = self.link(process, true)?;
                let (source, capacity) = self.copy_iovec(iovec)?;
                let current = port.registers(link)?;
                let mut bytes = Self::register_bytes(current)?;
                let count = capacity.min(bytes.len());
                self.copy_in(source, &mut bytes[..count])?;
                port.set_registers(link, self.decode_registers(&bytes)?)
            }
            PtracePlan::PeekUser {
                process,
                offset,
                destination,
            } => {
                let link = self.link(process, false)?;
                let image = port.registers(link)?;
                let bytes = Self::register_bytes(image)?;
                let mut word = [0; 8];
                if offset & 7 == 0 && offset <= bytes.len().saturating_sub(8) as u64 {
                    word.copy_from_slice(&bytes[offset as usize..offset as usize + 8]);
                }
                self.copy_out(destination, &word)
            }
            PtracePlan::PokeUser { process, offset, word } => {
                let link = self.link(process, false)?;
                let current = port.registers(link)?;
                let mut bytes = Self::register_bytes(current)?;
                if offset & 7 == 0 && offset <= bytes.len().saturating_sub(8) as u64 {
                    bytes[offset as usize..offset as usize + 8].copy_from_slice(&word.to_le_bytes());
                    port.set_registers(link, self.decode_registers(&bytes)?)?;
                }
                Ok(())
            }
            PtracePlan::PeekData {
                process,
                address,
                destination,
            } => {
                let link = self.link(process, true)?;
                let mut word = [0; 8];
                port.read(link, address, &mut word)?;
                self.copy_out(destination, &word)
            }
            PtracePlan::PokeData { process, address, word } => {
                let link = self.link(process, true)?;
                port.write(link, address, &word.to_le_bytes())
            }
        }
    }

    fn link(&self, process: u32, require_stop: bool) -> Result<TraceLinkId, TraceError> {
        let tracee = self.tasks.process_number(process)?;
        self.tasks.trace_link(self.process, tracee, require_stop)
    }

    fn resume(&self, process: u32, command: TraceResume) -> Result<(), TraceError> {
        let link = self.link(process, true)?;
        self.tasks.trace_resume(self.process, link, command)?;
        if let Some(port) = &self.ptrace {
            port.resumed(link);
        }
        Ok(())
    }

    fn copy_out(&self, destination: u64, bytes: &[u8]) -> Result<(), TraceError> {
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if marshaller.copy_to(destination, bytes).fault.is_some() {
            Err(TraceError::InvalidSnapshot)
        } else {
            Ok(())
        }
    }

    fn copy_registers(&self, source: u64) -> Result<StoppedRegisterImage, TraceError> {
        let length = match self.architecture {
            hl_linux::GuestArchitecture::X86_64 => crate::cpu::X86Prstatus::BYTES,
            hl_linux::GuestArchitecture::Aarch64 => crate::cpu::Aarch64Prstatus::BYTES,
        };
        let mut bytes = vec![0; length];
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if marshaller.copy_from(source, &mut bytes).fault.is_some() {
            return Err(TraceError::InvalidSnapshot);
        }
        self.decode_registers(&bytes)
    }

    fn decode_registers(&self, bytes: &[u8]) -> Result<StoppedRegisterImage, TraceError> {
        let registers = match self.architecture {
            hl_linux::GuestArchitecture::X86_64 => crate::cpu::StoppedRegisters::X86(
                crate::cpu::X86Prstatus::decode(bytes).map_err(|_| TraceError::InvalidSnapshot)?,
            ),
            hl_linux::GuestArchitecture::Aarch64 => crate::cpu::StoppedRegisters::Aarch64(
                crate::cpu::Aarch64Prstatus::decode(bytes).map_err(|_| TraceError::InvalidSnapshot)?,
            ),
        };
        Ok(StoppedRegisterImage::new(registers))
    }

    fn register_bytes(image: StoppedRegisterImage) -> Result<Vec<u8>, TraceError> {
        match image.restore().map_err(|_| TraceError::InvalidSnapshot)? {
            crate::cpu::StoppedRegisters::X86(value) => Ok(value.encode()),
            crate::cpu::StoppedRegisters::Aarch64(value) => Ok(value.encode()),
        }
    }

    fn copy_iovec(&self, address: u64) -> Result<(u64, usize), TraceError> {
        let mut bytes = [0; 16];
        self.copy_in(address, &mut bytes)?;
        let base = u64::from_le_bytes(bytes[..8].try_into().map_err(|_| TraceError::InvalidSnapshot)?);
        let length = u64::from_le_bytes(bytes[8..].try_into().map_err(|_| TraceError::InvalidSnapshot)?);
        Ok((base, usize::try_from(length).unwrap_or(usize::MAX)))
    }

    fn copy_in(&self, source: u64, bytes: &mut [u8]) -> Result<(), TraceError> {
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if marshaller.copy_from(source, bytes).fault.is_some() {
            Err(TraceError::InvalidSnapshot)
        } else {
            Ok(())
        }
    }

    const fn injection(signal: u32) -> Option<u32> {
        if signal == 0 { None } else { Some(signal) }
    }

    const fn trace_errno(error: TraceError) -> Errno {
        match error {
            TraceError::Capacity => Errno::ENOMEM,
            TraceError::Denied(_) | TraceError::AlreadyTraced(_) | TraceError::WrongTracer { .. } => Errno::EPERM,
            TraceError::InvalidSnapshot => Errno::EFAULT,
            TraceError::InvalidLink(_)
            | TraceError::InvalidProcess(_)
            | TraceError::NotStopped(_)
            | TraceError::AlreadyStopped(_) => Errno::ESRCH,
        }
    }
}
