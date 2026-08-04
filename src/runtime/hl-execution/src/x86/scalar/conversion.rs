use crate::{
    AccessKind, CpuState, DecodedInstruction, ExecutionExit, FloatWidth, GuestOperandMemory, ScalarInstruction,
    ScalarIrError, ScalarOperand, ScalarRegister, ScalarWidth, VectorSource,
};
use hl_softfloat::{ExceptionFlags, RoundingMode, Value};

use super::arithmetic::Arithmetic;

pub(crate) struct Conversion;

impl Conversion {
    pub(crate) fn mmx_to_float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        double: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let raw = match source {
            VectorSource::Register(source) => cpu.read_mmx(source),
            VectorSource::Memory(address) => {
                let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                match memory.read(address, 8) {
                    Ok(value) => value,
                    Err(()) => return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction, address, AccessKind::Read, 8,
                    )),
                }
            }
        };
        let environment = Arithmetic::environment(cpu.mxcsr);
        let format = if double { FloatWidth::Double } else { FloatWidth::Single };
        let mut output = if double { 0 } else { cpu.vectors[usize::from(destination)] & (u128::MAX << 64) };
        let mut exceptions = 0_u32;
        for lane in 0..2 {
            let signed = i64::from((raw >> (lane * 32)) as u32 as i32);
            let result = environment.from_signed(Arithmetic::soft_format(format), signed);
            let shift = lane * if double { 64 } else { 32 };
            output |= u128::from(result.value.bits()) << shift;
            exceptions |= Arithmetic::exceptions(result.flags);
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.vectors[usize::from(destination)] = output;
        staged.mxcsr |= exceptions;
        *cpu = staged;
        ExecutionExit::Continue
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mmx_from_float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        double: bool,
        truncate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = if double { 16_u8 } else { 8 };
        let raw = match source {
            VectorSource::Register(source) => cpu.vectors[usize::from(source)],
            VectorSource::Memory(address) => {
                let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                let low = match memory.read(address, 8) {
                    Ok(value) => value,
                    Err(()) => return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction, address, AccessKind::Read, u64::from(bytes),
                    )),
                };
                let high = if double {
                    match memory.read(address + 8, 8) {
                        Ok(value) => value,
                        Err(()) => return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                            instruction, address + 8, AccessKind::Read, 16,
                        )),
                    }
                } else {
                    0
                };
                u128::from(low) | (u128::from(high) << 64)
            }
        };
        let format = if double { FloatWidth::Double } else { FloatWidth::Single };
        let lane_bits = if double { 64 } else { 32 };
        let mut environment = Arithmetic::environment(cpu.mxcsr);
        if truncate {
            environment.rounding = RoundingMode::TowardZero;
        }
        let mut output = 0_u64;
        let mut exceptions = 0_u32;
        for lane in 0..2 {
            let bits = (raw >> (lane * lane_bits)) as u64
                & if double { u64::MAX } else { u64::from(u32::MAX) };
            let result = environment.to_signed(Value::from_bits(Arithmetic::soft_format(format), bits), 32);
            let invalid = result.flags.contains(ExceptionFlags::INVALID);
            let value = if invalid { 0x8000_0000 } else { result.value as u32 };
            output |= u64::from(value) << (lane * 32);
            let raised = Arithmetic::exceptions(result.flags) & !(1 << 1);
            exceptions |= raised;
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.write_mmx(destination, output);
        staged.mxcsr |= exceptions;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn packed_double(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 && !decoded.prefixes.rep && !decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::ConvertPackedDouble {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: super::Decoder::vector_source(decoded)?,
            from_integer: decoded.prefixes.rep,
            truncate: decoded.prefixes.operand_16,
        })
    }

    pub(crate) fn packed_single(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::ConvertPackedSingle {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: super::Decoder::vector_source(decoded)?,
            to_integer: decoded.prefixes.operand_16 || decoded.prefixes.rep,
            truncate: decoded.prefixes.rep,
        })
    }

    pub(crate) fn from_integer(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        Ok(ScalarInstruction::ConvertIntegerFloat {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: super::Decoder::rm(decoded, false)?,
            wide: decoded.rex().is_some_and(|rex| rex.w),
            format: super::Decoder::float_format(decoded)?,
            merge: None,
        })
    }

    pub(crate) fn to_integer(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        Ok(ScalarInstruction::ConvertFloatInteger {
            destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
            source: super::Decoder::vector_source(decoded)?,
            wide: decoded.rex().is_some_and(|rex| rex.w),
            format: super::Decoder::float_format(decoded)?,
            truncate: decoded.opcode == 0x2c,
        })
    }

    pub(crate) fn width(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let destination_format = if decoded.prefixes.rep || (!decoded.prefixes.repne && !decoded.prefixes.operand_16) {
            FloatWidth::Double
        } else {
            FloatWidth::Single
        };
        Ok(ScalarInstruction::ConvertFloatWidth {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: super::Decoder::vector_source(decoded)?,
            destination_format,
            packed: !decoded.prefixes.rep && !decoded.prefixes.repne,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_integer_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: ScalarOperand,
        wide: bool,
        format: FloatWidth,
        merge: Option<u8>,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let width = if wide { ScalarWidth::Qword } else { ScalarWidth::Dword };
        let bits = match Self::integer(cpu, memory, source, width, instruction, next) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let signed = if wide {
            bits as i64
        } else {
            i64::from(bits as u32 as i32)
        };
        let result = Arithmetic::environment(cpu.mxcsr).from_signed(Arithmetic::soft_format(format), signed);
        let mut staged = cpu.clone();
        staged.rip = next;
        let base = merge.map_or(staged.vectors[usize::from(destination)], |register| {
            cpu.vectors[usize::from(register)]
        });
        staged.vectors[usize::from(destination)] = Self::merge(base, result.value.bits(), format);
        if merge.is_some() {
            staged.vector_upper[usize::from(destination)] = 0;
        }
        staged.mxcsr |= Arithmetic::exceptions(result.flags);
        *cpu = staged;
        ExecutionExit::Continue
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn to_integer_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: ScalarRegister,
        source: VectorSource,
        wide: bool,
        format: FloatWidth,
        truncate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bits = match Self::float(cpu, memory, source, format, instruction, next) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let mut environment = Arithmetic::environment(cpu.mxcsr);
        if truncate {
            environment.rounding = RoundingMode::TowardZero;
        }
        let width = if wide { 64 } else { 32 };
        let result = environment.to_signed(Value::from_bits(Arithmetic::soft_format(format), bits), width);
        let invalid = result.flags.contains(ExceptionFlags::INVALID);
        let value = if invalid { 1_u64 << (width - 1) } else { result.value };
        // Unlike SSE arithmetic and comparisons, float-to-integer converts
        // never report the denormal-operand exception. They report precision
        // for an in-range subnormal that rounds to zero, or invalid alone for
        // an out-of-range/NaN result.
        let exceptions = Arithmetic::exceptions(result.flags) & !(1 << 1);
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.mxcsr |= exceptions;
        staged.write_register(
            destination,
            if wide { ScalarWidth::Qword } else { ScalarWidth::Dword },
            value,
        );
        *cpu = staged;
        ExecutionExit::Continue
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn width_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        destination_format: FloatWidth,
        packed: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let source_format = match destination_format {
            FloatWidth::Single => FloatWidth::Double,
            FloatWidth::Double => FloatWidth::Single,
        };
        if packed {
            return Self::width_packed_execute(
                cpu,
                memory,
                destination,
                source,
                source_format,
                destination_format,
                instruction,
                next,
            );
        }
        let bits = match Self::float(cpu, memory, source, source_format, instruction, next) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let result = Arithmetic::environment(cpu.mxcsr).convert(
            Value::from_bits(Arithmetic::soft_format(source_format), bits),
            Arithmetic::soft_format(destination_format),
        );
        let value = Self::converted_nan(result.value.bits(), bits, source_format, destination_format);
        let mut exceptions = Arithmetic::exceptions(result.flags);
        if cpu.mxcsr & (1 << 6) != 0 {
            exceptions &= !(1 << 1);
        } else if Arithmetic::denormal(bits, source_format) {
            exceptions |= 1 << 1;
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.mxcsr |= exceptions;
        staged.vectors[usize::from(destination)] =
            Self::merge(staged.vectors[usize::from(destination)], value, destination_format);
        *cpu = staged;
        ExecutionExit::Continue
    }

    #[allow(clippy::too_many_arguments)]
    fn width_packed_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        source_format: FloatWidth,
        destination_format: FloatWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let source = match source_format {
            FloatWidth::Double => match crate::x86::VectorLane::read(cpu, memory, source, next, instruction) {
                Ok(value) => value,
                Err(exit) => return exit,
            },
            FloatWidth::Single => match Self::packed_width_source(cpu, memory, source, instruction, next) {
                Ok(value) => value,
                Err(exit) => return exit,
            },
        };
        let source_bits = match source_format {
            FloatWidth::Single => 32,
            FloatWidth::Double => 64,
        };
        let destination_bits = match destination_format {
            FloatWidth::Single => 32,
            FloatWidth::Double => 64,
        };
        let mut output = 0_u128;
        let mut exceptions = 0_u32;
        for lane in 0..2 {
            let bits = ((source >> (lane * source_bits)) & ((1_u128 << source_bits) - 1)) as u64;
            let result = Arithmetic::environment(cpu.mxcsr).convert(
                Value::from_bits(Arithmetic::soft_format(source_format), bits),
                Arithmetic::soft_format(destination_format),
            );
            let value = Self::converted_nan(result.value.bits(), bits, source_format, destination_format);
            exceptions |= Arithmetic::exceptions(result.flags);
            if cpu.mxcsr & (1 << 6) != 0 {
                exceptions &= !(1 << 1);
            } else if Arithmetic::denormal(bits, source_format) {
                exceptions |= 1 << 1;
            }
            output |= u128::from(value) << (lane * destination_bits);
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.mxcsr |= exceptions;
        staged.vectors[usize::from(destination)] = output;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn packed_width_source<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        instruction: u64,
        next: u64,
    ) -> Result<u128, ExecutionExit> {
        let VectorSource::Memory(effective) = source else {
            let VectorSource::Register(register) = source else {
                unreachable!()
            };
            return Ok(cpu.vectors[usize::from(register)]);
        };
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(7)) {
            return Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            });
        }
        memory.read(address, 8).map(u128::from).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, AccessKind::Read, 8))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn packed_single_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        to_integer: bool,
        truncate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let source = match crate::x86::VectorLane::read(cpu, memory, source, next, instruction) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let mut environment = Arithmetic::environment(cpu.mxcsr);
        if truncate {
            environment.rounding = RoundingMode::TowardZero;
        }
        let mut output = 0_u128;
        let mut exceptions = 0;
        for lane in 0..4 {
            let bits = (source >> (lane * 32)) as u32;
            let value = if to_integer {
                let result = environment.to_signed(
                    Value::from_bits(Arithmetic::soft_format(FloatWidth::Single), u64::from(bits)),
                    32,
                );
                exceptions |= Arithmetic::exceptions(result.flags) & !(1 << 1);
                if result.flags.contains(ExceptionFlags::INVALID) {
                    0x8000_0000
                } else {
                    result.value as u32
                }
            } else {
                let result = environment.from_signed(hl_softfloat::Format::Binary32, i64::from(bits as i32));
                exceptions |= Arithmetic::exceptions(result.flags);
                result.value.bits() as u32
            };
            output |= u128::from(value) << (lane * 32);
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.mxcsr |= exceptions;
        staged.vectors[usize::from(destination)] = output;
        *cpu = staged;
        ExecutionExit::Continue
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn packed_double_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        from_integer: bool,
        truncate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let source = if from_integer {
            match Self::packed_width_source(cpu, memory, source, instruction, next) {
                Ok(value) => value,
                Err(exit) => return exit,
            }
        } else {
            match crate::x86::VectorLane::read(cpu, memory, source, next, instruction) {
                Ok(value) => value,
                Err(exit) => return exit,
            }
        };
        let mut environment = Arithmetic::environment(cpu.mxcsr);
        if truncate {
            environment.rounding = RoundingMode::TowardZero;
        }
        let mut output = 0_u128;
        let mut exceptions = 0_u32;
        for lane in 0..2 {
            if from_integer {
                let value = (source >> (lane * 32)) as u32 as i32;
                let result = environment.from_signed(Arithmetic::soft_format(FloatWidth::Double), i64::from(value));
                output |= u128::from(result.value.bits()) << (lane * 64);
                exceptions |= Arithmetic::exceptions(result.flags);
            } else {
                let bits = (source >> (lane * 64)) as u64;
                let result =
                    environment.to_signed(Value::from_bits(Arithmetic::soft_format(FloatWidth::Double), bits), 32);
                let value = if result.flags.contains(ExceptionFlags::INVALID) {
                    0x8000_0000
                } else {
                    result.value as u32
                };
                output |= u128::from(value) << (lane * 32);
                exceptions |= Arithmetic::exceptions(result.flags) & !(1 << 1);
            }
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.mxcsr |= exceptions;
        staged.vectors[usize::from(destination)] = output;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn integer<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: ScalarOperand,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> Result<u64, ExecutionExit> {
        match source {
            ScalarOperand::Register(register) => Ok(cpu.read_register(register, width)),
            ScalarOperand::Memory(effective) => {
                let bytes = if width == ScalarWidth::Qword { 8 } else { 4 };
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                Self::memory(memory, address, bytes, instruction)
            }
            ScalarOperand::Immediate(_) => unreachable!(),
        }
    }

    fn float<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        format: FloatWidth,
        instruction: u64,
        next: u64,
    ) -> Result<u64, ExecutionExit> {
        match source {
            VectorSource::Register(index) => Ok(match format {
                FloatWidth::Single => cpu.vectors[usize::from(index)] as u32 as u64,
                FloatWidth::Double => cpu.vectors[usize::from(index)] as u64,
            }),
            VectorSource::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                Self::memory(memory, address, Arithmetic::bytes(format), instruction)
            }
        }
    }

    fn memory<M: GuestOperandMemory>(
        memory: &M,
        address: u64,
        bytes: u8,
        instruction: u64,
    ) -> Result<u64, ExecutionExit> {
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

    const fn merge(vector: u128, value: u64, format: FloatWidth) -> u128 {
        match format {
            FloatWidth::Single => vector & (u128::MAX << 32) | value as u32 as u128,
            FloatWidth::Double => vector & (u128::MAX << 64) | value as u128,
        }
    }

    pub(crate) const fn converted_nan(result: u64, source: u64, from: FloatWidth, to: FloatWidth) -> u64 {
        let source_nan = match from {
            FloatWidth::Single => source & 0x7f80_0000 == 0x7f80_0000 && source & 0x007f_ffff != 0,
            FloatWidth::Double => {
                source & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && source & 0x000f_ffff_ffff_ffff != 0
            }
        };
        if !source_nan {
            return result;
        }
        let source_sign = match from {
            FloatWidth::Single => source >> 31,
            FloatWidth::Double => source >> 63,
        };
        result
            | source_sign
                << match to {
                    FloatWidth::Single => 31,
                    FloatWidth::Double => 63,
                }
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
}
