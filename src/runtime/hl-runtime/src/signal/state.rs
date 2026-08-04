use hl_linux::{Errno, GuestMemory, LinuxResult, MaskOperation, SignalAbi};
use hl_task::{AlternateStack, SignalMask};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub fn queue_fault(&self, signal: u8, code: i32, address: u64) -> Result<(), ()> {
        let signal = hl_task::SignalNumber::new(signal).map_err(|_| ())?;
        self.tasks
            .enqueue_signal(
                hl_task::PendingTarget::Thread(self.thread),
                hl_task::SignalInfo {
                    code,
                    address,
                    ..hl_task::SignalInfo::bare(signal)
                },
            )
            .map(|_| ())
            .map_err(|_| ())
    }

    pub fn queue_seccomp(&self, plan: hl_linux::SeccompTrapPlan) -> Result<(), ()> {
        let signal = hl_task::SignalNumber::new(plan.signal).map_err(|_| ())?;
        self.tasks
            .enqueue_signal(
                hl_task::PendingTarget::Thread(self.thread),
                hl_task::SignalInfo {
                    code: plan.code,
                    error: i32::from(plan.error),
                    address: plan.call_address,
                    value: plan.syscall_number as u32 as u64,
                    source_tag: plan.audit_architecture,
                    ..hl_task::SignalInfo::bare(signal)
                },
            )
            .map(|_| ())
            .map_err(|_| ())
    }
    pub(crate) fn rt_sigaction(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = SignalAbi::new(&self.memory, self.architecture);
        let (signal, action) = match abi.action(arguments[0] as u32, arguments[1], arguments[3] as usize) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let old = match self.tasks.action(self.process, signal) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        if arguments[2] != 0 {
            let copyout = match abi.stage_action(arguments[2], old) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
            if let Err(error) = copyout.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        match action {
            Some(action) => match self.tasks.set_action(self.process, signal, action) {
                Ok(()) => LinuxResult::Value(0),
                Err(_) => LinuxResult::Error(Errno::EINVAL),
            },
            None => LinuxResult::Value(0),
        }
    }

    pub(crate) fn rt_sigprocmask(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = SignalAbi::new(&self.memory, self.architecture);
        let (operation, supplied) = match abi.mask(arguments[0] as u32, arguments[1], arguments[3] as usize) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let old = match self.tasks.deliver_thread_state(self.thread) {
            Ok(value) => value.mask,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        if arguments[2] != 0 {
            let copyout = match abi.stage_mask(arguments[2], old) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
            if let Err(error) = copyout.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        let Some(supplied) = supplied else {
            return LinuxResult::Value(0);
        };
        let bits = match operation {
            MaskOperation::Block => old.bits() | supplied.bits(),
            MaskOperation::Unblock => old.bits() & !supplied.bits(),
            MaskOperation::Replace => supplied.bits(),
        };
        match self.tasks.set_signal_mask(self.thread, SignalMask::from_bits(bits)) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn rt_sigpending(&self, destination: u64, size: usize) -> LinuxResult {
        let abi = SignalAbi::new(&self.memory, self.architecture);
        if let Err(error) = abi.pending(destination, size) {
            return LinuxResult::Error(error.errno());
        }
        let pending = match self.tasks.pending_signal_mask(self.thread) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
        match abi
            .stage_mask(destination, pending)
            .and_then(|copyout| copyout.commit(&marshaller))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn sigaltstack(&self, new: u64, old: u64) -> LinuxResult {
        let abi = SignalAbi::new(&self.memory, self.architecture);
        let supplied = match abi.alternate_stack(new) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let current = match self.tasks.deliver_thread_state(self.thread) {
            Ok(value) => value.alternate_stack,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        if supplied.is_some() && matches!(current, AlternateStack::Active { .. }) {
            return LinuxResult::Error(Errno::EPERM);
        }
        if old != 0 {
            let copyout = match abi.stage_alternate_stack(old, current) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
            if let Err(error) = copyout.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        match supplied {
            Some(stack) => match self.tasks.set_alternate_stack(self.thread, stack) {
                Ok(()) => LinuxResult::Value(0),
                Err(_) => LinuxResult::Error(Errno::EINVAL),
            },
            None => LinuxResult::Value(0),
        }
    }
}
