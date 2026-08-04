use crate::{
    AccessKind, CpuState, ExclusiveMemory, ExecutionExit, GuestOperandMemory, ScalarInstruction, ScalarIr,
    ScalarOperand, Staged,
};
mod access;
mod endian;
mod execute;
mod flags;

pub struct ScalarInterpreter;
impl ScalarInterpreter {
    pub fn execute<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        ir: ScalarIr,
    ) -> ExecutionExit {
        Self::execute_with_budget(cpu, memory, ir, 1024)
    }
    pub fn execute_with_budget<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        ir: ScalarIr,
        budget: u64,
    ) -> ExecutionExit {
        if let ScalarInstruction::String(string) = ir.instruction {
            return crate::x86::string::StringInterpreter::execute(cpu, memory, ir.length, ir.width, string, budget);
        }
        if let ScalarInstruction::Leave { address_32 } = ir.instruction {
            return Self::leave(cpu, memory, ir.length, ir.width, address_32);
        }
        let instruction = cpu.rip;
        let next = instruction.wrapping_add(u64::from(ir.length));
        if let Some(exit) = crate::x86::eager::Eager::execute(cpu, memory, ir.instruction, ir.width, instruction, next)
        {
            return exit;
        }
        if let ScalarInstruction::CompareExchange {
            destination: ScalarOperand::Memory(address),
            source,
            locked: true,
        } = ir.instruction
        {
            return crate::x86::compare_exchange::CompareExchange::locked(
                cpu,
                memory,
                address,
                source,
                ir.width,
                instruction,
                next,
            );
        }
        if let ScalarInstruction::WideCompareExchange { address, wide, locked } = ir.instruction {
            return crate::x86::compare_exchange::CompareExchange::wide(
                cpu,
                memory,
                address,
                wide,
                locked,
                instruction,
                next,
            );
        }
        if let ScalarInstruction::Exchange {
            destination: ScalarOperand::Memory(address),
            source,
        } = ir.instruction
        {
            return crate::x86::compare_exchange::CompareExchange::exchange(
                cpu,
                memory,
                address,
                source,
                ir.width,
                instruction,
                next,
            );
        }
        if let ScalarInstruction::ExchangeAdd {
            destination: ScalarOperand::Memory(address),
            source,
            locked: true,
        } = ir.instruction
        {
            return crate::x86::compare_exchange::CompareExchange::locked_add(
                cpu,
                memory,
                address,
                source,
                ir.width,
                instruction,
                next,
            );
        }
        if let ScalarInstruction::Alu {
            operation,
            destination: ScalarOperand::Memory(address),
            source,
            locked: true,
        } = ir.instruction
        {
            let source = match source {
                ScalarOperand::Immediate(value) => value as u64,
                ScalarOperand::Register(register) => cpu.read_register(register, ir.width),
                ScalarOperand::Memory(_) => unreachable!(),
            };
            return crate::x86::compare_exchange::CompareExchange::locked_alu(
                cpu,
                memory,
                address,
                source,
                operation,
                ir.width,
                instruction,
                next,
            );
        }
        if let ScalarInstruction::VexGather {
            destination,
            mask,
            index,
            address,
            element,
            index_bytes,
            wide,
        } = ir.instruction
        {
            return Self::vex_gather(
                cpu,
                memory,
                destination,
                mask,
                index,
                address,
                element,
                index_bytes,
                wide,
                instruction,
                next,
            );
        }
        match Self::stage(cpu, memory, ir, instruction, next) {
            Ok(Staged::Cpu(staged)) => {
                *cpu = staged;
                ExecutionExit::Continue
            }
            Ok(Staged::Write(staged, reservation, value, address, length)) => {
                if memory.commit_write(reservation, value).is_err() {
                    return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction,
                        address,
                        AccessKind::Write,
                        u64::from(length),
                    ));
                }
                *cpu = staged;
                ExecutionExit::Continue
            }
            Ok(Staged::Batch(staged, reservation, values, address, length)) => {
                if memory
                    .commit_write_batch(reservation, &values[..usize::from(length / 8)])
                    .is_err()
                {
                    return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction,
                        address,
                        AccessKind::Write,
                        u64::from(length),
                    ));
                }
                *cpu = staged;
                ExecutionExit::Continue
            }
            Ok(Staged::Sparse(staged, reservations, values, count, address, length)) => {
                for (reservation, value) in reservations.into_iter().flatten().zip(values).take(usize::from(count)) {
                    if memory.commit_write(reservation, value).is_err() {
                        return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                            instruction, address, AccessKind::Write, u64::from(length),
                        ));
                    }
                }
                *cpu = staged;
                ExecutionExit::Continue
            }
            Ok(Staged::Exit(exit)) => exit,
            Err(exit) => exit,
        }
    }
}
