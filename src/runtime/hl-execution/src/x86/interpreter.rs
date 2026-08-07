use crate::{
    AccessKind, CpuState, ExclusiveMemory, ExecutionExit, GuestOperandMemory, ScalarInstruction, ScalarIr,
    ScalarOperand, Staged,
};
mod access;
mod endian;
mod execute;
mod flags;
mod scalar;

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
        let mut scalar = cpu.scalar.clone();
        scalar.rip = next;
        match Self::stage_scalar(cpu, &mut scalar, memory, ir, instruction, next) {
            Ok(Some(staged)) => {
                return match Self::commit(memory, staged, instruction) {
                    Ok(()) => {
                        cpu.scalar = scalar;
                        ExecutionExit::Continue
                    }
                    Err(exit) => exit,
                };
            }
            Ok(None) => {}
            Err(exit) => return exit,
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        match Self::stage(cpu, &mut staged, memory, ir, instruction, next)
            .and_then(|staged| Self::commit(memory, staged, instruction))
        {
            Ok(()) => {
                *cpu = staged;
                ExecutionExit::Continue
            }
            Err(exit) => exit,
        }
    }

    /// Performs the deferred memory commit. `Ok` means the scratch is now the
    /// architectural state and the caller must assign it.
    fn commit<M: GuestOperandMemory>(
        memory: &mut M,
        staged: Staged<M::Reservation, M::BatchReservation>,
        instruction: u64,
    ) -> Result<(), ExecutionExit> {
        let fault = |address, length: u8| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Write,
                u64::from(length),
            ))
        };
        match staged {
            Staged::Cpu => Ok(()),
            Staged::Write(reservation, value, address, length) => memory
                .commit_write(reservation, value)
                .map_err(|()| fault(address, length)),
            Staged::Batch(reservation, values, address, length) => memory
                .commit_write_batch(reservation, &values[..usize::from(length / 8)])
                .map_err(|()| fault(address, length)),
            Staged::Sparse(reservations, values, count, address, length) => {
                for (reservation, value) in reservations.into_iter().flatten().zip(values).take(usize::from(count)) {
                    memory
                        .commit_write(reservation, value)
                        .map_err(|()| fault(address, length))?;
                }
                Ok(())
            }
            Staged::Exit(exit) => Err(exit),
        }
    }
}
