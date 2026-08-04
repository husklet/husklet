use hl_isa::GuestArchitecture;

use super::frame::{Frame, FrameDecoder};
use crate::{Decision, Errno, FrameError, KillScope, LinuxResult, RegisterView, TrapPlan};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Family {
    Aio,
    Filesystem,
    DescriptorIo,
    Event,
    Memory,
    Network,
    TaskSignalTime,
    Ipc,
    Seccomp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Operation {
    pub canonical_number: u16,
    pub name: &'static str,
    pub family: Family,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Operation(Operation),
    Unsupported { canonical_number: u16, name: &'static str },
    Reserved { canonical_number: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NumberTranslation {
    pub raw_number: u16,
    pub canonical_number: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Route {
    pub translation: NumberTranslation,
    pub disposition: Disposition,
}

macro_rules! family_port {
    ($name:ident) => {
        pub trait $name {
            fn handle(&mut self, operation: Operation, arguments: [u64; 6]) -> LinuxResult;
        }
    };
}

pub trait DescriptorIoSyscalls {
    fn handle(&mut self, operation: Operation, arguments: [u64; 6]) -> LinuxResult;

    /// Reports whether an operation can wait after inspecting its live OFD.
    /// The conservative default keeps unknown adapters on a waiter lane.
    fn may_block(&self, _operation: Operation, _arguments: [u64; 6]) -> bool {
        true
    }
}

pub trait FilesystemSyscalls {
    fn handle(&mut self, operation: Operation, arguments: [u64; 6]) -> LinuxResult;

    /// Reports whether executing this operation can wait for another guest
    /// task. Callers use this only to choose the execution lane. The default
    /// is conservative so a forwarding adapter that omits this method cannot
    /// accidentally pin the scheduler; concrete ports should classify their
    /// synchronous operations precisely.
    fn may_block(&self, _operation: Operation, _arguments: [u64; 6]) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Family, FilesystemSyscalls, Operation};
    use crate::{Errno, LinuxResult};

    struct HandleOnly;

    impl FilesystemSyscalls for HandleOnly {
        fn handle(&mut self, _operation: Operation, _arguments: [u64; 6]) -> LinuxResult {
            LinuxResult::Error(Errno::ENOSYS)
        }
    }

    #[test]
    fn filesystem_adapter_omission_is_conservatively_blocking() {
        let adapter = HandleOnly;
        let operation = Operation {
            canonical_number: 56,
            name: "openat",
            family: Family::Filesystem,
        };
        assert!(adapter.may_block(operation, [0; 6]));
    }
}
family_port!(AioSyscalls);
family_port!(EventSyscalls);
family_port!(MemorySyscalls);
family_port!(NetworkSyscalls);
family_port!(TaskSignalTimeSyscalls);
family_port!(IpcSyscalls);
pub trait SeccompSyscalls {
    fn handle(&mut self, operation: Operation, arguments: [u64; 6]) -> LinuxResult;

    fn evaluate(&self, _frame: &Frame, _instruction_pointer: u64) -> Decision {
        Decision::Continue
    }
}

pub struct Ports<'a> {
    pub aio: &'a mut dyn AioSyscalls,
    pub filesystem: &'a mut dyn FilesystemSyscalls,
    pub descriptor_io: &'a mut dyn DescriptorIoSyscalls,
    pub event: &'a mut dyn EventSyscalls,
    pub memory: &'a mut dyn MemorySyscalls,
    pub network: &'a mut dyn NetworkSyscalls,
    pub task_signal_time: &'a mut dyn TaskSignalTimeSyscalls,
    pub ipc: &'a mut dyn IpcSyscalls,
    pub seccomp: &'a mut dyn SeccompSyscalls,
}

pub struct Dispatcher;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeccompDispatch {
    Result(LinuxResult),
    Trap(TrapPlan),
    Kill { scope: KillScope, signal: u8 },
    Trace { data: u16 },
    UserNotification { data: u16 },
}

impl Dispatcher {
    #[must_use]
    pub fn route(architecture: GuestArchitecture, raw_number: u64) -> Route {
        super::table::Table::route(architecture, raw_number)
    }

    pub fn dispatch(frame: Frame, ports: &mut Ports<'_>) -> LinuxResult {
        let route = Self::route(frame.architecture, frame.raw_number);
        let Disposition::Operation(operation) = route.disposition else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        match operation.family {
            Family::Aio => ports.aio.handle(operation, frame.arguments),
            Family::Filesystem => ports.filesystem.handle(operation, frame.arguments),
            Family::DescriptorIo => ports.descriptor_io.handle(operation, frame.arguments),
            Family::Event => ports.event.handle(operation, frame.arguments),
            Family::Memory => ports.memory.handle(operation, frame.arguments),
            Family::Network => ports.network.handle(operation, frame.arguments),
            Family::TaskSignalTime => ports.task_signal_time.handle(operation, frame.arguments),
            Family::Ipc => ports.ipc.handle(operation, frame.arguments),
            Family::Seccomp => ports.seccomp.handle(operation, frame.arguments),
        }
    }

    pub fn dispatch_seccomp(frame: Frame, decision: Decision, ports: &mut Ports<'_>) -> SeccompDispatch {
        match decision {
            Decision::Continue => SeccompDispatch::Result(Self::dispatch(frame, ports)),
            Decision::ReturnErrno(errno) => {
                SeccompDispatch::Result(LinuxResult::Error(Errno::from_raw(i32::from(errno))))
            }
            Decision::Trap(plan) => SeccompDispatch::Trap(plan),
            Decision::Kill { scope, signal } => SeccompDispatch::Kill { scope, signal },
            Decision::Trace { data } => SeccompDispatch::Trace { data },
            Decision::UserNotification { data } => SeccompDispatch::UserNotification { data },
        }
    }

    pub fn execute(
        architecture: GuestArchitecture,
        registers: &mut dyn RegisterView,
        ports: &mut Ports<'_>,
    ) -> Result<LinuxResult, FrameError> {
        let frame = FrameDecoder::decode(architecture, registers)?;
        let result = Self::dispatch(frame, ports);
        FrameDecoder::write_result(&frame, registers, result)?;
        Ok(result)
    }
}
