use crate::{Aarch64CpuState, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir, GuestSystemPort, SystemRegister};

pub(crate) struct Executor;

impl Executor {
    pub(crate) fn handles(instruction: &Aarch64Instruction) -> bool {
        matches!(
            *instruction,
            Aarch64Instruction::Barrier { .. }
                | Aarch64Instruction::SystemRead { .. }
                | Aarch64Instruction::SystemWrite { .. }
                | Aarch64Instruction::InstructionCache { .. }
        )
    }

    pub(crate) fn execute<S: GuestSystemPort>(
        cpu: &mut Aarch64CpuState,
        system: &mut S,
        ir: &Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        match ir.instruction {
            Aarch64Instruction::Barrier { kind, option } => {
                system.barrier(kind, option);
                Self::advance(cpu)
            }
            Aarch64Instruction::SystemRead { destination, register } => {
                let value = Self::read(cpu, system, register);
                cpu.set_register(destination, value);
                Self::advance(cpu)
            }
            Aarch64Instruction::SystemWrite { source, register } => {
                let value = cpu.register(source);
                register.write_local(cpu, value);
                Self::advance(cpu)
            }
            Aarch64Instruction::InstructionCache { source } => {
                system.invalidate_instruction(cpu.register(source));
                Self::advance(cpu)
            }
            _ => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word: ir.word,
            },
        }
    }

    fn read<S: GuestSystemPort>(cpu: &Aarch64CpuState, system: &S, register: SystemRegister) -> u64 {
        register.read_local(cpu).unwrap_or_else(|| match register {
            SystemRegister::CounterValue => system.counter_value(),
            SystemRegister::CounterFrequency => system.counter_frequency(),
            _ => unreachable!("local system register must have a local value"),
        })
    }

    fn advance(cpu: &mut Aarch64CpuState) -> Aarch64ExecutionExit {
        cpu.pc = cpu.pc.wrapping_add(4);
        Aarch64ExecutionExit::Continue
    }
}
