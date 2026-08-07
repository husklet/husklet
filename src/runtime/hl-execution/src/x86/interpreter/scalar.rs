use super::ScalarInterpreter;
use crate::{
    AccessKind, AluOperation, Arithmetic, CpuState, Division, ExecutionExit, Flag, FlagUpdate, GuestOperandMemory,
    Multiplication, ScalarInstruction, ScalarIr, ScalarOperand, ScalarRegister, ScalarState, ScalarWidth, ShiftCount,
    ShiftOperation, Staged, UnaryOperation,
};

impl ScalarInterpreter {
    /// Stages the instructions that only reach the scalar half of the state.
    /// `Ok(None)` means the instruction needs the SIMD or x87 half and must go
    /// through the full-state `stage`.
    pub(super) fn stage_scalar<M: GuestOperandMemory>(
        cpu: &CpuState,
        staged: &mut ScalarState,
        memory: &M,
        ir: ScalarIr,
        instruction: u64,
        next: u64,
    ) -> Result<Option<Staged<M::Reservation, M::BatchReservation>>, ExecutionExit> {
        let staged = match ir.instruction {
            ScalarInstruction::Move { destination, source } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                Self::write(staged, memory, destination, ir.width, value, next, instruction)
            }
            ScalarInstruction::EndianMove {
                register,
                address,
                store,
            } => Self::endian(
                staged,
                cpu,
                memory,
                register,
                address,
                store,
                ir.width,
                next,
                instruction,
            ),
            ScalarInstruction::Crc32c {
                destination,
                source,
                source_width,
            } => {
                let source = Self::read(cpu, memory, source, source_width, next, instruction)?;
                let mut crc = cpu.read_register(destination, ScalarWidth::Dword) as u32;
                for byte in 0..Self::bytes(source_width) {
                    crc ^= (source >> (u32::from(byte) * 8)) as u8 as u32;
                    for _ in 0..8 {
                        crc = (crc >> 1) ^ (0x82f6_3b78 & 0_u32.wrapping_sub(crc & 1));
                    }
                }
                staged.write_register(destination, ScalarWidth::Dword, u64::from(crc));
                Ok(Staged::Cpu)
            }
            ScalarInstruction::ByteSwap { register } => {
                let value = cpu.read_register(register, ir.width);
                staged.write_register(register, ir.width, Self::byte_swap(value, ir.width));
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Exchange {
                destination: ScalarOperand::Register(destination),
                source,
            } => {
                crate::x86::compare_exchange::CompareExchange::register(staged, cpu, destination, source, ir.width);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::AccumulatorExchange { source } => {
                crate::x86::compare_exchange::CompareExchange::register(
                    staged,
                    cpu,
                    ScalarRegister::General(0),
                    source,
                    ir.width,
                );
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Exchange {
                destination: ScalarOperand::Memory(_),
                ..
            } => unreachable!(),
            ScalarInstruction::Exchange {
                destination: ScalarOperand::Immediate(_),
                ..
            } => unreachable!(),
            ScalarInstruction::ExchangeAdd {
                destination,
                source,
                locked: false,
            } => crate::x86::compare_exchange::CompareExchange::add(
                staged,
                cpu,
                memory,
                destination,
                source,
                ir.width,
                next,
                instruction,
            ),
            ScalarInstruction::ExchangeAdd { locked: true, .. } => unreachable!(),
            ScalarInstruction::CompareExchange {
                destination,
                source,
                locked: false,
            } => {
                let accumulator = cpu.read_register(ScalarRegister::General(0), ir.width);
                let observed = Self::read(cpu, memory, destination, ir.width, next, instruction)?;
                let arithmetic = Arithmetic::sub(Self::integer_width(ir.width), accumulator, observed, false);
                staged.flags = staged.flags.apply(arithmetic.flags);
                let replacement = if accumulator == observed {
                    cpu.read_register(source, ir.width)
                } else {
                    staged.write_register(ScalarRegister::General(0), ir.width, observed);
                    observed
                };
                Self::write(staged, memory, destination, ir.width, replacement, next, instruction)
            }
            ScalarInstruction::CompareExchange { locked: true, .. } => unreachable!(),
            ScalarInstruction::WideCompareExchange { .. } => unreachable!(),
            ScalarInstruction::RotateRightNoFlags {
                destination,
                source,
                count,
            } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let result = match ir.width {
                    ScalarWidth::Dword => u64::from((value as u32).rotate_right(u32::from(count & 31))),
                    ScalarWidth::Qword => value.rotate_right(u32::from(count & 63)),
                    _ => unreachable!(),
                };
                staged.write_register(destination, ir.width, result);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::IsolateBit {
                operation,
                destination,
                source,
            } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let mask = if ir.width == ScalarWidth::Qword {
                    u64::MAX
                } else {
                    u64::from(u32::MAX)
                };
                let value = value & mask;
                let (result, carry) = match operation {
                    crate::x86::BitIsolation::Reset => (value.wrapping_sub(1) & value, value == 0),
                    crate::x86::BitIsolation::Mask => (value.wrapping_sub(1) ^ value, value == 0),
                    crate::x86::BitIsolation::Isolate => (value.wrapping_neg() & value, value != 0),
                };
                let result = result & mask;
                staged.write_register(destination, ir.width, result);
                staged.flags = staged
                    .flags
                    .with(Flag::Carry, carry)
                    .with(Flag::Zero, result == 0)
                    .with(
                        Flag::Sign,
                        result >> (if ir.width == ScalarWidth::Qword { 63 } else { 31 }) != 0,
                    )
                    .with(Flag::Overflow, false);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::AndNotGeneral {
                destination,
                first,
                second,
            } => {
                let left = cpu.read_register(first, ir.width);
                let right = Self::read(cpu, memory, second, ir.width, next, instruction)?;
                let mask = if ir.width == ScalarWidth::Qword {
                    u64::MAX
                } else {
                    u64::from(u32::MAX)
                };
                let result = !left & right & mask;
                staged.write_register(destination, ir.width, result);
                staged.flags = staged
                    .flags
                    .with(Flag::Carry, false)
                    .with(Flag::Zero, result == 0)
                    .with(
                        Flag::Sign,
                        result >> (if ir.width == ScalarWidth::Qword { 63 } else { 31 }) != 0,
                    )
                    .with(Flag::Overflow, false);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::ZeroHighBits {
                destination,
                source,
                index,
            } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let index = cpu.read_register(index, ScalarWidth::Qword) as u8;
                let bits = if ir.width == ScalarWidth::Qword { 64 } else { 32 };
                let out_of_range = u32::from(index) >= bits;
                let result = if out_of_range {
                    value
                } else if index == 0 {
                    0
                } else {
                    value & ((1_u64 << index) - 1)
                };
                let mask = if ir.width == ScalarWidth::Qword {
                    u64::MAX
                } else {
                    u64::from(u32::MAX)
                };
                let result = result & mask;
                staged.write_register(destination, ir.width, result);
                staged.flags = staged
                    .flags
                    .with(Flag::Carry, out_of_range)
                    .with(Flag::Zero, result == 0)
                    .with(Flag::Sign, result >> (bits - 1) != 0)
                    .with(Flag::Overflow, false);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VariableShift {
                operation,
                destination,
                source,
                count,
            } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let count = cpu.read_register(count, ScalarWidth::Qword) as u32
                    & if ir.width == ScalarWidth::Qword { 63 } else { 31 };
                let result = match (operation, ir.width) {
                    (crate::x86::VariableShift::Left, ScalarWidth::Dword) => u64::from((value as u32) << count),
                    (crate::x86::VariableShift::Left, ScalarWidth::Qword) => value << count,
                    (crate::x86::VariableShift::LogicalRight, ScalarWidth::Dword) => u64::from((value as u32) >> count),
                    (crate::x86::VariableShift::LogicalRight, ScalarWidth::Qword) => value >> count,
                    (crate::x86::VariableShift::ArithmeticRight, ScalarWidth::Dword) => {
                        u64::from(((value as u32) as i32 >> count) as u32)
                    }
                    (crate::x86::VariableShift::ArithmeticRight, ScalarWidth::Qword) => {
                        ((value as i64) >> count) as u64
                    }
                    _ => unreachable!(),
                };
                staged.write_register(destination, ir.width, result);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::MultiplyExtended { high, low, source } => {
                let right = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let left = cpu.registers[2];
                let (low_value, high_value) = if ir.width == ScalarWidth::Qword {
                    let product = u128::from(left) * u128::from(right);
                    (product as u64, (product >> 64) as u64)
                } else {
                    let product = u64::from(left as u32) * u64::from(right as u32);
                    (u64::from(product as u32), product >> 32)
                };
                staged.write_register(low, ir.width, low_value);
                staged.write_register(high, ir.width, high_value);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::TransferBits {
                destination,
                source,
                mask,
                deposit,
            } => {
                let source = cpu.read_register(source, ir.width);
                let mask = Self::read(cpu, memory, mask, ir.width, next, instruction)?;
                let mut result = 0_u64;
                let mut source_bit = 1_u64;
                let mut remaining = mask;
                while remaining != 0 {
                    let target_bit = remaining & remaining.wrapping_neg();
                    if source & source_bit != 0 {
                        result |= if deposit { target_bit } else { source_bit };
                    }
                    source_bit <<= 1;
                    remaining &= remaining - 1;
                }
                if !deposit {
                    result = 0;
                    let mut output = 1_u64;
                    let mut remaining = mask;
                    while remaining != 0 {
                        let selected = remaining & remaining.wrapping_neg();
                        if source & selected != 0 {
                            result |= output;
                        }
                        output <<= 1;
                        remaining &= remaining - 1;
                    }
                }
                staged.write_register(destination, ir.width, result);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::AccumulatorSignExtend { source_width } => {
                let value = cpu.read_register(ScalarRegister::General(0), source_width);
                let extended = match source_width {
                    ScalarWidth::Byte => (value as i8) as i64 as u64,
                    ScalarWidth::Word => (value as i16) as i64 as u64,
                    ScalarWidth::Dword => (value as i32) as i64 as u64,
                    ScalarWidth::Qword => unreachable!(),
                };
                staged.write_register(ScalarRegister::General(0), ir.width, extended);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::AccumulatorHighExtend => {
                let value = cpu.read_register(ScalarRegister::General(0), ir.width);
                let sign = Self::mask(ir.width) ^ (Self::mask(ir.width) >> 1);
                let high = if value & sign == 0 { 0 } else { Self::mask(ir.width) };
                staged.write_register(ScalarRegister::General(2), ir.width, high);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::MoveSignExtend {
                destination,
                source,
                source_width,
            } => {
                let value = Self::read(cpu, memory, source, source_width, next, instruction)?;
                let Some(extended) = crate::x86::extension::Extension::sign(value, source_width) else {
                    return Ok(Some(Staged::Exit(ExecutionExit::UndefinedInstruction { instruction })));
                };
                staged.write_register(destination, ir.width, extended);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::MoveZeroExtend {
                destination,
                source,
                source_width,
            } => {
                let value = Self::read(cpu, memory, source, source_width, next, instruction)?;
                staged.write_register(destination, ir.width, value);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::BitScan {
                operation,
                destination,
                source,
            } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                staged.apply_bit_scan(operation, destination, ir.width, value);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::PopulationCount { destination, source } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let mask = match ir.width {
                    ScalarWidth::Word => 0xffff,
                    ScalarWidth::Dword => 0xffff_ffff,
                    ScalarWidth::Qword => u64::MAX,
                    ScalarWidth::Byte => unreachable!(),
                };
                staged.write_register(destination, ir.width, (value & mask).count_ones().into());
                staged.flags = crate::FlagState::default().with(crate::Flag::Zero, value & mask == 0);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Xlat { address_32, segment } => {
                let mut address = cpu.registers[3].wrapping_add(cpu.registers[0] & 0xff);
                if address_32 {
                    address &= u64::from(u32::MAX);
                }
                address = address.wrapping_add(match segment {
                    Some(crate::Segment::Fs) => cpu.fs_base,
                    Some(crate::Segment::Gs) => cpu.gs_base,
                    None => 0,
                });
                let value = Self::memory_read(memory, address, 1, instruction)?;
                staged.write_register(
                    ScalarRegister::Byte(crate::ByteRegister::Low(0)),
                    ScalarWidth::Byte,
                    value,
                );
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Lea {
                destination,
                mut address,
            } => {
                address.segment = None;
                let value = address.resolve(&cpu.registers, next, 0, 0);
                staged.write_register(destination, ir.width, value);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Push { source } => {
                let value = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let bytes = Self::bytes(ir.width);
                staged.registers[4] = staged.registers[4].wrapping_sub(u64::from(bytes));
                let stack = Self::stack(staged.registers[4]);
                Self::write(
                    staged,
                    memory,
                    ScalarOperand::Memory(stack),
                    ir.width,
                    value,
                    next,
                    instruction,
                )
            }
            ScalarInstruction::Pop { destination } => {
                let bytes = Self::bytes(ir.width);
                let value = Self::memory_read(memory, cpu.registers[4], bytes, instruction)?;
                staged.registers[4] = staged.registers[4].wrapping_add(u64::from(bytes));
                Self::write(staged, memory, destination, ir.width, value, next, instruction)
            }
            ScalarInstruction::PushFlags => Self::push_flags(staged, cpu, memory, ir.width, next, instruction),
            ScalarInstruction::PopFlags => Self::pop_flags(staged, cpu, memory, ir.width, instruction),
            ScalarInstruction::ReadSelector { destination, value } => Self::write(
                staged,
                memory,
                destination,
                ir.width,
                u64::from(value),
                next,
                instruction,
            ),
            ScalarInstruction::WriteSelector => Ok(Staged::Cpu),
            ScalarInstruction::Iret => Self::iret(staged, cpu, memory, instruction),
            ScalarInstruction::FlagControl(operation) => {
                Self::control(staged, cpu, operation);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Alu {
                operation,
                destination,
                source,
                locked: false,
            } => {
                let left = Self::read(cpu, memory, destination, ir.width, next, instruction)?;
                let right = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let arithmetic = crate::x86::compare_exchange::CompareExchange::arithmetic(
                    operation, ir.width, left, right, cpu.flags,
                );
                staged.flags = staged.flags.apply(arithmetic.flags);
                if matches!(operation, AluOperation::Compare | AluOperation::Test) {
                    Ok(Staged::Cpu)
                } else {
                    Self::write(
                        staged,
                        memory,
                        destination,
                        ir.width,
                        arithmetic.result,
                        next,
                        instruction,
                    )
                }
            }
            ScalarInstruction::Alu { locked: true, .. } => unreachable!(),
            ScalarInstruction::Shift {
                operation,
                destination,
                count,
            } => {
                let value = Self::read(cpu, memory, destination, ir.width, next, instruction)?;
                let count = match count {
                    ShiftCount::One => 1,
                    ShiftCount::Immediate(value) => value,
                    ShiftCount::Cl => cpu.registers[1] as u8,
                };
                let arithmetic = match operation {
                    ShiftOperation::RotateLeft => Arithmetic::rotate_left(Self::integer_width(ir.width), value, count),
                    ShiftOperation::RotateRight => {
                        Arithmetic::rotate_right(Self::integer_width(ir.width), value, count)
                    }
                    ShiftOperation::CarryLeft => Arithmetic::rotate_carry_left(
                        Self::integer_width(ir.width),
                        value,
                        count,
                        cpu.flags.contains(Flag::Carry),
                    ),
                    ShiftOperation::CarryRight => Arithmetic::rotate_carry_right(
                        Self::integer_width(ir.width),
                        value,
                        count,
                        cpu.flags.contains(Flag::Carry),
                    ),
                    ShiftOperation::Left => Arithmetic::shift_left(Self::integer_width(ir.width), value, count),
                    ShiftOperation::Right => Arithmetic::shift_right(Self::integer_width(ir.width), value, count),
                    ShiftOperation::ArithmeticRight => {
                        Arithmetic::shift_arithmetic_right(Self::integer_width(ir.width), value, count)
                    }
                };
                staged.flags = staged.flags.apply(arithmetic.flags);
                Self::write(
                    staged,
                    memory,
                    destination,
                    ir.width,
                    arithmetic.result,
                    next,
                    instruction,
                )
            }
            ScalarInstruction::Unary { operation, operand } => {
                let value = Self::read(cpu, memory, operand, ir.width, next, instruction)?;
                let result = match operation {
                    UnaryOperation::Not => !value & Self::mask(ir.width),
                    UnaryOperation::Negate => {
                        let arithmetic = Arithmetic::sub(Self::integer_width(ir.width), 0, value, false);
                        staged.flags = staged.flags.apply(arithmetic.flags);
                        arithmetic.result
                    }
                };
                Self::write(staged, memory, operand, ir.width, result, next, instruction)
            }
            ScalarInstruction::Multiply { signed, source } => {
                let source = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let product = Multiplication::widening(Self::integer_width(ir.width), cpu.registers[0], source, signed);
                staged.write_product(ir.width, product);
                staged.flags = staged.flags.apply(product.flags);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::TruncatedMultiply {
                destination,
                source,
                multiplier,
            } => {
                let source = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let left = match multiplier {
                    Some(value) => value as u64,
                    None => cpu.read_register(destination, ir.width),
                };
                let product = Multiplication::truncating(Self::integer_width(ir.width), left, source);
                staged.write_register(destination, ir.width, product.low);
                let overflow = product.flags.values() != 0;
                staged.flags = staged.flags.apply(FlagUpdate::truncated_multiply(
                    Self::integer_width(ir.width),
                    product.low,
                    overflow,
                ));
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Divide { signed, divisor } => {
                let divisor = Self::read(cpu, memory, divisor, ir.width, next, instruction)?;
                let division = if signed {
                    Division::signed(
                        Self::integer_width(ir.width),
                        cpu.registers[2],
                        cpu.registers[0],
                        divisor,
                    )
                } else {
                    Division::unsigned(
                        Self::integer_width(ir.width),
                        cpu.registers[2],
                        cpu.registers[0],
                        divisor,
                    )
                };
                match division {
                    Ok(result) => {
                        staged.write_division(ir.width, result);
                        Ok(Staged::Cpu)
                    }
                    Err(error) => Ok(Staged::Exit(ExecutionExit::DivideError { instruction, error })),
                }
            }
            ScalarInstruction::Jump { target } => {
                Self::canonical(target, 1, instruction, AccessKind::Execute)?;
                staged.rip = target;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::JumpConditional { condition, target } => {
                if cpu.condition(condition) {
                    Self::canonical(target, 1, instruction, AccessKind::Execute)?;
                    staged.rip = target;
                }
                Ok(Staged::Cpu)
            }
            ScalarInstruction::CountBranch {
                target,
                address_32,
                decrement,
                zero,
            } => {
                let counter = if decrement {
                    cpu.registers[1].wrapping_sub(1)
                } else {
                    cpu.registers[1]
                };
                let counter = if address_32 {
                    counter & u64::from(u32::MAX)
                } else {
                    counter
                };
                if decrement {
                    staged.registers[1] = counter;
                }
                let condition = zero.is_none_or(|expected| cpu.flags.contains(Flag::Zero) == expected);
                if counter == 0 && !decrement || counter != 0 && decrement && condition {
                    Self::canonical(target, 1, instruction, AccessKind::Execute)?;
                    staged.rip = target;
                }
                Ok(Staged::Cpu)
            }
            ScalarInstruction::ConditionalMove {
                condition,
                destination,
                source,
            } => {
                let source = Self::read(cpu, memory, source, ir.width, next, instruction)?;
                let value = if cpu.condition(condition) {
                    source
                } else {
                    cpu.read_register(destination, ir.width)
                };
                staged.write_register(destination, ir.width, value);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::SetConditional { condition, destination } => {
                let value = u64::from(cpu.condition(condition));
                Self::write(staged, memory, destination, ScalarWidth::Byte, value, next, instruction)
            }
            ScalarInstruction::FlagsFromAh => {
                let ah = (cpu.registers[0] >> 8) as u8;
                staged.flags = staged
                    .flags
                    .with(Flag::Sign, ah & 0x80 != 0)
                    .with(Flag::Zero, ah & 0x40 != 0)
                    .with(Flag::Auxiliary, ah & 0x10 != 0)
                    .with(Flag::Parity, ah & 0x04 != 0)
                    .with(Flag::Carry, ah & 0x01 != 0);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::AhFromFlags => {
                let mut ah = 0x02_u64;
                for (flag, bit) in [
                    (Flag::Sign, 7),
                    (Flag::Zero, 6),
                    (Flag::Auxiliary, 4),
                    (Flag::Parity, 2),
                    (Flag::Carry, 0),
                ] {
                    ah |= u64::from(cpu.flags.contains(flag)) << bit;
                }
                staged.registers[0] = (staged.registers[0] & !0xff00) | (ah << 8);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::JumpIndirect { target } => {
                let target = Self::read(cpu, memory, target, ir.width, next, instruction)?;
                Self::canonical(target, 1, instruction, AccessKind::Execute)?;
                staged.rip = target;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Call { target } => Self::call(staged, memory, ir.width, next, target, instruction),
            ScalarInstruction::CallIndirect { target } => {
                let target = Self::read(cpu, memory, target, ScalarWidth::Qword, next, instruction)?;
                Self::call(staged, memory, ir.width, next, target, instruction)
            }
            ScalarInstruction::Return { pop_bytes } => {
                let bytes = Self::bytes(ir.width);
                let target = Self::memory_read(memory, cpu.registers[4], bytes, instruction)?;
                Self::canonical(target, 1, instruction, AccessKind::Execute)?;
                staged.registers[4] = staged.registers[4].wrapping_add(u64::from(bytes) + u64::from(pop_bytes));
                staged.rip = target;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::Leave { .. } => unreachable!(),
            ScalarInstruction::String(_) => unreachable!(),
            ScalarInstruction::Nop => Ok(Staged::Cpu),
            ScalarInstruction::Undefined => Ok(Staged::Exit(ExecutionExit::UndefinedInstruction { instruction })),
            ScalarInstruction::Breakpoint => Ok(Staged::Exit(ExecutionExit::Breakpoint { instruction, next })),
            ScalarInstruction::Cpuid => {
                crate::GuestFeaturePolicy::interpreter().apply(staged);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::TimestampCounter { auxiliary } => Ok(Staged::Exit(ExecutionExit::TimestampCounter {
                instruction,
                next,
                auxiliary,
            })),
            ScalarInstruction::Syscall => Ok(Staged::Exit(ExecutionExit::Syscall { instruction, next })),
            _ => return Ok(None),
        };
        staged.map(Some)
    }
}
