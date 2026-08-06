use crate::{
    AccessKind, CpuState, DecodedInstruction, ExecutionExit, Flag, FloatWidth, GuestOperandMemory, ScalarInstruction,
    ScalarIrError, VectorSource,
};

pub(crate) struct Comparison;

enum Ordering {
    Less,
    Equal,
    Greater,
    Unordered,
}

impl Comparison {
    pub(crate) fn mask_decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let packed = !decoded.prefixes.rep && !decoded.prefixes.repne;
        Ok(ScalarInstruction::VectorFloatCompare {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: super::Decoder::vector_source(decoded)?,
            format: if packed {
                if decoded.prefixes.operand_16 {
                    FloatWidth::Double
                } else {
                    FloatWidth::Single
                }
            } else {
                super::Decoder::float_format(decoded)?
            },
            packed,
            predicate: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8 & 7,
        })
    }

    pub(crate) fn mask_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        packed: bool,
        predicate: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = if packed {
            16
        } else {
            match format {
                FloatWidth::Single => 4,
                FloatWidth::Double => 8,
            }
        };
        let source = if bytes == 16 {
            match crate::x86::VectorLane::read(cpu, memory, source, next, instruction) {
                Ok(value) => value,
                Err(exit) => return exit,
            }
        } else {
            match Self::read(cpu, memory, source, bytes, instruction, next) {
                Ok(value) => u128::from(value),
                Err(exit) => return exit,
            }
        };
        let bits = match format {
            FloatWidth::Single => 32,
            FloatWidth::Double => 64,
        };
        let lanes = if packed { 128 / bits } else { 1 };
        let daz = cpu.mxcsr & (1 << 6) != 0;
        let mut staged = cpu.clone();
        staged.rip = next;
        for lane in 0..lanes {
            let shift = lane * bits;
            let left = Self::lane_at(cpu.vectors[usize::from(destination)], format, shift);
            let right = Self::lane_at(source, format, shift);
            if Self::signaling_nan(left, format)
                || Self::signaling_nan(right, format)
                || matches!(predicate & 7, 1 | 2 | 5 | 6) && (Self::nan(left, format) || Self::nan(right, format))
            {
                staged.mxcsr |= 1;
            }
            if !daz && (Self::denormal(left, format) || Self::denormal(right, format)) {
                staged.mxcsr |= 1 << 1;
            }
            let ordering = Self::compare(
                Self::normalize(left, format, daz),
                Self::normalize(right, format, daz),
                format,
            );
            let selected = match predicate & 7 {
                0 => matches!(ordering, Ordering::Equal),
                1 => matches!(ordering, Ordering::Less),
                2 => matches!(ordering, Ordering::Less | Ordering::Equal),
                3 => matches!(ordering, Ordering::Unordered),
                4 => !matches!(ordering, Ordering::Equal),
                5 => !matches!(ordering, Ordering::Less),
                6 => !matches!(ordering, Ordering::Less | Ordering::Equal),
                _ => !matches!(ordering, Ordering::Unordered),
            };
            let value = if selected {
                match format {
                    FloatWidth::Single => u32::MAX as u64,
                    FloatWidth::Double => u64::MAX,
                }
            } else {
                0
            };
            staged.vectors[usize::from(destination)] =
                Self::merge_at(staged.vectors[usize::from(destination)], value, format, shift);
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let right = if let Some(register) = decoded.register_operand {
            VectorSource::Register(register)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorScalarCompare {
            left: decoded.register.ok_or(ScalarIrError::Invalid)?,
            right,
            format: if decoded.prefixes.operand_16 {
                FloatWidth::Double
            } else {
                FloatWidth::Single
            },
            signaling_only: decoded.opcode == 0x2e,
        })
    }

    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        left: u8,
        right: VectorSource,
        format: FloatWidth,
        signaling_only: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes: u8 = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        let right = match right {
            VectorSource::Register(index) => Self::lane(cpu.vectors[usize::from(index)], format),
            VectorSource::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                if !Self::canonical(address) || !Self::canonical(address.wrapping_add(u64::from(bytes - 1))) {
                    return ExecutionExit::NonCanonical {
                        instruction,
                        address,
                        access: AccessKind::Read,
                    };
                }
                match memory.read(address, bytes) {
                    Ok(value) => value,
                    Err(()) => {
                        return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                            instruction,
                            address,
                            AccessKind::Read,
                            u64::from(bytes),
                        ));
                    }
                }
            }
        };
        let left = Self::lane(cpu.vectors[usize::from(left)], format);
        let daz = cpu.mxcsr & (1 << 6) != 0;
        let left = Self::normalize(left, format, daz);
        let right = Self::normalize(right, format, daz);
        let (zero, parity, carry) = match Self::compare(left, right, format) {
            Ordering::Less => (false, false, true),
            Ordering::Equal => (true, false, false),
            Ordering::Greater => (false, false, false),
            Ordering::Unordered => (true, true, true),
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged
            .flags
            .with(Flag::Zero, zero)
            .with(Flag::Parity, parity)
            .with(Flag::Carry, carry)
            .with(Flag::Overflow, false)
            .with(Flag::Sign, false)
            .with(Flag::Auxiliary, false);
        let invalid = if signaling_only {
            Self::signaling_nan(left, format) || Self::signaling_nan(right, format)
        } else {
            Self::nan(left, format) || Self::nan(right, format)
        };
        if invalid {
            staged.mxcsr |= 1;
        }
        if !daz && (Self::denormal(left, format) || Self::denormal(right, format)) {
            staged.mxcsr |= 1 << 1;
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn compare(left: u64, right: u64, format: FloatWidth) -> Ordering {
        if Self::nan(left, format) || Self::nan(right, format) {
            return Ordering::Unordered;
        }
        let sign = Self::sign(format);
        if left << 1 == 0 && right << 1 == 0 || left == right {
            return Ordering::Equal;
        }
        let left_negative = left & sign != 0;
        let right_negative = right & sign != 0;
        if left_negative != right_negative {
            return if left_negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let less = if left_negative { left > right } else { left < right };
        if less { Ordering::Less } else { Ordering::Greater }
    }

    fn read<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        bytes: u8,
        instruction: u64,
        next: u64,
    ) -> Result<u64, ExecutionExit> {
        let VectorSource::Memory(effective) = source else {
            let VectorSource::Register(index) = source else {
                unreachable!()
            };
            return Ok(cpu.vectors[usize::from(index)] as u64);
        };
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(u64::from(bytes - 1))) {
            return Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            });
        }
        memory.read(address, bytes).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Read,
                u64::from(bytes),
            ))
        })
    }

    const fn lane_at(vector: u128, format: FloatWidth, shift: u32) -> u64 {
        match format {
            FloatWidth::Single => (vector >> shift) as u32 as u64,
            FloatWidth::Double => (vector >> shift) as u64,
        }
    }

    const fn merge_at(vector: u128, value: u64, format: FloatWidth, shift: u32) -> u128 {
        let mask = match format {
            FloatWidth::Single => u32::MAX as u128,
            FloatWidth::Double => u64::MAX as u128,
        } << shift;
        vector & !mask | ((value as u128) << shift) & mask
    }

    const fn lane(vector: u128, format: FloatWidth) -> u64 {
        match format {
            FloatWidth::Single => vector as u32 as u64,
            FloatWidth::Double => vector as u64,
        }
    }
    const fn normalize(bits: u64, format: FloatWidth, daz: bool) -> u64 {
        if daz && Self::denormal(bits, format) {
            bits & Self::sign(format)
        } else {
            bits
        }
    }
    const fn nan(bits: u64, format: FloatWidth) -> bool {
        match format {
            FloatWidth::Single => bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0,
            FloatWidth::Double => {
                bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
            }
        }
    }
    const fn signaling_nan(bits: u64, format: FloatWidth) -> bool {
        Self::nan(bits, format)
            && bits
                & match format {
                    FloatWidth::Single => 1 << 22,
                    FloatWidth::Double => 1_u64 << 51,
                }
                == 0
    }
    const fn denormal(bits: u64, format: FloatWidth) -> bool {
        match format {
            FloatWidth::Single => bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0,
            FloatWidth::Double => bits & 0x7ff0_0000_0000_0000 == 0 && bits & 0x000f_ffff_ffff_ffff != 0,
        }
    }
    const fn sign(format: FloatWidth) -> u64 {
        match format {
            FloatWidth::Single => 1 << 31,
            FloatWidth::Double => 1_u64 << 63,
        }
    }
    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
}
