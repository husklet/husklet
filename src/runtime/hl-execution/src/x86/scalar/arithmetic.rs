use crate::{
    AccessKind, CpuState, DecodedInstruction, ExecutionExit, FloatArithmetic, FloatWidth, GuestOperandMemory,
    ScalarInstruction, ScalarIrError, VectorSource,
};
use hl_softfloat::{Environment, ExceptionFlags, Format, NaNMode, RoundingMode, TininessMode, Value};

pub(crate) struct Arithmetic;

impl Arithmetic {
    pub(crate) fn round<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        packed: bool,
        control: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = if packed { 16 } else { Self::bytes(format) };
        let source = match Self::read(cpu, memory, source, bytes, instruction, next) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let mut environment = Self::environment(cpu.mxcsr);
        environment.rounding = match if control & 4 != 0 {
            cpu.mxcsr >> 13 & 3
        } else {
            u32::from(control & 3)
        } {
            0 => hl_softfloat::RoundingMode::NearestEven,
            1 => hl_softfloat::RoundingMode::TowardNegative,
            2 => hl_softfloat::RoundingMode::TowardPositive,
            _ => hl_softfloat::RoundingMode::TowardZero,
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        let lane_bits = u32::from(Self::bytes(format)) * 8;
        let lanes = if packed { 128 / lane_bits } else { 1 };
        for lane in 0..lanes {
            let shift = lane * lane_bits;
            let bits = Self::lane_at(source, format, shift);
            let result =
                environment.round_to_integral(Value::from_bits(Self::soft_format(format), bits), control & 8 == 0);
            let value = if Self::infinity(bits, format) {
                bits
            } else {
                result.value.bits()
            };
            staged.vectors[usize::from(destination)] =
                Self::merge_at(staged.vectors[usize::from(destination)], value, format, shift);
            let exceptions = Self::exceptions(result.flags) & !(1 << 1);
            staged.mxcsr |= exceptions;
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn pair_decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !(decoded.prefixes.operand_16 ^ decoded.prefixes.repne) || decoded.prefixes.rep {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::VectorPairArithmetic {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: super::Decoder::vector_source(decoded)?,
            format: if decoded.prefixes.operand_16 {
                FloatWidth::Double
            } else {
                FloatWidth::Single
            },
            subtract: decoded.opcode == 0x7d,
            alternating: decoded.opcode == 0xd0,
        })
    }

    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if matches!(decoded.opcode, 0x52 | 0x53) && (decoded.prefixes.operand_16 || decoded.prefixes.repne) {
            return Err(ScalarIrError::Invalid);
        }
        let packed = !decoded.prefixes.rep && !decoded.prefixes.repne;
        let format = if packed {
            if decoded.prefixes.operand_16 {
                FloatWidth::Double
            } else {
                FloatWidth::Single
            }
        } else {
            super::Decoder::float_format(decoded)?
        };
        Ok(ScalarInstruction::VectorFloatArithmetic {
            operation: super::Decoder::float_operation(decoded.opcode)?,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: super::Decoder::vector_source(decoded)?,
            format,
            packed,
        })
    }

    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        operation: FloatArithmetic,
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        packed: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = if packed { 16 } else { Self::bytes(format) };
        let source_vector = match Self::read(cpu, memory, source, bytes, instruction, next) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let environment = Self::environment(cpu.mxcsr);
        let soft_format = Self::soft_format(format);
        let mut staged = cpu.clone();
        staged.rip = next;
        let lane_bits = u32::from(Self::bytes(format)) * 8;
        let lane_count = if packed { 128 / lane_bits } else { 1 };
        for lane in 0..lane_count {
            let shift = lane * lane_bits;
            let destination_bits = Self::lane_at(cpu.vectors[usize::from(destination)], format, shift);
            let source_bits = Self::lane_at(source_vector, format, shift);
            let left = Value::from_bits(soft_format, destination_bits);
            let right = Value::from_bits(soft_format, source_bits);
            if matches!(
                operation,
                FloatArithmetic::Reciprocal | FloatArithmetic::ReciprocalSquareRoot
            ) {
                let input = f32::from_bits(source_bits as u32);
                let output = if operation == FloatArithmetic::Reciprocal {
                    1.0 / input
                } else if input.is_sign_negative() && input != 0.0 && !input.is_nan() {
                    f32::from_bits(0xffc0_0000)
                } else {
                    1.0 / input.sqrt()
                };
                staged.vectors[usize::from(destination)] = Self::merge_at(
                    staged.vectors[usize::from(destination)],
                    u64::from(output.to_bits()),
                    FloatWidth::Single,
                    shift,
                );
                continue;
            }
            let result = match operation {
                FloatArithmetic::Add => environment.add(left, right),
                FloatArithmetic::Subtract => environment.subtract(left, right),
                FloatArithmetic::Multiply => environment.multiply(left, right),
                FloatArithmetic::Divide => environment.divide(left, right),
                FloatArithmetic::SquareRoot => environment.square_root(right),
                FloatArithmetic::Minimum | FloatArithmetic::Maximum => {
                    let (value, flags) = Self::extremum(
                        destination_bits,
                        source_bits,
                        format,
                        operation == FloatArithmetic::Maximum,
                        cpu.mxcsr & (1 << 6) != 0,
                    );
                    hl_softfloat::Result {
                        value: Value::from_bits(soft_format, value),
                        flags,
                    }
                }
                FloatArithmetic::Reciprocal | FloatArithmetic::ReciprocalSquareRoot => unreachable!(),
            };
            let result_bits = if matches!(operation, FloatArithmetic::Minimum | FloatArithmetic::Maximum) {
                result.value.bits()
            } else if operation == FloatArithmetic::SquareRoot {
                Self::x86_nan(result.value.bits(), source_bits, None, format)
            } else {
                Self::x86_nan(result.value.bits(), destination_bits, Some(source_bits), format)
            };
            staged.vectors[usize::from(destination)] =
                Self::merge_at(staged.vectors[usize::from(destination)], result_bits, format, shift);
            let mut exceptions = Self::exceptions(result.flags);
            if cpu.mxcsr & (1 << 6) != 0 {
                exceptions &= !(1 << 1);
            } else if Self::denormal(source_bits, format)
                || operation != FloatArithmetic::SquareRoot && Self::denormal(destination_bits, format)
            {
                exceptions |= 1 << 1;
            }
            staged.mxcsr |= exceptions;
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn pair_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        subtract: bool,
        alternating: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let source = match Self::read(cpu, memory, source, 16, instruction, next) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let original = cpu.vectors[usize::from(destination)];
        let lane_bits = u32::from(Self::bytes(format)) * 8;
        let lanes = 128 / lane_bits;
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.vectors[usize::from(destination)] = 0;
        for lane in 0..lanes {
            let (left, right, subtract_lane) = if alternating {
                (
                    Self::lane_at(original, format, lane * lane_bits),
                    Self::lane_at(source, format, lane * lane_bits),
                    lane & 1 == 0,
                )
            } else {
                let half = lanes / 2;
                let vector = if lane < half { original } else { source };
                let pair = lane % half;
                (
                    Self::lane_at(vector, format, pair * 2 * lane_bits),
                    Self::lane_at(vector, format, (pair * 2 + 1) * lane_bits),
                    subtract,
                )
            };
            let environment = Self::environment(cpu.mxcsr);
            let left_value = Value::from_bits(Self::soft_format(format), left);
            let right_value = Value::from_bits(Self::soft_format(format), right);
            let result = if subtract_lane {
                environment.subtract(left_value, right_value)
            } else {
                environment.add(left_value, right_value)
            };
            let result_bits = Self::x86_nan(result.value.bits(), left, Some(right), format);
            staged.vectors[usize::from(destination)] = Self::merge_at(
                staged.vectors[usize::from(destination)],
                result_bits,
                format,
                lane * lane_bits,
            );
            let mut exceptions = Self::exceptions(result.flags);
            if cpu.mxcsr & (1 << 6) != 0 {
                exceptions &= !(1 << 1);
            } else if Self::denormal(left, format) || Self::denormal(right, format) {
                exceptions |= 1 << 1;
            }
            staged.mxcsr |= exceptions;
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn vex_pair_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        destination: u8,
        first: u8,
        second: VectorSource,
        format: FloatWidth,
        subtract: bool,
        alternating: bool,
        wide: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let right = match Self::vex_read(cpu, memory, second, wide, instruction, next) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let left = [cpu.vectors[usize::from(first)], cpu.vector_upper[usize::from(first)]];
        let mut staged = cpu.clone();
        staged.rip = next;
        let lane_bits = u32::from(Self::bytes(format)) * 8;
        let pairs = 128 / lane_bits / 2;
        for half in 0..if wide { 2 } else { 1 } {
            let mut output = 0_u128;
            for lane in 0..pairs * 2 {
                let (a, b, subtract_lane) = if alternating {
                    (
                        Self::lane_at(left[half], format, lane * lane_bits),
                        Self::lane_at(right[half], format, lane * lane_bits),
                        lane & 1 == 0,
                    )
                } else {
                    let vector = if lane < pairs { left[half] } else { right[half] };
                    let pair = lane % pairs;
                    (
                        Self::lane_at(vector, format, pair * 2 * lane_bits),
                        Self::lane_at(vector, format, (pair * 2 + 1) * lane_bits),
                        subtract,
                    )
                };
                let environment = Self::environment(cpu.mxcsr);
                let a_value = Value::from_bits(Self::soft_format(format), a);
                let b_value = Value::from_bits(Self::soft_format(format), b);
                let result = if subtract_lane {
                    environment.subtract(a_value, b_value)
                } else {
                    environment.add(a_value, b_value)
                };
                output = Self::merge_at(
                    output,
                    Self::x86_nan(result.value.bits(), a, Some(b), format),
                    format,
                    lane * lane_bits,
                );
                let mut exceptions = Self::exceptions(result.flags);
                if cpu.mxcsr & (1 << 6) != 0 {
                    exceptions &= !(1 << 1);
                } else if Self::denormal(a, format) || Self::denormal(b, format) {
                    exceptions |= 1 << 1;
                }
                staged.mxcsr |= exceptions;
            }
            if half == 0 {
                staged.vectors[usize::from(destination)] = output;
            } else {
                staged.vector_upper[usize::from(destination)] = output;
            }
        }
        if !wide {
            staged.vector_upper[usize::from(destination)] = 0;
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn vex_execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        operation: FloatArithmetic,
        destination: u8,
        first: u8,
        second: VectorSource,
        format: FloatWidth,
        scalar: bool,
        wide: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let lane_bytes = Self::bytes(format);
        let right = match if scalar {
            Self::read(cpu, memory, second, lane_bytes, instruction, next).map(|value| [value, 0])
        } else {
            Self::vex_read(cpu, memory, second, wide, instruction, next)
        } {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let left = [cpu.vectors[usize::from(first)], cpu.vector_upper[usize::from(first)]];
        let mut output = left;
        let mut raised = 0;
        let lane_bits = u32::from(lane_bytes) * 8;
        let lanes = if scalar {
            1
        } else if wide {
            256 / lane_bits
        } else {
            128 / lane_bits
        };
        let environment = Self::environment(cpu.mxcsr);
        for lane in 0..lanes {
            let half = (lane * lane_bits / 128) as usize;
            let shift = lane * lane_bits % 128;
            let a = Self::lane_at(left[half], format, shift);
            let b = Self::lane_at(right[half], format, shift);
            if matches!(
                operation,
                FloatArithmetic::Reciprocal | FloatArithmetic::ReciprocalSquareRoot
            ) {
                let input = f32::from_bits(b as u32);
                let value = if operation == FloatArithmetic::Reciprocal {
                    1.0 / input
                } else if input.is_sign_negative() && input != 0.0 && !input.is_nan() {
                    f32::from_bits(0xffc0_0000)
                } else {
                    1.0 / input.sqrt()
                };
                output[half] = Self::merge_at(output[half], u64::from(value.to_bits()), format, shift);
                continue;
            }
            let soft = Self::soft_format(format);
            let result = match operation {
                FloatArithmetic::Add => environment.add(Value::from_bits(soft, a), Value::from_bits(soft, b)),
                FloatArithmetic::Subtract => environment.subtract(Value::from_bits(soft, a), Value::from_bits(soft, b)),
                FloatArithmetic::Multiply => environment.multiply(Value::from_bits(soft, a), Value::from_bits(soft, b)),
                FloatArithmetic::Divide => environment.divide(Value::from_bits(soft, a), Value::from_bits(soft, b)),
                FloatArithmetic::SquareRoot => environment.square_root(Value::from_bits(soft, b)),
                FloatArithmetic::Minimum | FloatArithmetic::Maximum => {
                    let (value, flags) = Self::extremum(
                        a,
                        b,
                        format,
                        operation == FloatArithmetic::Maximum,
                        cpu.mxcsr & (1 << 6) != 0,
                    );
                    hl_softfloat::Result {
                        value: Value::from_bits(soft, value),
                        flags,
                    }
                }
                FloatArithmetic::Reciprocal | FloatArithmetic::ReciprocalSquareRoot => unreachable!(),
            };
            let bits = if matches!(operation, FloatArithmetic::Minimum | FloatArithmetic::Maximum) {
                result.value.bits()
            } else if operation == FloatArithmetic::SquareRoot {
                Self::x86_nan(result.value.bits(), b, None, format)
            } else {
                Self::x86_nan(result.value.bits(), a, Some(b), format)
            };
            output[half] = Self::merge_at(output[half], bits, format, shift);
            let mut exceptions = Self::exceptions(result.flags);
            if cpu.mxcsr & (1 << 6) != 0 {
                exceptions &= !(1 << 1);
            } else if Self::denormal(b, format) || operation != FloatArithmetic::SquareRoot && Self::denormal(a, format)
            {
                exceptions |= 1 << 1;
            }
            raised |= exceptions;
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.vectors[usize::from(destination)] = output[0];
        staged.vector_upper[usize::from(destination)] = if wide && !scalar { output[1] } else { 0 };
        staged.mxcsr |= raised;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn vex_read<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        wide: bool,
        instruction: u64,
        next: u64,
    ) -> Result<[u128; 2], ExecutionExit> {
        if let VectorSource::Register(register) = source {
            return Ok([
                cpu.vectors[usize::from(register)],
                cpu.vector_upper[usize::from(register)],
            ]);
        }
        let VectorSource::Memory(effective) = source else {
            unreachable!()
        };
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let bytes = if wide { 32_u8 } else { 16 };
        let Some(last) = address.checked_add(u64::from(bytes - 1)) else {
            return Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            });
        };
        if !Self::canonical(address) || !Self::canonical(last) {
            return Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            });
        }
        let mut words = [0_u64; 4];
        for (index, word) in words[..usize::from(bytes / 8)].iter_mut().enumerate() {
            let cursor = address + (index * 8) as u64;
            *word = memory.read(cursor, 8).map_err(|()| {
                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    cursor,
                    AccessKind::Read,
                    u64::from(bytes),
                ))
            })?;
        }
        Ok([
            u128::from(words[0]) | (u128::from(words[1]) << 64),
            u128::from(words[2]) | (u128::from(words[3]) << 64),
        ])
    }

    pub(crate) fn environment(mxcsr: u32) -> Environment {
        Environment {
            rounding: match mxcsr >> 13 & 3 {
                0 => RoundingMode::NearestEven,
                1 => RoundingMode::TowardNegative,
                2 => RoundingMode::TowardPositive,
                _ => RoundingMode::TowardZero,
            },
            tininess: TininessMode::AfterRounding,
            nan: NaNMode::PropagatePayload,
            flush_inputs: mxcsr & (1 << 6) != 0,
            flush_outputs: mxcsr & (1 << 15) != 0,
        }
    }

    pub(crate) fn exceptions(flags: ExceptionFlags) -> u32 {
        let mut result = 0;
        for (flag, bit) in [
            (ExceptionFlags::INVALID, 0),
            (ExceptionFlags::INPUT_DENORMAL, 1),
            (ExceptionFlags::DIVIDE_BY_ZERO, 2),
            (ExceptionFlags::OVERFLOW, 3),
            (ExceptionFlags::UNDERFLOW, 4),
            (ExceptionFlags::INEXACT, 5),
        ] {
            if flags.contains(flag) {
                result |= 1 << bit;
            }
        }
        result
    }

    pub(crate) const fn soft_format(format: FloatWidth) -> Format {
        match format {
            FloatWidth::Single => Format::Binary32,
            FloatWidth::Double => Format::Binary64,
        }
    }

    pub(crate) const fn bytes(format: FloatWidth) -> u8 {
        match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        }
    }

    pub(crate) const fn denormal(bits: u64, format: FloatWidth) -> bool {
        match format {
            FloatWidth::Single => bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0,
            FloatWidth::Double => bits & 0x7ff0_0000_0000_0000 == 0 && bits & 0x000f_ffff_ffff_ffff != 0,
        }
    }

    pub(crate) const fn infinity(bits: u64, format: FloatWidth) -> bool {
        match format {
            FloatWidth::Single => bits & 0x7fff_ffff == 0x7f80_0000,
            FloatWidth::Double => bits & 0x7fff_ffff_ffff_ffff == 0x7ff0_0000_0000_0000,
        }
    }

    const fn zero(bits: u64, format: FloatWidth) -> bool {
        match format {
            FloatWidth::Single => bits & 0x7fff_ffff == 0,
            FloatWidth::Double => bits & 0x7fff_ffff_ffff_ffff == 0,
        }
    }

    fn extremum(left: u64, right: u64, format: FloatWidth, maximum: bool, daz: bool) -> (u64, ExceptionFlags) {
        let mut flags = ExceptionFlags::default();
        if Self::signaling_nan(left, format) || Self::signaling_nan(right, format) {
            flags |= ExceptionFlags::INVALID;
        }
        if !daz && (Self::denormal(left, format) || Self::denormal(right, format)) {
            flags |= ExceptionFlags::INPUT_DENORMAL;
        }
        let left = Self::normalize(left, format, daz);
        let right = Self::normalize(right, format, daz);
        if Self::nan(left, format)
            || Self::nan(right, format)
            || left == right
            || Self::zero(left, format) && Self::zero(right, format)
        {
            return (right, flags);
        }
        let sign = Self::sign(format);
        let left_negative = left & sign != 0;
        let right_negative = right & sign != 0;
        let less = if left_negative != right_negative {
            left_negative
        } else if left_negative {
            left > right
        } else {
            left < right
        };
        let choose_left = if maximum { !less } else { less };
        (if choose_left { left } else { right }, flags)
    }

    const fn normalize(bits: u64, format: FloatWidth, daz: bool) -> u64 {
        if daz && Self::denormal(bits, format) {
            bits & Self::sign(format)
        } else {
            bits
        }
    }

    const fn signaling_nan(bits: u64, format: FloatWidth) -> bool {
        Self::nan(bits, format) && bits & Self::quiet(format) == 0
    }

    const fn sign(format: FloatWidth) -> u64 {
        match format {
            FloatWidth::Single => 1 << 31,
            FloatWidth::Double => 1_u64 << 63,
        }
    }

    fn read<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        bytes: u8,
        instruction: u64,
        next: u64,
    ) -> Result<u128, ExecutionExit> {
        if bytes == 16 {
            return crate::x86::VectorLane::read(cpu, memory, source, next, instruction);
        }
        let VectorSource::Memory(effective) = source else {
            let VectorSource::Register(index) = source else {
                unreachable!()
            };
            return Ok(u128::from(
                cpu.vectors[usize::from(index)] as u64 & Self::byte_mask(bytes),
            ));
        };
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(u64::from(bytes - 1))) {
            return Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            });
        }
        memory.read(address, bytes).map(u128::from).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Read,
                u64::from(bytes),
            ))
        })
    }

    const fn x86_nan(result: u64, left: u64, right: Option<u64>, format: FloatWidth) -> u64 {
        if Self::nan(left, format) {
            return left | Self::quiet(format);
        }
        if let Some(right) = right
            && Self::nan(right, format)
        {
            return right | Self::quiet(format);
        }
        if Self::nan(result, format) {
            return match format {
                FloatWidth::Single => 0xffc0_0000,
                FloatWidth::Double => 0xfff8_0000_0000_0000,
            };
        }
        result
    }

    const fn nan(bits: u64, format: FloatWidth) -> bool {
        match format {
            FloatWidth::Single => bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0,
            FloatWidth::Double => {
                bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
            }
        }
    }

    const fn quiet(format: FloatWidth) -> u64 {
        match format {
            FloatWidth::Single => 1 << 22,
            FloatWidth::Double => 1_u64 << 51,
        }
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

    const fn byte_mask(bytes: u8) -> u64 {
        if bytes == 8 { u64::MAX } else { u32::MAX as u64 }
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
}
