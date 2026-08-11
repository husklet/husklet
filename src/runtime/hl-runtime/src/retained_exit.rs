use hl_execution::ExecutionCpuSnapshot;
use hl_isa::GuestArchitecture;

use crate::{RuntimeSyscallTrap, RuntimeTrapOutcome};

/// Runtime owner for the context-free `AArch64` `exit` syscall used by the retained translator.
#[derive(Debug, Default)]
pub struct RetainedExitTrap;

impl RetainedExitTrap {
    #[must_use]
    pub const fn dispatch_aarch64(&self, number: u64, status: u64) -> RuntimeTrapOutcome {
        if number == 93 {
            RuntimeTrapOutcome::Exit((status & 0xff) as i32)
        } else {
            RuntimeTrapOutcome::Fault
        }
    }
}

impl RuntimeSyscallTrap for RetainedExitTrap {
    fn dispatch(&self, architecture: GuestArchitecture, cpu: &mut ExecutionCpuSnapshot) -> RuntimeTrapOutcome {
        let ExecutionCpuSnapshot::Aarch64(cpu) = cpu else {
            return RuntimeTrapOutcome::Fault;
        };
        if architecture != GuestArchitecture::Aarch64 {
            return RuntimeTrapOutcome::Fault;
        }
        self.dispatch_aarch64(cpu.registers[8], cpu.registers[0])
    }
}

#[cfg(test)]
mod tests {
    use super::RetainedExitTrap;
    use crate::{RuntimeSyscallTrap, RuntimeTrapOutcome};
    use hl_execution::{Aarch64CpuState, ExecutionCpuSnapshot};
    use hl_isa::GuestArchitecture;

    #[test]
    fn aarch64_exit_status_is_linux_low_byte() {
        let mut cpu = Aarch64CpuState::default();
        cpu.registers[0] = 0x12a;
        cpu.registers[8] = 93;
        assert_eq!(
            RetainedExitTrap.dispatch(GuestArchitecture::Aarch64, &mut ExecutionCpuSnapshot::Aarch64(cpu),),
            RuntimeTrapOutcome::Exit(42)
        );
    }
}
