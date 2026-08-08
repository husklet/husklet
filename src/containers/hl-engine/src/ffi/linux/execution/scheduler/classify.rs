//! Syscall classification and step-outcome mapping for the scheduler.

use hl_execution::{ExecutionCpuSnapshot, ExecutionMachine, StepOutcome};
use hl_runtime::RuntimeSyscallRouter;

use super::super::{ArenaMemory, GuestExecutor};
use super::{ThreadTerminal, TurnAction};
use crate::activation::GuestIsa;
use crate::engine::EngineError;

impl GuestExecutor {
    pub(super) fn cpu_boundary_call(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 103 | 107 | 110),
            GuestIsa::X86_64 => matches!(number, 38 | 222 | 223),
        }
    }

    pub(super) fn configures_cpu_boundary(isa: GuestIsa, machine: &ExecutionMachine) -> bool {
        let mut configures = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            let number = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8],
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
            };
            configures = Self::cpu_boundary_call(isa, number);
            StepOutcome::Continue
        });
        outcome == StepOutcome::Continue && configures
    }

    pub(super) fn cpu_accounting_call(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 153 | 165),
            GuestIsa::X86_64 => matches!(number, 98 | 100),
        }
    }

    pub(super) fn observes_cpu_accounting(isa: GuestIsa, machine: &ExecutionMachine) -> bool {
        let mut observes = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            let number = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8],
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
            };
            observes = Self::cpu_accounting_call(isa, number);
            StepOutcome::Continue
        });
        outcome == StepOutcome::Continue && observes
    }

    /// Syscalls that change which signals are deliverable: masks, dispositions,
    /// alternate stacks, and handler return. Servicing one inline would apply the
    /// new deliverability without passing a signal-delivery boundary.
    pub(super) fn changes_signal_delivery(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 132 | 134 | 135 | 138),
            GuestIsa::X86_64 => matches!(number, 13 | 14 | 129 | 131),
        }
    }

    pub(super) fn defers_signal_delivery(isa: GuestIsa, machine: &ExecutionMachine) -> bool {
        let mut defers = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            let number = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8],
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
            };
            defers = Self::changes_signal_delivery(isa, number);
            StepOutcome::Continue
        });
        outcome == StepOutcome::Continue && defers
    }

    pub(super) fn descriptor_blocks(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(
                number,
                32 | 63..=66 | 182 | 183 | 202 | 203 | 206 | 207 | 211 | 212 | 242
            ),
            GuestIsa::X86_64 => matches!(number, 0 | 1 | 19 | 20 | 42..=47 | 73 | 242 | 243 | 288),
        }
    }

    pub(super) fn record_lock_blocks(isa: GuestIsa, number: u64, command: u64) -> bool {
        command == 7
            && match isa {
                GuestIsa::Aarch64 => number == 25,
                GuestIsa::X86_64 => number == 72,
            }
    }

    pub(super) fn ipc_blocks(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 188 | 189 | 192 | 193),
            GuestIsa::X86_64 => matches!(number, 65 | 69 | 70 | 220),
        }
    }

    pub(super) fn signal_blocks(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 133 | 137),
            GuestIsa::X86_64 => matches!(number, 34 | 128 | 130),
        }
    }

    pub(super) fn readiness_blocks(isa: GuestIsa, number: u64) -> bool {
        // The scheduler classifies before it can safely inspect guest timeout
        // pointers. Route the whole wait family through the waiter pool: zero
        // timeouts complete there immediately, while finite and infinite waits
        // cannot pin the sole guest scheduling thread.
        match isa {
            GuestIsa::Aarch64 => matches!(number, 22 | 72 | 73 | 441),
            GuestIsa::X86_64 => matches!(number, 7 | 23 | 232 | 270 | 271 | 281 | 441),
        }
    }
    pub(super) fn step<M>(isa: GuestIsa, memory: &M, outcome: StepOutcome) -> Result<TurnAction, EngineError>
    where
        M: hl_execution::ExecutionInstructionMemory + super::super::operand::ImageMemory,
    {
        match outcome {
            StepOutcome::Yield | StepOutcome::Continue => Ok(TurnAction::Continue),
            StepOutcome::ReplaceImage { generation } => Ok(TurnAction::Replace(generation)),
            StepOutcome::Syscall { .. } => Ok(TurnAction::Dispatch),
            StepOutcome::Exit { status } => Ok(TurnAction::Terminal(ThreadTerminal::Thread(Self::code(status)))),
            StepOutcome::Fault(fault) => Ok(Self::fault_signal(memory, fault).map_or_else(
                || TurnAction::Terminal(ThreadTerminal::Thread(Self::fault(isa, memory, fault))),
                |(signal, code, address)| TurnAction::Signal {
                    signal,
                    code,
                    address,
                    fallback: Self::fault(isa, memory, fault),
                },
            )),
        }
    }

    pub(super) fn fault_signal<M>(memory: &M, fault: hl_execution::ExecutionFault) -> Option<(u8, i32, u64)>
    where
        M: super::super::operand::ImageMemory,
    {
        let (address, span) = match fault {
            hl_execution::ExecutionFault::Memory(value) => (value.address, None),
            hl_execution::ExecutionFault::Operand(value) => (value.address(), Some(value.length())),
            hl_execution::ExecutionFault::Alignment { address, .. } => {
                return Some((7, 1, address));
            }
            hl_execution::ExecutionFault::Decode { instruction }
            | hl_execution::ExecutionFault::Unsupported { instruction } => {
                return Some((4, 2, instruction));
            }
            hl_execution::ExecutionFault::Signal(trap) => {
                let signal = match trap.signal {
                    hl_execution::TrapSignal::Illegal => 4,
                    hl_execution::TrapSignal::Divide => 8,
                    hl_execution::TrapSignal::Breakpoint => 5,
                };
                return Some((signal, trap.code, trap.address));
            }
            _ => return None,
        };
        let lease = memory.selected_lease();
        let (signal, code, reported) = Self::classify(memory, &lease, address, span);
        Some((signal, code, reported))
    }

    pub(super) fn classify(
        memory: &impl super::super::operand::ImageMemory,
        lease: &super::super::space::SpaceLease,
        address: u64,
        span: Option<u64>,
    ) -> (u8, i32, u64) {
        let bus = span.and_then(|length| memory.address_space().resolve_bus(lease, address, length));
        if let Some(fault) = bus.filter(|fault| lease.arena().take_bus(*fault)) {
            (7, 2, fault)
        } else {
            let mapped = lease
                .mappings_ref()
                .ledger()
                .regions()
                .iter()
                .any(|region| region.range().contains(hl_isa::GuestAddress::new(address)));
            (11, if mapped { 2 } else { 1 }, address)
        }
    }

    pub(super) fn blocks(
        isa: GuestIsa,
        machine: &ExecutionMachine,
        router: &RuntimeSyscallRouter,
        solo: bool,
    ) -> Result<bool, EngineError> {
        let mut blocking = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            let architecture = match isa {
                GuestIsa::Aarch64 => hl_isa::GuestArchitecture::Aarch64,
                GuestIsa::X86_64 => hl_isa::GuestArchitecture::X86_64,
            };
            let (number, operation) = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => (cpu.registers[8], cpu.registers[1]),
                ExecutionCpuSnapshot::X86_64(cpu) => (cpu.registers[0], cpu.registers[6]),
            };
            blocking = matches!((number, operation & 127), (98 | 202, 0 | 6 | 9 | 11 | 13))
                || matches!(number, 35 | 61 | 101 | 115 | 230 | 260)
                || Self::record_lock_blocks(isa, number, operation)
                || (Self::descriptor_blocks(isa, number) && (!solo || router.descriptor_may_block(architecture, cpu)))
                || Self::ipc_blocks(isa, number)
                || Self::signal_blocks(isa, number)
                || Self::readiness_blocks(isa, number);
            blocking = blocking || router.filesystem_may_block(architecture, cpu);
            StepOutcome::Continue
        });
        match outcome {
            StepOutcome::Continue => Ok(blocking),
            _ => Err(EngineError::WaitFailed),
        }
    }

    pub(super) fn dispatch_outcome(
        isa: GuestIsa,
        memory: &ArenaMemory,
        router: &RuntimeSyscallRouter,
        outcome: StepOutcome,
    ) -> Result<Option<ThreadTerminal>, EngineError> {
        match outcome {
            StepOutcome::Continue | StepOutcome::Yield => Ok(None),
            StepOutcome::ReplaceImage { generation } => Self::replace(router, generation).map(|()| None),
            StepOutcome::Exit { status } => {
                let exit = Self::code(status);
                Ok(Some(match router.take_terminal() {
                    Some(hl_runtime::RuntimeTerminal::Group(_)) => ThreadTerminal::Group(exit),
                    _ => ThreadTerminal::Thread(exit),
                }))
            }
            StepOutcome::Fault(fault) => Ok(Some(ThreadTerminal::Thread(Self::fault(isa, memory, fault)))),
            StepOutcome::Syscall { .. } => Err(EngineError::WaitFailed),
        }
    }
}
