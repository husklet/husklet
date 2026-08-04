use crate::{
    CpuState, DecodedInstruction, ExecutionExit, GuestOperandMemory, MmxCount, MmxOperation, ScalarInstruction,
    ScalarInterpreter, ScalarIrError, ScalarOperand, ScalarWidth, Staged, VectorPackKind, VectorShiftKind,
    VectorSource,
};

pub(crate) struct Mmx;

impl Mmx {
    pub(crate) fn accepts(decoded: &DecodedInstruction) -> bool {
        !decoded.prefixes.operand_16
            && !decoded.prefixes.rep
            && !decoded.prefixes.repne
            && matches!(decoded.opcode,
                0x60..=0x6b | 0x6e..=0x77 | 0x7e..=0x7f |
                0xd1..=0xd5 | 0xd8..=0xdf | 0xe0..=0xe5 |
                0xe8..=0xef | 0xf1..=0xf6 | 0xf8..=0xfe)
    }

    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.opcode == 0x77 {
            return Ok(ScalarInstruction::MmxEmpty);
        }
        let register = decoded.raw_reg.ok_or(ScalarIrError::Invalid)?;
        if matches!(decoded.opcode, 0x6e | 0x7e) {
            return Ok(ScalarInstruction::MmxScalar {
                register,
                operand: crate::x86::scalar::Decoder::rm(decoded, false)?,
                store: decoded.opcode == 0x7e,
            });
        }
        let operand = Self::source(decoded)?;
        if matches!(decoded.opcode, 0x6f | 0x7f) {
            return Ok(ScalarInstruction::MmxTransport {
                register,
                operand,
                store: decoded.opcode == 0x7f,
            });
        }
        if matches!(decoded.opcode, 0x71..=0x73) {
            let (kind, lane) = Self::shift(decoded.opcode, register)?;
            return Ok(ScalarInstruction::MmxShift {
                kind,
                lane,
                destination: decoded.raw_rm.ok_or(ScalarIrError::Invalid)?,
                count: MmxCount::Immediate(decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8),
            });
        }
        if let Ok((kind, lane)) = Self::shift(decoded.opcode, 0) {
            return Ok(ScalarInstruction::MmxShift {
                kind,
                lane,
                destination: register,
                count: MmxCount::Source(operand),
            });
        }
        Ok(ScalarInstruction::MmxPacked {
            operation: Self::operation(decoded.opcode)?,
            destination: register,
            source: operand,
        })
    }

    fn source(decoded: &DecodedInstruction) -> Result<VectorSource, ScalarIrError> {
        Ok(if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.raw_rm.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        })
    }

    fn shift(opcode: u8, group: u8) -> Result<(VectorShiftKind, u8), ScalarIrError> {
        let key = if matches!(opcode, 0x71..=0x73) {
            (group, opcode)
        } else {
            (0, opcode)
        };
        match key {
            (2, 0x71) | (0, 0xd1) => Ok((VectorShiftKind::LogicalRight, 2)),
            (2, 0x72) | (0, 0xd2) => Ok((VectorShiftKind::LogicalRight, 4)),
            (2, 0x73) | (0, 0xd3) => Ok((VectorShiftKind::LogicalRight, 8)),
            (4, 0x71) | (0, 0xe1) => Ok((VectorShiftKind::ArithmeticRight, 2)),
            (4, 0x72) | (0, 0xe2) => Ok((VectorShiftKind::ArithmeticRight, 4)),
            (6, 0x71) | (0, 0xf1) => Ok((VectorShiftKind::Left, 2)),
            (6, 0x72) | (0, 0xf2) => Ok((VectorShiftKind::Left, 4)),
            (6, 0x73) | (0, 0xf3) => Ok((VectorShiftKind::Left, 8)),
            _ => Err(ScalarIrError::Unsupported),
        }
    }

    fn operation(opcode: u8) -> Result<MmxOperation, ScalarIrError> {
        use MmxOperation as O;
        Ok(match opcode {
            0xdb => O::And,
            0xdf => O::AndNot,
            0xeb => O::Or,
            0xef => O::Xor,
            0xfc => O::Add(1),
            0xfd => O::Add(2),
            0xfe => O::Add(4),
            0xd4 => O::Add(8),
            0xf8 => O::Subtract(1),
            0xf9 => O::Subtract(2),
            0xfa => O::Subtract(4),
            0xdc => O::AddUnsigned(1),
            0xdd => O::AddUnsigned(2),
            0xd8 => O::SubtractUnsigned(1),
            0xd9 => O::SubtractUnsigned(2),
            0xec => O::AddSigned(1),
            0xed => O::AddSigned(2),
            0xe8 => O::SubtractSigned(1),
            0xe9 => O::SubtractSigned(2),
            0x74 => O::Equal(1),
            0x75 => O::Equal(2),
            0x76 => O::Equal(4),
            0x64 => O::Greater(1),
            0x65 => O::Greater(2),
            0x66 => O::Greater(4),
            0xda => O::Extrema {
                lane: 1,
                signed: false,
                minimum: true,
            },
            0xde => O::Extrema {
                lane: 1,
                signed: false,
                minimum: false,
            },
            0xea => O::Extrema {
                lane: 2,
                signed: true,
                minimum: true,
            },
            0xee => O::Extrema {
                lane: 2,
                signed: true,
                minimum: false,
            },
            0xe0 => O::Average(1),
            0xe3 => O::Average(2),
            0x60 => O::Unpack { lane: 1, high: false },
            0x61 => O::Unpack { lane: 2, high: false },
            0x62 => O::Unpack { lane: 4, high: false },
            0x68 => O::Unpack { lane: 1, high: true },
            0x69 => O::Unpack { lane: 2, high: true },
            0x6a => O::Unpack { lane: 4, high: true },
            0x63 => O::Pack(VectorPackKind::SignedBytes),
            0x67 => O::Pack(VectorPackKind::UnsignedBytes),
            0x6b => O::Pack(VectorPackKind::SignedWords),
            0xd5 => O::MultiplyLow,
            0xe4 => O::UnsignedMultiplyHigh,
            0xe5 => O::MultiplyHigh,
            0xf4 => O::UnsignedMultiplyDword,
            0xf5 => O::MultiplyAdd,
            0xf6 => O::SumAbsoluteDifferences,
            _ => return Err(ScalarIrError::Unsupported),
        })
    }

    fn packed(left: u64, right: u64, operation: MmxOperation) -> u64 {
        use MmxOperation as O;
        match operation {
            O::And => left & right,
            O::AndNot => !left & right,
            O::Or => left | right,
            O::Xor => left ^ right,
            O::Add(lane) => Self::lanes(left, right, lane, u64::wrapping_add),
            O::Subtract(lane) => Self::lanes(left, right, lane, u64::wrapping_sub),
            O::AddUnsigned(lane) => Self::saturating(left, right, lane, false, false),
            O::SubtractUnsigned(lane) => Self::saturating(left, right, lane, false, true),
            O::AddSigned(lane) => Self::saturating(left, right, lane, true, false),
            O::SubtractSigned(lane) => Self::saturating(left, right, lane, true, true),
            O::Equal(lane) => Self::comparison(left, right, lane, false),
            O::Greater(lane) => Self::comparison(left, right, lane, true),
            O::Extrema { lane, signed, minimum } => Self::extrema(left, right, lane, signed, minimum),
            O::Average(lane) => Self::lanes(left, right, lane, |a, b| (a + b + 1) >> 1),
            O::Unpack { lane, high } => Self::unpack(left, right, lane, high),
            O::Pack(kind) => Self::pack(left, right, kind),
            O::MultiplyLow => Self::multiply_words(left, right, false),
            O::MultiplyHigh => Self::multiply_words(left, right, true),
            O::UnsignedMultiplyHigh => Self::multiply_high_unsigned(left, right),
            O::MultiplyAdd => Self::multiply_add(left, right),
            O::UnsignedMultiplyDword => u64::from(left as u32) * u64::from(right as u32),
            O::SumAbsoluteDifferences => {
                let mut sum = 0_u64;
                for index in 0..8 {
                    let a = (left >> (index * 8)) as u8;
                    let b = (right >> (index * 8)) as u8;
                    sum += u64::from(a.abs_diff(b));
                }
                sum
            }
        }
    }

    fn shifted(value: u64, lane: u8, count: u64, kind: VectorShiftKind) -> u64 {
        let count = u8::try_from(count).unwrap_or(u8::MAX);
        crate::x86::vector::Lane::shift(u128::from(value), lane, count, kind) as u64
    }

    fn extrema(left: u64, right: u64, lane: u8, signed: bool, minimum: bool) -> u64 {
        let bits = u32::from(lane) * 8;
        let mask = (1_u64 << bits) - 1;
        let mut result = 0_u64;
        for index in 0..8 / u32::from(lane) {
            let shift = index * bits;
            let a = left >> shift & mask;
            let b = right >> shift & mask;
            let take_a = if signed {
                let a = Self::signed(a, bits);
                let b = Self::signed(b, bits);
                if minimum { a < b } else { a > b }
            } else if minimum {
                a < b
            } else {
                a > b
            };
            result |= (if take_a { a } else { b }) << shift;
        }
        result
    }

    fn lanes(left: u64, right: u64, lane: u8, operation: fn(u64, u64) -> u64) -> u64 {
        let bits = u32::from(lane) * 8;
        let mask = if bits == 64 { u64::MAX } else { (1_u64 << bits) - 1 };
        let mut result = 0;
        for index in 0..8 / u32::from(lane) {
            let shift = index * bits;
            let value = operation(left >> shift & mask, right >> shift & mask) & mask;
            result |= value << shift;
        }
        result
    }

    fn saturating(left: u64, right: u64, lane: u8, signed: bool, subtract: bool) -> u64 {
        let bits = u32::from(lane) * 8;
        let mask = (1_u64 << bits) - 1;
        let mut result = 0;
        for index in 0..8 / u32::from(lane) {
            let shift = index * bits;
            let a = left >> shift & mask;
            let b = right >> shift & mask;
            let value = Self::saturated_value(a, b, bits, mask, signed, subtract);
            result |= value << shift;
        }
        result
    }

    fn saturated_value(a: u64, b: u64, bits: u32, mask: u64, signed: bool, subtract: bool) -> u64 {
        if !signed {
            return if subtract {
                a.saturating_sub(b)
            } else {
                a.saturating_add(b).min(mask)
            };
        }
        let a = Self::signed(a, bits);
        let b = Self::signed(b, bits);
        let raw = if subtract { a - b } else { a + b };
        let minimum = -(1_i128 << (bits - 1));
        let maximum = (1_i128 << (bits - 1)) - 1;
        raw.clamp(minimum, maximum) as u64 & mask
    }

    fn comparison(left: u64, right: u64, lane: u8, greater: bool) -> u64 {
        let bits = u32::from(lane) * 8;
        let mask = (1_u64 << bits) - 1;
        let mut result = 0;
        for index in 0..8 / u32::from(lane) {
            let shift = index * bits;
            let a = left >> shift & mask;
            let b = right >> shift & mask;
            let selected = if greater {
                Self::signed(a, bits) > Self::signed(b, bits)
            } else {
                a == b
            };
            if selected {
                result |= mask << shift;
            }
        }
        result
    }

    fn unpack(left: u64, right: u64, lane: u8, high: bool) -> u64 {
        let bits = u32::from(lane) * 8;
        let mask = (1_u64 << bits) - 1;
        let count = 4 / u32::from(lane);
        let base = if high { count } else { 0 };
        let mut result = 0;
        for index in 0..count {
            let source = (base + index) * bits;
            let destination = index * bits * 2;
            result |= (left >> source & mask) << destination;
            result |= (right >> source & mask) << (destination + bits);
        }
        result
    }

    fn pack(left: u64, right: u64, kind: VectorPackKind) -> u64 {
        let (source_bits, destination_bits, unsigned) = match kind {
            VectorPackKind::SignedBytes => (16, 8, false),
            VectorPackKind::UnsignedBytes => (16, 8, true),
            VectorPackKind::SignedWords => (32, 16, false),
            VectorPackKind::UnsignedWords => (32, 16, true),
        };
        let source_mask = (1_u64 << source_bits) - 1;
        let destination_mask = (1_u64 << destination_bits) - 1;
        let count = 64 / source_bits;
        let mut result = 0;
        for (half, source) in [left, right].into_iter().enumerate() {
            for index in 0..count {
                let raw = source >> (index * source_bits) & source_mask;
                let signed = Self::signed(raw, source_bits);
                let value = Self::packed_value(signed, destination_bits, destination_mask, unsigned);
                let destination = (half as u32 * count + index) * destination_bits;
                result |= value << destination;
            }
        }
        result
    }

    fn packed_value(value: i128, destination_bits: u32, destination_mask: u64, unsigned: bool) -> u64 {
        if unsigned {
            return value.clamp(0, destination_mask as i128) as u64;
        }
        let minimum = -(1_i128 << (destination_bits - 1));
        let maximum = (1_i128 << (destination_bits - 1)) - 1;
        value.clamp(minimum, maximum) as u64 & destination_mask
    }

    fn multiply_words(left: u64, right: u64, high: bool) -> u64 {
        let mut result = 0;
        for index in 0..4 {
            let shift = index * 16;
            let a = Self::signed(left >> shift & 0xffff, 16);
            let b = Self::signed(right >> shift & 0xffff, 16);
            let product = a * b;
            let value = if high { product >> 16 } else { product } as u64 & 0xffff;
            result |= value << shift;
        }
        result
    }

    fn multiply_add(left: u64, right: u64) -> u64 {
        let mut result = 0;
        for pair in 0..2 {
            let mut sum = 0_i128;
            for lane in 0..2 {
                let shift = (pair * 2 + lane) * 16;
                sum += Self::signed(left >> shift & 0xffff, 16) * Self::signed(right >> shift & 0xffff, 16);
            }
            result |= (sum as u64 & u64::from(u32::MAX)) << (pair * 32);
        }
        result
    }

    fn multiply_high_unsigned(left: u64, right: u64) -> u64 {
        let mut result = 0;
        for index in 0..4 {
            let shift = index * 16;
            let product = (left >> shift & 0xffff) * (right >> shift & 0xffff);
            result |= (product >> 16 & 0xffff) << shift;
        }
        result
    }

    fn signed(value: u64, bits: u32) -> i128 {
        ((u128::from(value) << (128 - bits)) as i128) >> (128 - bits)
    }

    pub(crate) fn stage<M: GuestOperandMemory>(
        mut staged: CpuState,
        cpu: &CpuState,
        memory: &M,
        operation: ScalarInstruction,
        width: ScalarWidth,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        match operation {
            ScalarInstruction::MmxScalar {
                register,
                operand,
                store,
            } => {
                if store {
                    return ScalarInterpreter::write(
                        staged,
                        memory,
                        operand,
                        width,
                        cpu.read_mmx(register),
                        next,
                        instruction,
                    );
                }
                let value = ScalarInterpreter::read(cpu, memory, operand, width, next, instruction)?;
                staged.write_mmx(register, value);
            }
            ScalarInstruction::MmxTransport {
                register,
                operand,
                store,
            } => {
                if store {
                    return Self::store(staged, cpu, memory, register, operand, next, instruction);
                }
                let value = match operand {
                    VectorSource::Register(source) => cpu.read_mmx(source),
                    VectorSource::Memory(address) => ScalarInterpreter::read(
                        cpu,
                        memory,
                        ScalarOperand::Memory(address),
                        ScalarWidth::Qword,
                        next,
                        instruction,
                    )?,
                };
                staged.write_mmx(register, value);
            }
            ScalarInstruction::MmxVector { mmx, vector, to_vector } => {
                if to_vector {
                    staged.vectors[usize::from(vector)] = u128::from(cpu.read_mmx(mmx));
                } else {
                    staged.write_mmx(mmx, cpu.vectors[usize::from(vector)] as u64);
                }
            }
            ScalarInstruction::MmxExtractWord {
                source,
                destination,
                lane,
            } => {
                staged.write_register(
                    destination,
                    ScalarWidth::Dword,
                    cpu.read_mmx(source) >> (u32::from(lane) * 16) & u64::from(u16::MAX),
                );
            }
            ScalarInstruction::MmxMask { destination, source } => {
                let value = cpu.read_mmx(source);
                let mask = (0..8).fold(0_u64, |mask, lane| mask | ((value >> (lane * 8 + 7) & 1) << lane));
                staged.write_register(destination, ScalarWidth::Dword, mask);
            }
            ScalarInstruction::MmxInsertWord {
                destination,
                source,
                lane,
            } => {
                let value = ScalarInterpreter::read(cpu, memory, source, ScalarWidth::Word, next, instruction)?;
                let shift = u32::from(lane) * 16;
                let mask = u64::from(u16::MAX) << shift;
                staged.write_mmx(destination, cpu.read_mmx(destination) & !mask | value << shift);
            }
            ScalarInstruction::MmxPacked {
                operation,
                destination,
                source,
            } => {
                let right = Self::read(cpu, memory, source, next, instruction)?;
                let value = Self::packed(cpu.read_mmx(destination), right, operation);
                staged.write_mmx(destination, value);
            }
            ScalarInstruction::MmxShift {
                kind,
                lane,
                destination,
                count,
            } => {
                let count = match count {
                    MmxCount::Immediate(value) => u64::from(value),
                    MmxCount::Source(source) => Self::read(cpu, memory, source, next, instruction)?,
                };
                let value = Self::shifted(cpu.read_mmx(destination), lane, count, kind);
                staged.write_mmx(destination, value);
            }
            ScalarInstruction::MmxEmpty => staged.empty_mmx(),
            _ => unreachable!(),
        }
        Ok(Staged::Cpu(staged))
    }

    fn read<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        next: u64,
        instruction: u64,
    ) -> Result<u64, ExecutionExit> {
        match source {
            VectorSource::Register(register) => Ok(cpu.read_mmx(register)),
            VectorSource::Memory(address) => ScalarInterpreter::read(
                cpu,
                memory,
                ScalarOperand::Memory(address),
                ScalarWidth::Qword,
                next,
                instruction,
            ),
        }
    }

    fn store<M: GuestOperandMemory>(
        mut staged: CpuState,
        cpu: &CpuState,
        memory: &M,
        register: u8,
        operand: VectorSource,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let value = cpu.read_mmx(register);
        match operand {
            VectorSource::Register(destination) => {
                staged.write_mmx(destination, value);
                Ok(Staged::Cpu(staged))
            }
            VectorSource::Memory(address) => ScalarInterpreter::write(
                staged,
                memory,
                ScalarOperand::Memory(address),
                ScalarWidth::Qword,
                value,
                next,
                instruction,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_arithmetic() {
        assert_eq!(
            Mmx::packed(0x00ff_7f80_0102_feff, 0x0100_8081_0203_ff00, MmxOperation::Average(1)),
            0x0180_8081_0203_ff80
        );
        assert_eq!(
            Mmx::packed(
                0x01ff_7f80_1020_3040,
                0x0201_8080_2010_3050,
                MmxOperation::Extrema {
                    lane: 1,
                    signed: false,
                    minimum: true
                },
            ),
            0x0101_7f80_1010_3040
        );
        assert_eq!(
            Mmx::packed(
                0x7fff_8000_0001_ffff,
                0x7000_9000_ffff_0001,
                MmxOperation::Extrema {
                    lane: 2,
                    signed: true,
                    minimum: false
                },
            ),
            0x7fff_9000_0001_0001
        );
        assert_eq!(Mmx::packed(u64::MAX, 2, MmxOperation::Add(8)), 1);
        assert_eq!(
            Mmx::packed(0x7fff_8000_ffff_0001, 0x0001_ffff_0001_ffff, MmxOperation::AddSigned(2)),
            0x7fff_8000_0000_0000
        );
        assert_eq!(
            Mmx::packed(
                0xff00_0100_00ff_0000,
                0x0200_0200_0200_0100,
                MmxOperation::SubtractUnsigned(2)
            ),
            0xfd00_0000_0000_0000
        );
        assert_eq!(
            Mmx::packed(0x0102_ff80_7f00_00ff, 0x0001_0080_8000_00ff, MmxOperation::Greater(1)),
            0xffff_0000_ff00_0000
        );
    }

    #[test]
    fn packed_reordering() {
        assert_eq!(
            Mmx::packed(
                0x0706_0504_0302_0100,
                0x1716_1514_1312_1110,
                MmxOperation::Unpack { lane: 1, high: false }
            ),
            0x1303_1202_1101_1000
        );
        assert_eq!(
            Mmx::packed(
                0x0001_007f_0080_7fff,
                0xffff_ff80_ff7f_8000,
                MmxOperation::Pack(VectorPackKind::SignedBytes)
            ),
            0xff_80_80_80_01_7f_7f_7f
        );
    }

    #[test]
    fn packed_products() {
        assert_eq!(
            Mmx::packed(0x0002_ffff_8000_7fff, 0x0003_0002_0002_0002, MmxOperation::MultiplyLow),
            0x0006_fffe_0000_fffe
        );
        assert_eq!(
            Mmx::packed(0x0002_0003_ffff_0004, 0x0005_0006_0007_0008, MmxOperation::MultiplyAdd),
            0x0000_001c_0000_0019
        );
        assert_eq!(
            Mmx::packed(
                0xffff_ffff_ffff_fffe,
                0x1234_5678_ffff_fffd,
                MmxOperation::UnsignedMultiplyDword
            ),
            u64::from(u32::MAX - 1) * u64::from(u32::MAX - 2)
        );
    }

    #[test]
    fn packed_shifts() {
        assert_eq!(
            Mmx::shifted(0x8000_7fff_ffff_0001, 2, 16, VectorShiftKind::ArithmeticRight),
            0xffff_0000_ffff_0000
        );
        assert_eq!(Mmx::shifted(u64::MAX, 4, 32, VectorShiftKind::LogicalRight), 0);
        assert_eq!(Mmx::shifted(1, 8, 64, VectorShiftKind::Left), 0);
    }
}
