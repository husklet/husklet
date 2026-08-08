use crate::aarch64_integer_support::{
    add_carry, arithmetic, bitfield, logical, logical_flags, logical_operand, multiply, select_value, shifted,
    write_destination, write_register,
};
use crate::{
    Aarch64CpuState, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir, LogicalOperation, MoveWideOperation,
    PcCoordinatePort,
};

pub(crate) struct RegisterExecutor;

impl RegisterExecutor {
    pub(crate) const fn supports(instruction: &Aarch64Instruction) -> bool {
        matches!(
            *instruction,
            Aarch64Instruction::AddSubtractImmediate { .. }
                | Aarch64Instruction::AddSubtractShifted { .. }
                | Aarch64Instruction::AddCarry { .. }
                | Aarch64Instruction::LogicalImmediate { .. }
                | Aarch64Instruction::LogicalShifted { .. }
                | Aarch64Instruction::MoveWide { .. }
                | Aarch64Instruction::Bitfield { .. }
                | Aarch64Instruction::Multiply { .. }
                | Aarch64Instruction::ConditionalSelect { .. }
                | Aarch64Instruction::BranchImmediate { .. }
                | Aarch64Instruction::BranchConditional { .. }
                | Aarch64Instruction::CompareBranch { .. }
                | Aarch64Instruction::TestBranch { .. }
                | Aarch64Instruction::Nop
        )
    }

    /// Register-only instructions cannot fault after decode, so updating the
    /// CPU directly preserves the staged interpreter's failure atomicity.
    pub(crate) fn execute(
        cpu: &mut Aarch64CpuState,
        coordinates: &dyn PcCoordinatePort,
        ir: &Aarch64Ir,
    ) -> Option<Aarch64ExecutionExit> {
        let instruction = cpu.pc;
        if instruction & 3 != 0 {
            return Some(Aarch64ExecutionExit::AlignmentFault {
                instruction,
                target: instruction,
                access: crate::AccessKind::Execute,
            });
        }
        let next = instruction.wrapping_add(4);
        let exit = match ir.instruction {
            Aarch64Instruction::AddSubtractImmediate {
                subtract,
                set_flags,
                source,
                destination,
                immediate,
            } => {
                let left = cpu.register_or_sp(source);
                let value = arithmetic(cpu, ir.wide, left, immediate, subtract, set_flags);
                if set_flags {
                    write_register(cpu, ir.wide, destination, value);
                } else {
                    write_destination(cpu, ir.wide, destination, value);
                }
                cpu.pc = next;
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
                let left = cpu.register(source);
                let right = shifted(cpu.register(operand), shift, amount, ir.wide);
                let value = arithmetic(cpu, ir.wide, left, right, subtract, set_flags);
                write_register(cpu, ir.wide, destination, value);
                cpu.pc = next;
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::AddCarry {
                subtract,
                set_flags,
                left,
                right,
                destination,
            } => {
                let left = cpu.register(left);
                let right = cpu.register(right);
                let carry = cpu.nzcv.carry();
                let value = add_carry(cpu, ir.wide, left, right, carry, subtract, set_flags);
                write_register(cpu, ir.wide, destination, value);
                cpu.pc = next;
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::LogicalImmediate {
                operation,
                source,
                destination,
                mask,
            } => {
                let value = logical(operation, cpu.register(source), mask, ir.wide);
                if operation == LogicalOperation::Ands {
                    logical_flags(cpu, value, ir.wide);
                    write_register(cpu, ir.wide, destination, value);
                } else {
                    write_destination(cpu, ir.wide, destination, value);
                }
                cpu.pc = next;
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
                let value = logical(operation, cpu.register(source), right, ir.wide);
                if operation == LogicalOperation::Ands {
                    logical_flags(cpu, value, ir.wide);
                }
                write_register(cpu, ir.wide, destination, value);
                cpu.pc = next;
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
                write_register(cpu, ir.wide, destination, value);
                cpu.pc = next;
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
                write_register(cpu, ir.wide, destination, value);
                cpu.pc = next;
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
                write_register(cpu, ir.wide, destination, value);
                cpu.pc = next;
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::ConditionalSelect {
                source,
                alternate,
                destination,
                condition,
                invert,
                increment,
            } => {
                let value = select_value(cpu, source, alternate, condition.holds(cpu.nzcv), invert, increment);
                write_register(cpu, ir.wide, destination, value);
                cpu.pc = next;
                Aarch64ExecutionExit::Continue
            }
            Aarch64Instruction::BranchImmediate { displacement, link } => {
                let target = instruction.wrapping_add(displacement as u64);
                if target & 3 != 0 {
                    return Some(Self::alignment(instruction, target));
                }
                if link {
                    cpu.set_register(30, coordinates.architectural_pc(instruction).wrapping_add(4));
                }
                cpu.pc = target;
                Aarch64ExecutionExit::Branch { target }
            }
            Aarch64Instruction::BranchConditional {
                condition,
                displacement,
            } => {
                let target = if condition.holds(cpu.nzcv) {
                    instruction.wrapping_add(displacement as u64)
                } else {
                    next
                };
                if target & 3 != 0 {
                    return Some(Self::alignment(instruction, target));
                }
                cpu.pc = target;
                Aarch64ExecutionExit::Branch { target }
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
                let target = if (value != 0) == nonzero {
                    instruction.wrapping_add(displacement as u64)
                } else {
                    next
                };
                if target & 3 != 0 {
                    return Some(Self::alignment(instruction, target));
                }
                cpu.pc = target;
                Aarch64ExecutionExit::Branch { target }
            }
            Aarch64Instruction::TestBranch {
                source,
                bit,
                nonzero,
                displacement,
            } => {
                let target = if ((cpu.register(source) >> bit) & 1 != 0) == nonzero {
                    instruction.wrapping_add(displacement as u64)
                } else {
                    next
                };
                if target & 3 != 0 {
                    return Some(Self::alignment(instruction, target));
                }
                cpu.pc = target;
                Aarch64ExecutionExit::Branch { target }
            }
            Aarch64Instruction::Nop => {
                cpu.pc = next;
                Aarch64ExecutionExit::Continue
            }
            _ => return None,
        };
        Some(exit)
    }

    fn alignment(instruction: u64, target: u64) -> Aarch64ExecutionExit {
        Aarch64ExecutionExit::AlignmentFault {
            instruction,
            target,
            access: crate::AccessKind::Execute,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Aarch64Decoder, Aarch64Interpreter, Nzcv};

    struct Coordinates;

    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, execution_pc: u64) -> u64 {
            execution_pc
        }
    }

    fn registers(seed: u64) -> [u64; 31] {
        std::array::from_fn(|index| seed.rotate_left(index as u32))
    }

    #[test]
    fn carry_matches_staged() {
        let words = [
            0xcb03_0041,
            0xf100_0508,
            0xca01_00a5,
            0x5400_0340,
            0x9b0b_2800,
            0xab07_0002,
            0x9a9f_37e4,
            0xd341_fc01,
            0xab04_0021,
            0xd342_fc09,
            0xab01_0063,
            0x9a84_3484,
            0xd37f_f801,
            0xab01_0084,
            0xaa02_03e7,
            0x9a9f_37e1,
            0xab04_00c4,
            0x9a09_0021,
            0xca04_0049,
            0x8b01_00a5,
            0xaa04_03e6,
            0xca05_0061,
            0xeb01_013f,
            0x54ff_fd08,
            0xcb02_0063,
            0xf100_0508,
            0x8b03_0083,
            0x54ff_fd01,
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for word in words {
            let ir = Aarch64Decoder::decode(word).unwrap();
            assert!(RegisterExecutor::supports(&ir.instruction));
            for _ in 0..64 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let mut expected = Aarch64CpuState {
                    pc: 0x4005_44,
                    nzcv: Nzcv::from_bits((seed as u32) & Nzcv::MASK),
                    registers: registers(seed),
                    ..Aarch64CpuState::default()
                };
                let mut actual = expected.clone();
                let expected_exit = Aarch64Interpreter::execute(&mut expected, &Coordinates, &ir);
                let actual_exit = RegisterExecutor::execute(&mut actual, &Coordinates, &ir).unwrap();
                assert_eq!(actual_exit, expected_exit, "word={word:#010x}");
                assert_eq!(actual, expected, "word={word:#010x}");
            }
        }
    }
}
