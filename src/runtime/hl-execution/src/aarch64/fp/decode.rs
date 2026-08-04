use crate::{
    Aarch64BranchCondition, Aarch64DecodeError, Aarch64Instruction, FpBinaryOperation, FpFormat, FpRoundingMode,
    FpUnaryOperation,
};

pub(crate) struct Decoder;
pub(crate) type Aarch64FpDecoder = Decoder;

impl Aarch64FpDecoder {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        if let Some(decoded) = crate::aarch64_simd_bf16::Bf16::decode_scalar(word) {
            return Some(Ok(decoded));
        }
        if word & 0xffa0_fc00 == 0x7ea0_d400 {
            return Some(Ok(Aarch64Instruction::FpBinary {
                operation: FpBinaryOperation::AbsoluteDifference,
                format: if word >> 22 & 1 == 0 {
                    FpFormat::Single
                } else {
                    FpFormat::Double
                },
                left: (word >> 5 & 31) as u8,
                right: (word >> 16 & 31) as u8,
                destination: (word & 31) as u8,
            }));
        }
        if !matches!(word & 0x7f00_0000, 0x1e00_0000 | 0x1f00_0000) {
            return None;
        }
        Some(Self::scalar(word))
    }

    fn scalar(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let format_bits = word >> 22 & 3;
        let source = (word >> 5 & 31) as u8;
        let destination = (word & 31) as u8;
        if word & 0x7f00_0000 == 0x1f00_0000 {
            let format = Self::format(format_bits)?;
            let negate_addend = word >> 21 & 1 != 0;
            return Ok(Aarch64Instruction::FpFused {
                format,
                left: source,
                right: (word >> 16 & 31) as u8,
                addend: (word >> 10 & 31) as u8,
                destination,
                negate_product: negate_addend != (word >> 15 & 1 != 0),
                negate_addend,
            });
        }
        if word & 0x0000_fc00 == 0 {
            return Self::integer(word, format_bits, source, destination);
        }
        let format = Self::format(format_bits)?;
        if word >> 10 & 3 == 1 {
            return Ok(Aarch64Instruction::FpCompare {
                format,
                left: source,
                right: Some((word >> 16 & 31) as u8),
                signaling: word >> 4 & 1 != 0,
                condition: Some(Aarch64BranchCondition((word >> 12 & 15) as u8)),
                alternative_nzcv: (word & 15) as u8,
            });
        }
        if word >> 10 & 3 == 2 {
            let operation = match word >> 12 & 15 {
                0 => FpBinaryOperation::Multiply,
                1 => FpBinaryOperation::Divide,
                2 => FpBinaryOperation::Add,
                3 => FpBinaryOperation::Subtract,
                4 => FpBinaryOperation::Maximum,
                5 => FpBinaryOperation::Minimum,
                6 => FpBinaryOperation::MaximumNumber,
                7 => FpBinaryOperation::MinimumNumber,
                _ => return Err(Aarch64DecodeError::Unsupported),
            };
            return Ok(Aarch64Instruction::FpBinary {
                operation,
                format,
                left: source,
                right: (word >> 16 & 31) as u8,
                destination,
            });
        }
        if word & 0xff20_0c00 == 0x1e20_0c00 {
            return Ok(Aarch64Instruction::FpSelect {
                format,
                source,
                alternate: (word >> 16 & 31) as u8,
                destination,
                condition: Aarch64BranchCondition((word >> 12 & 15) as u8),
            });
        }
        if word & 0x0000_7c00 == 0x0000_4000 {
            return Self::one_source(word, format, source, destination);
        }
        if word & 0x0000_3c00 == 0x0000_2000 {
            if word >> 14 & 3 != 0 || word & 7 != 0 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::FpCompare {
                format,
                left: source,
                right: if word >> 3 & 1 != 0 {
                    None
                } else {
                    Some((word >> 16 & 31) as u8)
                },
                signaling: word >> 4 & 1 != 0,
                condition: None,
                alternative_nzcv: 0,
            });
        }
        if word & 0x0000_1c00 == 0x0000_1000 {
            if word >> 5 & 31 != 0 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::FpImmediate {
                format,
                immediate: (word >> 13 & 0xff) as u8,
                destination,
            });
        }
        Err(Aarch64DecodeError::Unsupported)
    }

    fn one_source(
        word: u32,
        format: FpFormat,
        source: u8,
        destination: u8,
    ) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let opcode = word >> 15 & 63;
        if opcode & 0x3c == 4 {
            let destination_format = Self::format(opcode & 3)?;
            if destination_format == format {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::FpFormatConvert {
                source_format: format,
                destination_format,
                source,
                destination,
            });
        }
        let operation = match opcode {
            0 => FpUnaryOperation::Move,
            1 => FpUnaryOperation::Absolute,
            2 => FpUnaryOperation::Negate,
            3 => FpUnaryOperation::SquareRoot,
            8..=12 | 14 | 15 => {
                let (rounding, exact) = match opcode {
                    8 => (FpRoundingMode::NearestEven, false),
                    9 => (FpRoundingMode::PositiveInfinity, false),
                    10 => (FpRoundingMode::NegativeInfinity, false),
                    11 => (FpRoundingMode::Zero, false),
                    12 => (FpRoundingMode::NearestAway, false),
                    14 => (FpRoundingMode::Current, true),
                    _ => (FpRoundingMode::Current, false),
                };
                return Ok(Aarch64Instruction::FpRoundIntegral {
                    format,
                    source,
                    destination,
                    rounding,
                    exact,
                });
            }
            _ => return Err(Aarch64DecodeError::Unsupported),
        };
        Ok(Aarch64Instruction::FpUnary {
            operation,
            format,
            source,
            destination,
        })
    }

    fn integer(word: u32, format_bits: u32, vector: u8, general: u8) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let rmode = word >> 19 & 3;
        let opcode = word >> 16 & 7;
        let integer_wide = word >> 31 != 0;
        if matches!(opcode, 6 | 7) {
            let (format, top_half) = match (integer_wide, format_bits, rmode) {
                (false, 0, 0) => (FpFormat::Single, false),
                (true, 1, 0) => (FpFormat::Double, false),
                (true, 2, 1) => (FpFormat::Double, true),
                _ => return Err(Aarch64DecodeError::Reserved),
            };
            return Ok(Aarch64Instruction::FpGeneralMove {
                format,
                general: if opcode == 6 { general } else { vector },
                vector: if opcode == 6 { vector } else { general },
                general_to_fp: opcode == 7,
                top_half,
            });
        }
        let format = Self::format(format_bits)?;
        if matches!(opcode, 2 | 3) && rmode == 0 {
            return Ok(Aarch64Instruction::FpConvert {
                format,
                fp_to_integer: false,
                signed: opcode == 2,
                integer_wide,
                rounding: FpRoundingMode::Current,
                source: vector,
                destination: general,
            });
        }
        if opcode <= 1 || matches!(opcode, 4 | 5) {
            if opcode >= 4 && rmode != 0 {
                return Err(Aarch64DecodeError::Reserved);
            }
            let rounding = if opcode >= 4 {
                FpRoundingMode::NearestAway
            } else {
                [
                    FpRoundingMode::NearestEven,
                    FpRoundingMode::PositiveInfinity,
                    FpRoundingMode::NegativeInfinity,
                    FpRoundingMode::Zero,
                ][rmode as usize]
            };
            return Ok(Aarch64Instruction::FpConvert {
                format,
                fp_to_integer: true,
                signed: opcode & 1 == 0,
                integer_wide,
                rounding,
                source: vector,
                destination: general,
            });
        }
        Err(Aarch64DecodeError::Reserved)
    }

    fn format(value: u32) -> Result<FpFormat, Aarch64DecodeError> {
        match value {
            0 => Ok(FpFormat::Single),
            1 => Ok(FpFormat::Double),
            3 => Ok(FpFormat::Half),
            _ => Err(Aarch64DecodeError::Reserved),
        }
    }
}
