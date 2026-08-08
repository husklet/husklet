use super::coordinate::{Identity as IdentityCoordinates, stage_branch};
use super::state::ScalarAccess;
use super::{atomic, system};
use crate::aarch64_integer_support::{
    add_carry, arithmetic, bitfield, logical, logical_flags, logical_operand, multiply, reverse_bytes, select_value,
    shifted, write_destination, write_register,
};
use crate::{
    Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir,
    ExclusiveMemory, GuestOperandMemory, GuestSystemPort, LogicalOperation, MoveWideOperation, PcCoordinatePort,
    aarch64_memory::Aarch64MemoryInterpreter, aarch64_simd_interpreter::Aarch64SimdInterpreter,
};
pub struct Interpreter;
pub type Aarch64Interpreter = Interpreter;
impl Aarch64Interpreter {
    pub fn execute_word(
        cpu: &mut Aarch64CpuState,
        coordinates: &dyn PcCoordinatePort,
        word: u32,
    ) -> Aarch64ExecutionExit {
        match Aarch64Decoder::decode(word) {
            Ok(ir) => Self::execute(cpu, coordinates, &ir),
            Err(Aarch64DecodeError::Reserved) => Aarch64ExecutionExit::UndefinedInstruction {
                instruction: cpu.pc,
                word,
            },
            Err(Aarch64DecodeError::Unsupported) => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word,
            },
        }
    }
    pub fn execute_memory<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        word: u32,
    ) -> Aarch64ExecutionExit {
        match Aarch64Decoder::decode(word) {
            Ok(ir) => Self::execute_with_memory(cpu, memory, coordinates, &ir),
            Err(Aarch64DecodeError::Reserved) => Aarch64ExecutionExit::UndefinedInstruction {
                instruction: cpu.pc,
                word,
            },
            Err(Aarch64DecodeError::Unsupported) => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word,
            },
        }
    }

    pub fn execute_with_memory<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        ir: &Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        if cpu.pc & 3 != 0 {
            return Aarch64ExecutionExit::AlignmentFault {
                instruction: cpu.pc,
                target: cpu.pc,
                access: crate::AccessKind::Execute,
            };
        }
        if Aarch64MemoryInterpreter::is_memory(&ir.instruction) {
            return Aarch64MemoryInterpreter::execute(cpu, memory, coordinates, ir);
        }
        Self::execute(cpu, coordinates, ir)
    }
    pub fn execute_concurrent<E: ExclusiveMemory, S: GuestSystemPort>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        system: &mut S,
        word: u32,
    ) -> Aarch64ExecutionExit {
        match Aarch64Decoder::decode(word) {
            Ok(ir) => Self::execute_with_concurrency(cpu, memory, system, &ir),
            Err(Aarch64DecodeError::Reserved) => Aarch64ExecutionExit::UndefinedInstruction {
                instruction: cpu.pc,
                word,
            },
            Err(Aarch64DecodeError::Unsupported) => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word,
            },
        }
    }

    pub fn execute_with_concurrency<E: ExclusiveMemory, S: GuestSystemPort>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        system: &mut S,
        ir: &Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        if cpu.pc & 3 != 0 {
            return Aarch64ExecutionExit::AlignmentFault {
                instruction: cpu.pc,
                target: cpu.pc,
                access: crate::AccessKind::Execute,
            };
        }
        if atomic::Executor::handles(&ir.instruction) {
            return atomic::Executor::execute(cpu, memory, ir);
        }
        if system::Executor::handles(&ir.instruction) {
            return system::Executor::execute(cpu, system, ir);
        }
        Self::execute(cpu, &IdentityCoordinates, ir)
    }

    pub fn execute(
        cpu: &mut Aarch64CpuState,
        coordinates: &dyn PcCoordinatePort,
        ir: &Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        if let Some(exit) = super::crc32::Crc::execute(cpu, ir) {
            return exit;
        }
        if Aarch64SimdInterpreter::is_simd(&ir.instruction) {
            return Aarch64SimdInterpreter::execute(cpu, ir);
        }
        let instruction = cpu.pc;
        if instruction & 3 != 0 {
            return Aarch64ExecutionExit::AlignmentFault {
                instruction,
                target: instruction,
                access: crate::AccessKind::Execute,
            };
        }
        let mut staged = cpu.stage_scalar();
        staged.pc = instruction.wrapping_add(4);
        let exit = match ir.instruction {
            Aarch64Instruction::AddSubtractImmediate {
                subtract,
                set_flags,
                source,
                destination,
                immediate,
            } => {
                let left = cpu.register_or_sp(source);
                let result = arithmetic(&mut staged, ir.wide, left, immediate, subtract, set_flags);
                if set_flags {
                    write_register(&mut staged, ir.wide, destination, result);
                } else {
                    write_destination(&mut staged, ir.wide, destination, result);
                }
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::AddSubtractShifted {
                subtract,
                set_flags,
                source,
                operand,
                destination,
                shift,
                amount,
            } => {
                let right = shifted(cpu.register(operand), shift, amount, ir.wide);
                let result = arithmetic(&mut staged, ir.wide, cpu.register(source), right, subtract, set_flags);
                write_register(&mut staged, ir.wide, destination, result);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::AddSubtractExtended {
                subtract,
                set_flags,
                source,
                operand,
                destination,
                extension,
                amount,
            } => {
                let right = extension.apply(cpu.register(operand)) << amount;
                let result = arithmetic(
                    &mut staged,
                    ir.wide,
                    cpu.register_or_sp(source),
                    right,
                    subtract,
                    set_flags,
                );
                if set_flags {
                    write_register(&mut staged, ir.wide, destination, result);
                } else {
                    write_destination(&mut staged, ir.wide, destination, result);
                }
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::AddCarry {
                subtract,
                set_flags,
                left,
                right,
                destination,
            } => {
                let value = add_carry(
                    &mut staged,
                    ir.wide,
                    cpu.register(left),
                    cpu.register(right),
                    cpu.nzcv.carry(),
                    subtract,
                    set_flags,
                );
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::LogicalImmediate {
                operation,
                source,
                destination,
                mask,
            } => {
                let result = logical(operation, cpu.register(source), mask, ir.wide);
                if operation == LogicalOperation::Ands {
                    logical_flags(&mut staged, result, ir.wide);
                    write_register(&mut staged, ir.wide, destination, result);
                } else {
                    write_destination(&mut staged, ir.wide, destination, result);
                }
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::LogicalShifted {
                operation,
                invert,
                source,
                operand,
                destination,
                shift,
                amount,
            } => {
                let right = logical_operand(cpu.register(operand), shift, amount, ir.wide, invert);
                let result = logical(operation, cpu.register(source), right, ir.wide);
                if operation == LogicalOperation::Ands {
                    logical_flags(&mut staged, result, ir.wide);
                }
                write_register(&mut staged, ir.wide, destination, result);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::MoveWide {
                operation,
                destination,
                immediate,
                shift,
            } => {
                let field = u64::from(immediate) << shift;
                let value = match operation {
                    MoveWideOperation::Not => !field,
                    MoveWideOperation::Zero => field,
                    MoveWideOperation::Keep => cpu.register(destination) & !(0xffff_u64 << shift) | field,
                };
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::Bitfield {
                operation,
                source,
                destination,
                rotate,
                sign_bit,
                write_mask,
                top_mask,
            } => {
                let value = bitfield(
                    cpu,
                    ir.wide,
                    operation,
                    source,
                    destination,
                    rotate,
                    sign_bit,
                    write_mask,
                    top_mask,
                );
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::Extract {
                high,
                low,
                destination,
                amount,
            } => {
                let value = if ir.wide {
                    cpu.register(low) >> amount | cpu.register(high).wrapping_shl(u32::from(64 - amount))
                } else {
                    let low = cpu.register(low) as u32;
                    let high = cpu.register(high) as u32;
                    u64::from(low >> amount | high.wrapping_shl(u32::from(32 - amount)))
                };
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::Multiply {
                operation,
                subtract,
                source,
                operand,
                addend,
                destination,
            } => {
                let value = multiply(cpu, operation, subtract, source, operand, addend);
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::VariableShift {
                shift,
                source,
                amount,
                destination,
            } => {
                let count = cpu.register(amount) as u8 & if ir.wide { 63 } else { 31 };
                let value = shifted(cpu.register(source), shift, count, ir.wide);
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::Divide {
                operation,
                source,
                divisor,
                destination,
            } => {
                let value = operation.apply(ir.wide, cpu.register(source), cpu.register(divisor));
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::ByteReverse {
                source,
                destination,
                container_bytes,
            } => {
                write_register(
                    &mut staged,
                    ir.wide,
                    destination,
                    reverse_bytes(cpu.register(source), if ir.wide { 8 } else { 4 }, container_bytes),
                );
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::BitReverse { source, destination } => {
                let value = if ir.wide {
                    cpu.register(source).reverse_bits()
                } else {
                    u64::from((cpu.register(source) as u32).reverse_bits())
                };
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::CountLeadingZero { source, destination } => {
                let value = if ir.wide {
                    cpu.register(source).leading_zeros()
                } else {
                    (cpu.register(source) as u32).leading_zeros()
                };
                staged.write(destination, u64::from(value));
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::ConditionalCompare {
                subtract,
                source,
                operand,
                condition,
                literal,
            } => {
                if condition.holds(cpu.nzcv) {
                    let right = operand.read(cpu);
                    arithmetic(&mut staged, ir.wide, cpu.register(source), right, subtract, true);
                } else {
                    staged.nzcv = crate::Nzcv::from_bits(u32::from(literal) << 28);
                }
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::Address {
                destination,
                displacement,
                page,
            } => {
                let base = coordinates.architectural_pc(instruction);
                let base = if page { base & !0xfff } else { base };
                let scale = if page { 4096 } else { 1 };
                staged.write(
                    destination,
                    base.wrapping_add((displacement as u64).wrapping_mul(scale)),
                );
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::BranchImmediate { displacement, link } => {
                if link {
                    staged.write(30, coordinates.architectural_pc(instruction).wrapping_add(4));
                }
                stage_branch(&mut staged, instruction, instruction.wrapping_add(displacement as u64))
            }
            Aarch64Instruction::BranchRegister { source, link } => {
                let target = cpu.register(source);
                if link {
                    staged.write(30, coordinates.architectural_pc(instruction).wrapping_add(4));
                }
                stage_branch(&mut staged, instruction, target)
            }
            Aarch64Instruction::Return { source } => stage_branch(&mut staged, instruction, cpu.register(source)),
            Aarch64Instruction::BranchConditional {
                condition,
                displacement,
            } => {
                let target = if condition.holds(cpu.nzcv) {
                    instruction.wrapping_add(displacement as u64)
                } else {
                    instruction.wrapping_add(4)
                };
                stage_branch(&mut staged, instruction, target)
            }
            Aarch64Instruction::ConditionalSelect {
                source,
                alternate,
                destination,
                condition,
                invert,
                increment,
            } => {
                let holds = condition.holds(cpu.nzcv);
                let value = select_value(cpu, source, alternate, holds, invert, increment);
                write_register(&mut staged, ir.wide, destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::CompareBranch {
                source,
                nonzero,
                displacement,
            } => {
                let value = if ir.wide {
                    cpu.register(source)
                } else {
                    u64::from(cpu.register(source) as u32)
                };
                let take = (value != 0) == nonzero;
                let target = if take {
                    instruction.wrapping_add(displacement as u64)
                } else {
                    instruction.wrapping_add(4)
                };
                stage_branch(&mut staged, instruction, target)
            }
            Aarch64Instruction::TestBranch {
                source,
                bit,
                nonzero,
                displacement,
            } => {
                let take = ((cpu.register(source) >> bit) & 1 != 0) == nonzero;
                let target = if take {
                    instruction.wrapping_add(displacement as u64)
                } else {
                    instruction.wrapping_add(4)
                };
                stage_branch(&mut staged, instruction, target)
            }
            Aarch64Instruction::SystemRead { destination, register } => {
                let Some(value) = register.read_local(cpu) else {
                    return Aarch64ExecutionExit::UnsupportedInstruction {
                        instruction,
                        word: ir.word,
                    };
                };
                staged.write(destination, value);
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::SystemWrite { source, register } => {
                if !register.write_local(&mut staged, cpu.register(source)) {
                    return Aarch64ExecutionExit::UnsupportedInstruction {
                        instruction,
                        word: ir.word,
                    };
                }
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::Nop => Aarch64ExecutionExit::Continue,
            Aarch64Instruction::SupervisorCall { immediate } => {
                return Aarch64ExecutionExit::Syscall { instruction, immediate };
            }
            Aarch64Instruction::Breakpoint { immediate } => {
                return Aarch64ExecutionExit::Breakpoint { instruction, immediate };
            }
            Aarch64Instruction::Undefined => {
                return Aarch64ExecutionExit::UndefinedInstruction {
                    instruction,
                    word: ir.word,
                };
            }
            _ => {
                return Aarch64ExecutionExit::UnsupportedInstruction {
                    instruction,
                    word: ir.word,
                };
            }
        };
        if !matches!(exit, Aarch64ExecutionExit::AlignmentFault { .. }) {
            cpu.commit_scalar(&staged);
        }
        exit
    }
}
