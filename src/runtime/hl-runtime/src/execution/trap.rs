use hl_execution::{ExecutionCpuSnapshot, ExecutionMachine, StepOutcome};
use hl_isa::GuestArchitecture;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTrapOutcome {
    Continue,
    ReplaceImage { generation: u64 },
    Exit(i32),
    Fault,
}

pub trait RuntimeSyscallTrap: Send + Sync {
    fn dispatch(&self, architecture: GuestArchitecture, cpu: &mut ExecutionCpuSnapshot) -> RuntimeTrapOutcome;
}

pub fn dispatch_runtime_syscall(
    machine: &ExecutionMachine,
    expected_epoch: u64,
    trap: &dyn RuntimeSyscallTrap,
) -> StepOutcome {
    machine.handle_syscall(expected_epoch, |cpu| match trap.dispatch(cpu.architecture(), cpu) {
        RuntimeTrapOutcome::Continue => StepOutcome::Continue,
        RuntimeTrapOutcome::ReplaceImage { generation } => StepOutcome::ReplaceImage { generation },
        RuntimeTrapOutcome::Exit(status) => StepOutcome::Exit { status },
        RuntimeTrapOutcome::Fault => StepOutcome::Fault(hl_execution::ExecutionFault::Unsupported {
            instruction: cpu.instruction(),
        }),
    })
}

trait CpuArchitecture {
    fn architecture(&self) -> GuestArchitecture;
    fn instruction(&self) -> u64;
}

impl CpuArchitecture for ExecutionCpuSnapshot {
    fn architecture(&self) -> GuestArchitecture {
        match self {
            Self::Aarch64(_) => GuestArchitecture::Aarch64,
            Self::X86_64(_) => GuestArchitecture::X86_64,
        }
    }

    fn instruction(&self) -> u64 {
        match self {
            Self::Aarch64(cpu) => cpu.pc,
            Self::X86_64(cpu) => cpu.rip,
        }
    }
}
