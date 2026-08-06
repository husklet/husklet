use hl_isa::GuestArchitecture;
use hl_linux::{
    AARCH64_SIGNAL_FRAME_SIZE, Errno, LinuxResult, SignalFrameCodec, SignalFrameError, SignalFrameImage,
    SignalFrameRequest, SignalMachine, X86_SIGNAL_FRAME_SIZE,
};
use hl_task::{SignalDisposition, ThreadId};

use crate::RuntimeProcessSyscalls;

const SA_RESETHAND: u64 = 0x8000_0000;
const SA_RESTORER: u64 = 0x0400_0000;

/// Prepared guest-memory and CPU publication. Preparation stages frame bytes;
/// publication may still fail while replacing the frozen machine. Dropping an
/// uncommitted value restores both the staged bytes and any published machine.
pub trait PreparedFramePublication: Send {
    /// Publishes the machine image while retaining enough state to roll it
    /// back if the task-side signal transaction cannot commit.
    fn publish(&mut self) -> Result<(), ()>;

    /// Finalizes task and machine publication and releases rollback state.
    fn commit(self: Box<Self>);
}

/// Execution-owned transactional boundary used by signal delivery and return.
pub trait FramePort: Send + Sync {
    fn snapshot(&self, thread: ThreadId) -> Result<SignalMachine, ()>;

    fn default_sigreturn_pc(&self, architecture: GuestArchitecture) -> Option<u64>;

    fn prepare_install(
        &self,
        thread: ThreadId,
        image: &SignalFrameImage,
    ) -> Result<Box<dyn PreparedFramePublication>, ()>;

    fn read_frame(&self, thread: ThreadId, address: u64, length: usize) -> Result<Vec<u8>, ()>;

    fn prepare_restore(
        &self,
        thread: ThreadId,
        machine: &SignalMachine,
    ) -> Result<Box<dyn PreparedFramePublication>, ()>;
}

impl<M: hl_linux::GuestMemory> RuntimeProcessSyscalls<M> {
    pub(super) fn reconcile_signal_frames(&self) -> Result<(), Errno> {
        let Some(port) = self.signal_frames.as_deref() else {
            return Ok(());
        };
        let machine = port.snapshot(self.thread).map_err(|_| Errno::EIO)?;
        self.tasks
            .unwind_signal_frames(self.thread, Self::stack_pointer(&machine))
            .map(|_| ())
            .map_err(|_| Errno::ESRCH)
    }

    pub fn deliver_forced_frame(&self) -> LinuxResult {
        let Some(port) = self.signal_frames.as_deref() else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Some(forced) = self.tasks.prepare_forced_delivery(self.thread) else {
            return LinuxResult::Error(Errno::EAGAIN);
        };
        let info = forced.info();
        let Ok(action) = self.tasks.action(self.process, info.signal) else { return LinuxResult::Error(Errno::ESRCH) };
        if !matches!(action.disposition, SignalDisposition::Handler(_)) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let Ok(thread) = self.tasks.deliver_thread_state(self.thread) else { return LinuxResult::Error(Errno::ESRCH) };
        let Ok(machine) = port.snapshot(self.thread) else { return LinuxResult::Error(Errno::EIO) };
        if Self::architecture(&machine) != self.architecture {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let request = SignalFrameRequest {
            machine,
            information: info,
            action,
            mask: thread.mask,
            alternate_stack: thread.alternate_stack,
            sigreturn_pc: if action.flags & SA_RESTORER != 0 {
                action.restorer
            } else {
                match port.default_sigreturn_pc(self.architecture) {
                    Some(address) => address,
                    None => return LinuxResult::Error(Errno::EINVAL),
                }
            },
        };
        let image = match SignalFrameCodec::build(&request) {
            Ok(image) => image,
            Err(error) => return LinuxResult::Error(Self::frame_errno(error)),
        };
        let Ok(mut publication) = port.prepare_install(self.thread, &image) else {
            return LinuxResult::Error(Errno::EFAULT)
        };
        if publication.publish().is_err() {
            return LinuxResult::Error(Errno::EIO);
        }
        if self
            .tasks
            .commit_frame_delivery(
                forced,
                image.handler_mask,
                image.handler_alternate_stack,
                Self::stack_pointer(&image.handler_machine),
                action.flags & SA_RESETHAND != 0,
            )
            .is_err()
        {
            return LinuxResult::Error(Errno::ESRCH);
        }
        publication.commit();
        LinuxResult::Value(0)
    }

    pub(crate) fn rt_sigreturn(&self) -> LinuxResult {
        let Some(port) = self.signal_frames.as_deref() else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Ok(current) = port.snapshot(self.thread) else { return LinuxResult::Error(Errno::EIO) };
        let (address, length) = match &current {
            SignalMachine::Aarch64(machine) => (machine.stack_pointer, AARCH64_SIGNAL_FRAME_SIZE),
            SignalMachine::X86_64(machine) => (machine.stack_pointer, X86_SIGNAL_FRAME_SIZE - 8),
        };
        let Ok(bytes) = port.read_frame(self.thread, address, length) else { return LinuxResult::Error(Errno::EFAULT) };
        let restore = match SignalFrameCodec::restore(self.architecture, address, &bytes) {
            Ok(restore) => restore,
            Err(error) => return LinuxResult::Error(Self::frame_errno(error)),
        };
        let Ok(mut publication) = port.prepare_restore(self.thread, &restore.machine) else {
            return LinuxResult::Error(Errno::EFAULT)
        };
        if publication.publish().is_err() {
            return LinuxResult::Error(Errno::EIO);
        }
        if self
            .tasks
            .replace_signal_context(self.thread, restore.mask, restore.alternate_stack)
            .is_err()
        {
            return LinuxResult::Error(Errno::ESRCH);
        }
        publication.commit();
        LinuxResult::Value(Self::result_register(&restore.machine))
    }

    fn architecture(machine: &SignalMachine) -> GuestArchitecture {
        match machine {
            SignalMachine::Aarch64(_) => GuestArchitecture::Aarch64,
            SignalMachine::X86_64(_) => GuestArchitecture::X86_64,
        }
    }

    fn stack_pointer(machine: &SignalMachine) -> u64 {
        match machine {
            SignalMachine::Aarch64(machine) => machine.stack_pointer,
            SignalMachine::X86_64(machine) => machine.stack_pointer,
        }
    }

    fn frame_errno(error: SignalFrameError) -> Errno {
        match error {
            SignalFrameError::Address | SignalFrameError::Alignment | SignalFrameError::Overflow => Errno::EFAULT,
            SignalFrameError::Architecture | SignalFrameError::Malformed | SignalFrameError::UnsupportedState => {
                Errno::EINVAL
            }
        }
    }

    fn result_register(machine: &SignalMachine) -> u64 {
        match machine {
            SignalMachine::Aarch64(machine) => machine.registers[0],
            SignalMachine::X86_64(machine) => machine.registers[0],
        }
    }
}
