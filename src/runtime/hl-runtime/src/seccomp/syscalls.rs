use std::sync::Arc;

use hl_linux::{
    BpfInstruction, BpfProgram, Errno, FilterInstallPlan, GuestMemory, LinuxResult, SECCOMP_MAXIMUM_INSTRUCTIONS,
    SeccompPolicy, SeccompPolicyError, SeccompSyscalls, SyscallOperation,
};
use hl_task::{ProcessId, TaskRegistry, ThreadId};

use super::{Control, ControlError};

pub struct RuntimeSyscalls<M: GuestMemory> {
    control: Arc<Control>,
    tasks: Arc<TaskRegistry>,
    process: ProcessId,
    thread: ThreadId,
    memory: M,
    administrator: bool,
    baseline: hl_linux::SeccompBaseline,
}

impl<M: GuestMemory> RuntimeSyscalls<M> {
    pub fn new(
        control: Arc<Control>,
        tasks: Arc<TaskRegistry>,
        process: ProcessId,
        thread: ThreadId,
        memory: M,
    ) -> Self {
        Self {
            control,
            tasks,
            process,
            thread,
            memory,
            administrator: false,
            baseline: hl_linux::SeccompBaseline::Container,
        }
    }

    #[must_use]
    pub fn with_baseline(mut self, baseline: hl_linux::SeccompBaseline) -> Self {
        self.baseline = baseline;
        self
    }

    #[must_use]
    pub fn with_admin(mut self, administrator: bool) -> Self {
        self.administrator = administrator;
        self
    }

    fn filter(&self, flags: u64, address: u64) -> LinuxResult {
        let Ok(flags) = u32::try_from(flags) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if flags & !0x1f != 0 || flags & 0x08 != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let mut header = [0_u8; 16];
        if self.memory.read(address, &mut header) != Ok(16) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let length = usize::from(u16::from_le_bytes([header[0], header[1]]));
        if length == 0 || length > SECCOMP_MAXIMUM_INSTRUCTIONS {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let pointer = u64::from_le_bytes(header[8..16].try_into().expect("fixed slice"));
        if pointer == 0 {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let Some(byte_length) = length.checked_mul(8) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let mut encoded = vec![0_u8; byte_length];
        if self.memory.read(pointer, &mut encoded) != Ok(byte_length) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let instructions = encoded
            .chunks_exact(8)
            .map(|bytes| BpfInstruction {
                code: u16::from_le_bytes([bytes[0], bytes[1]]),
                jump_true: bytes[2],
                jump_false: bytes[3],
                value: u32::from_le_bytes(bytes[4..8].try_into().expect("fixed slice")),
            })
            .collect();
        let program = match BpfProgram::new(instructions) {
            Ok(program) => program,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let plan = match SeccompPolicy::install_plan(program, flags) {
            Ok(plan) => plan,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        self.install(plan)
    }

    fn install(&self, plan: FilterInstallPlan) -> LinuxResult {
        let Some(credentials) = self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|process| process.id == self.process)
            .map(|process| process.credentials)
        else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        if credentials.no_new_privileges {
            if let Err(error) = self.control.lock_privileges(self.thread) {
                return LinuxResult::Error(Self::errno(error));
            }
        }
        let administrator = self.administrator || credentials.has_capability(1_u64 << 21);
        let threads = self
            .tasks
            .snapshot()
            .threads
            .into_iter()
            .filter(|thread| thread.process == self.process)
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        let divergence_is_esrch = plan.flags.synchronize_threads_esrch;
        let transaction = match self.control.begin_install(self.thread, &threads, plan, administrator) {
            Ok(transaction) => transaction,
            Err(ControlError::PolicyDivergence(thread)) if !divergence_is_esrch => {
                return LinuxResult::Value(u64::from(thread.number()));
            }
            Err(ControlError::PolicyDivergence(_)) => {
                return LinuxResult::Error(Errno::ESRCH);
            }
            Err(error) => return LinuxResult::Error(Self::errno(error)),
        };
        match self.control.commit_install(transaction) {
            Ok(None) => LinuxResult::Value(0),
            Ok(Some(_)) => LinuxResult::Error(Errno::EINVAL),
            Err(error) => LinuxResult::Error(Self::errno(error)),
        }
    }

    fn errno(error: ControlError) -> Errno {
        match error {
            ControlError::Policy(SeccompPolicyError::PermissionDenied) => Errno::EACCES,
            ControlError::Policy(SeccompPolicyError::Capacity) => Errno::ENOMEM,
            ControlError::Conflict => Errno::EBUSY,
            ControlError::MissingThread => Errno::ESRCH,
            _ => Errno::EINVAL,
        }
    }

    fn action(&self, flags: u64, address: u64) -> LinuxResult {
        if flags != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let mut bytes = [0_u8; 4];
        if self.memory.read(address, &mut bytes) != Ok(4) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let action = u32::from_le_bytes(bytes);
        if matches!(
            action,
            0x8000_0000
                | 0x0000_0000
                | 0x0003_0000
                | 0x0005_0000
                | 0x7fc0_0000
                | 0x7ff0_0000
                | 0x7ffc_0000
                | 0x7fff_0000
        ) {
            LinuxResult::Value(0)
        } else {
            LinuxResult::Error(Errno::from_raw(95))
        }
    }

    fn sizes(&self, flags: u64, address: u64) -> LinuxResult {
        if flags != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let mut bytes = [0_u8; 6];
        bytes[..2].copy_from_slice(&80_u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&24_u16.to_le_bytes());
        bytes[4..].copy_from_slice(&64_u16.to_le_bytes());
        if self.memory.write(address, &bytes) != Ok(bytes.len()) {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(0)
        }
    }
}

impl<M: GuestMemory> SeccompSyscalls for RuntimeSyscalls<M> {
    fn handle(&mut self, _: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match arguments[0] {
            0 if arguments[1] == 0 && arguments[2] == 0 => self
                .control
                .enable_strict(self.thread)
                .map(|()| LinuxResult::Value(0))
                .unwrap_or_else(|error| LinuxResult::Error(Self::errno(error))),
            0 => LinuxResult::Error(Errno::EINVAL),
            1 => self.filter(arguments[1], arguments[2]),
            2 => self.action(arguments[1], arguments[2]),
            3 => self.sizes(arguments[1], arguments[2]),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }

    fn evaluate(&self, frame: &hl_linux::SyscallFrame, instruction_pointer: u64) -> hl_linux::SeccompDecision {
        self.control
            .evaluate_syscall(self.thread, frame, instruction_pointer)
            .unwrap_or(hl_linux::SeccompDecision::Kill {
                scope: hl_linux::SeccompKillScope::Thread,
                signal: 31,
            })
    }
}

impl<M: GuestMemory + Send + Sync> super::PrctlPort for RuntimeSyscalls<M> {
    fn mode(&self) -> LinuxResult {
        match self.control.status(self.thread, self.baseline) {
            Ok(status) => LinuxResult::Value(match status.mode {
                hl_linux::SeccompMode::Disabled => 0,
                hl_linux::SeccompMode::Strict => 1,
                hl_linux::SeccompMode::Filter => 2,
            }),
            Err(error) => LinuxResult::Error(Self::errno(error)),
        }
    }

    fn strict(&self) -> LinuxResult {
        self.control
            .enable_strict(self.thread)
            .map(|()| LinuxResult::Value(0))
            .unwrap_or_else(|error| LinuxResult::Error(Self::errno(error)))
    }

    fn filter(&self, address: u64) -> LinuxResult {
        Self::filter(self, 0, address)
    }

    fn retire(&self, threads: &[ThreadId]) {
        for thread in threads {
            if self.control.unregister(*thread).is_err() {
                continue;
            }
        }
    }
}
