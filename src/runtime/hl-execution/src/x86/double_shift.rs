use crate::{
    AccessKind, Arithmetic, CpuState, DecodedInstruction, ExecutionExit, GuestOperandMemory, IntegerWidth,
    ScalarInstruction, ScalarIrError, ScalarOperand, ScalarRegister, ScalarWidth, ShiftCount,
};

pub(crate) struct DoubleShift;

impl DoubleShift {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let count = if matches!(decoded.opcode, 0xa4 | 0xac) {
            ShiftCount::Immediate(decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8)
        } else {
            ShiftCount::Cl
        };
        Ok(ScalarInstruction::DoubleShift {
            destination: crate::x86::scalar::Decoder::rm(decoded, false)?,
            source: crate::x86::scalar::Decoder::general_reg(decoded)?,
            right: decoded.opcode >= 0xac,
            count,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: ScalarOperand,
        source: ScalarRegister,
        right: bool,
        count: ShiftCount,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let count = match count {
            ShiftCount::Cl => cpu.registers[1] as u8,
            ShiftCount::Immediate(value) => value,
            ShiftCount::One => unreachable!(),
        };
        let fill = cpu.read_register(source, width);
        let (value, address) = match destination {
            ScalarOperand::Register(register) => (cpu.read_register(register, width), None),
            ScalarOperand::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                let bytes = Self::bytes(width);
                if !Self::valid(address, bytes) {
                    return ExecutionExit::NonCanonical {
                        instruction,
                        address,
                        access: AccessKind::Read,
                    };
                }
                let value = match memory.read(address, bytes) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, address, AccessKind::Read, bytes),
                };
                (value, Some(address))
            }
            ScalarOperand::Immediate(_) => unreachable!(),
        };
        let arithmetic = if right {
            Arithmetic::shift_right_double(Self::integer(width), value, fill, count)
        } else {
            Arithmetic::shift_left_double(Self::integer(width), value, fill, count)
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged.flags.apply(arithmetic.flags);
        match (destination, address) {
            (ScalarOperand::Register(register), None) => {
                staged.write_register(register, width, arithmetic.result);
                *cpu = staged;
                ExecutionExit::Continue
            }
            (ScalarOperand::Memory(_), Some(address)) => {
                if arithmetic.flags.preserved(crate::Flag::Carry) {
                    *cpu = staged;
                    return ExecutionExit::Continue;
                }
                let bytes = Self::bytes(width);
                let reservation = match memory.reserve_write(address, bytes) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, address, AccessKind::Write, bytes),
                };
                if memory.commit_write(reservation, arithmetic.result).is_err() {
                    return Self::fault(instruction, address, AccessKind::Write, bytes);
                }
                *cpu = staged;
                ExecutionExit::Continue
            }
            _ => unreachable!(),
        }
    }

    const fn bytes(width: ScalarWidth) -> u8 {
        match width {
            ScalarWidth::Byte => 1,
            ScalarWidth::Word => 2,
            ScalarWidth::Dword => 4,
            ScalarWidth::Qword => 8,
        }
    }
    const fn integer(width: ScalarWidth) -> IntegerWidth {
        match width {
            ScalarWidth::Byte => IntegerWidth::Byte,
            ScalarWidth::Word => IntegerWidth::Word,
            ScalarWidth::Dword => IntegerWidth::Dword,
            ScalarWidth::Qword => IntegerWidth::Qword,
        }
    }
    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
    fn valid(address: u64, bytes: u8) -> bool {
        address.checked_add(u64::from(bytes) - 1).is_some_and(Self::canonical) && Self::canonical(address)
    }
    fn fault(instruction: u64, address: u64, access: AccessKind, length: u8) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(
            instruction,
            address,
            access,
            u64::from(length),
        ))
    }
}
