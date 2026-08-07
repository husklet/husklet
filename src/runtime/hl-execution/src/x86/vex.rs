use crate::x86::VexImmediateShift;
use crate::x86::VexOperation;
use crate::x86::{FmaForm, FmaOperation};
use crate::{
    DecodedInstruction, Encoding, FloatWidth, ScalarInstruction, ScalarIrError, ScalarOperand, ScalarRegister,
    VectorSource,
};

pub(crate) struct Decoder;

impl Decoder {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let Encoding::Vex {
            map,
            pp,
            length,
            w,
            source,
            rex,
        } = decoded.encoding
        else {
            return Err(ScalarIrError::Invalid);
        };
        if decoded.prefixes.lock || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        match (map, decoded.opcode, pp) {
            (2, 0xf5, 2 | 3) if length == 0 => Ok(ScalarInstruction::TransferBits {
                destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                source: ScalarRegister::General(source),
                mask: decoded
                    .register_operand
                    .map(|r| crate::ScalarOperand::Register(ScalarRegister::General(r)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
                deposit: pp == 3,
            }),
            (2, 0xf6, 3) if length == 0 => Ok(ScalarInstruction::MultiplyExtended {
                high: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                low: ScalarRegister::General(source),
                source: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
            }),
            (2, 0xf7, 1..=3) if length == 0 => Ok(ScalarInstruction::VariableShift {
                operation: match pp {
                    1 => crate::x86::VariableShift::Left,
                    2 => crate::x86::VariableShift::ArithmeticRight,
                    3 => crate::x86::VariableShift::LogicalRight,
                    _ => unreachable!(),
                },
                destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                source: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
                count: ScalarRegister::General(source),
            }),
            (2, 0xf5, 0) if length == 0 => Ok(ScalarInstruction::ZeroHighBits {
                destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                source: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
                index: ScalarRegister::General(source),
            }),
            (2, 0xf2, 0) if length == 0 => Ok(ScalarInstruction::AndNotGeneral {
                destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                first: ScalarRegister::General(source),
                second: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
            }),
            (2, 0xf3, 0) if length == 0 && !rex.r => Ok(ScalarInstruction::IsolateBit {
                operation: match decoded.raw_reg.ok_or(ScalarIrError::Invalid)? {
                    1 => crate::x86::BitIsolation::Reset,
                    2 => crate::x86::BitIsolation::Mask,
                    3 => crate::x86::BitIsolation::Isolate,
                    _ => return Err(ScalarIrError::Invalid),
                },
                destination: ScalarRegister::General(source),
                source: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
            }),
            (3, 0xf0, 3) if length == 0 && source == 0 => Ok(ScalarInstruction::RotateRightNoFlags {
                destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                source: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
                count: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
            }),
            (1, 0x77, _) if decoded.modrm.is_none() => Ok(if length == 0 {
                ScalarInstruction::VexZeroUpper
            } else {
                ScalarInstruction::VexZeroAll
            }),
            (1, 0x6e, 1) if length == 0 && source == 0 => Ok(ScalarInstruction::VexGeneralToVector {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                source: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
                wide: w,
            }),
            (1, 0x7e, 2) if length == 0 && source == 0 => Ok(ScalarInstruction::VexQword {
                vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                operand: Self::source(decoded)?,
                store: false,
            }),
            (1, 0xd6, 1) if length == 0 && source == 0 => Ok(ScalarInstruction::VexQword {
                vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                operand: Self::source(decoded)?,
                store: true,
            }),
            (1, 0x12 | 0x16, 0 | 1) if length == 0 => Ok(ScalarInstruction::VexHalfMove {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: source,
                second: Self::source(decoded)?,
                high: decoded.opcode == 0x16,
            }),
            (1, 0x13 | 0x17, 0 | 1) if length == 0 && source == 0 && decoded.address.is_some() => {
                Ok(ScalarInstruction::VexHalfStore {
                    source: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    address: decoded.address.ok_or(ScalarIrError::Invalid)?,
                    high: decoded.opcode == 0x17,
                })
            }
            (1, 0x50, 0 | 1) | (1, 0xd7, 1) if source == 0 && decoded.register_operand.is_some() => {
                Ok(ScalarInstruction::VexMask {
                    destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                    source: decoded.register_operand.ok_or(ScalarIrError::Invalid)?,
                    lane: match (decoded.opcode, pp) {
                        (0x50, 0) => 4,
                        (0x50, 1) => 8,
                        _ => 1,
                    },
                    wide: length != 0,
                })
            }
            (1, 0x10 | 0x11 | 0x28 | 0x29, 0 | 1) | (1, 0x6f | 0x7f, 1 | 2) => {
                Ok(ScalarInstruction::VexVectorTransport {
                    vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    operand: Self::source(decoded)?,
                    store: matches!(decoded.opcode, 0x11 | 0x29 | 0x7f),
                    wide: length != 0,
                })
            }
            (1, 0xf0, 3) if source == 0 && decoded.address.is_some() => Ok(ScalarInstruction::VexVectorTransport {
                vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                operand: Self::source(decoded)?,
                store: false,
                wide: length != 0,
            }),
            (1, 0xe6, 1..=3) if source == 0 => Ok(ScalarInstruction::VexPackedDoubleConvert {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                source: Self::source(decoded)?,
                from_integer: pp == 2,
                truncate: pp == 1,
                wide: length != 0,
            }),
            (1, 0x10 | 0x11, 2 | 3) if length == 0 && decoded.register_operand.is_some() => {
                let (destination, second) = if decoded.opcode == 0x10 {
                    (decoded.register.ok_or(ScalarIrError::Invalid)?, Self::source(decoded)?)
                } else {
                    (
                        decoded.register_operand.ok_or(ScalarIrError::Invalid)?,
                        VectorSource::Register(decoded.register.ok_or(ScalarIrError::Invalid)?),
                    )
                };
                Ok(ScalarInstruction::VexScalarMerge {
                    destination,
                    first: source,
                    second,
                    double: pp == 3,
                })
            }
            (1, 0x10, 2 | 3) if length == 0 && decoded.address.is_some() => Ok(ScalarInstruction::VexScalarLoad {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                source: Self::source(decoded)?,
                double: pp == 3,
            }),
            (1, 0x11, 2 | 3) if length == 0 && decoded.address.is_some() => Ok(ScalarInstruction::VectorLaneExtract {
                source: decoded.register.ok_or(ScalarIrError::Invalid)?,
                destination: ScalarOperand::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?),
                bytes: if pp == 3 { 8 } else { 4 },
                lane: 0,
            }),
            (1, 0x54..=0x57, 0 | 1) => Self::binary(
                decoded,
                source,
                length,
                match decoded.opcode {
                    0x54 => VexOperation::And,
                    0x55 => VexOperation::AndNot,
                    0x56 => VexOperation::Or,
                    _ => VexOperation::Xor,
                },
            ),
            (1, 0xdb | 0xdf | 0xeb | 0xef, 1) => Self::binary(
                decoded,
                source,
                length,
                match decoded.opcode {
                    0xdb => VexOperation::And,
                    0xdf => VexOperation::AndNot,
                    0xeb => VexOperation::Or,
                    _ => VexOperation::Xor,
                },
            ),
            (1, 0x58 | 0x59 | 0x5c..=0x5f, 0..=3) if length == 0 || pp < 2 => {
                Ok(ScalarInstruction::VexFloatArithmetic {
                    operation: match decoded.opcode {
                        0x58 => crate::FloatArithmetic::Add,
                        0x59 => crate::FloatArithmetic::Multiply,
                        0x5c => crate::FloatArithmetic::Subtract,
                        0x5d => crate::FloatArithmetic::Minimum,
                        0x5e => crate::FloatArithmetic::Divide,
                        0x5f => crate::FloatArithmetic::Maximum,
                        _ => unreachable!(),
                    },
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    first: source,
                    second: Self::source(decoded)?,
                    format: if matches!(pp, 1 | 3) {
                        FloatWidth::Double
                    } else {
                        FloatWidth::Single
                    },
                    scalar: pp >= 2,
                    wide: length != 0,
                })
            }
            (1, 0x51, 0..=3) if (length == 0 || pp < 2) && (pp >= 2 || source == 0) => {
                Ok(ScalarInstruction::VexFloatArithmetic {
                    operation: crate::FloatArithmetic::SquareRoot,
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    first: source,
                    second: Self::source(decoded)?,
                    format: if matches!(pp, 1 | 3) {
                        FloatWidth::Double
                    } else {
                        FloatWidth::Single
                    },
                    scalar: pp >= 2,
                    wide: length != 0,
                })
            }
            (1, 0x52 | 0x53, 0 | 2) if (length == 0 || pp == 0) && (pp == 2 || source == 0) => {
                Ok(ScalarInstruction::VexFloatArithmetic {
                    operation: if decoded.opcode == 0x52 {
                        crate::FloatArithmetic::ReciprocalSquareRoot
                    } else {
                        crate::FloatArithmetic::Reciprocal
                    },
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    first: source,
                    second: Self::source(decoded)?,
                    format: FloatWidth::Single,
                    scalar: pp == 2,
                    wide: length != 0,
                })
            }
            (2, 0x96 | 0x97 | 0xa6 | 0xa7 | 0xb6 | 0xb7, 1) => {
                if length > 1 {
                    return Err(ScalarIrError::Invalid);
                }
                Ok(ScalarInstruction::VexFma {
                    operation: if decoded.opcode & 1 == 0 {
                        FmaOperation::AddSubtract
                    } else {
                        FmaOperation::SubtractAdd
                    },
                    form: Self::fma_form(decoded.opcode),
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    first: source,
                    second: Self::source(decoded)?,
                    format: if w { FloatWidth::Double } else { FloatWidth::Single },
                    scalar: false,
                    wide: length != 0,
                })
            }
            (2, 0x98..=0xbf, 1) => {
                let scalar = decoded.opcode & 1 != 0;
                if scalar && length != 0 {
                    return Err(ScalarIrError::Invalid);
                }
                let operation = match decoded.opcode & 0x0e {
                    0x08 => FmaOperation::Add,
                    0x0a => FmaOperation::Subtract,
                    0x0c => FmaOperation::NegativeAdd,
                    0x0e => FmaOperation::NegativeSubtract,
                    _ => return Err(ScalarIrError::Unsupported),
                };
                Ok(ScalarInstruction::VexFma {
                    operation,
                    form: Self::fma_form(decoded.opcode),
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    first: source,
                    second: Self::source(decoded)?,
                    format: if w { FloatWidth::Double } else { FloatWidth::Single },
                    scalar,
                    wide: length != 0,
                })
            }
            (1, 0x2a, 2 | 3) if length == 0 => Ok(ScalarInstruction::ConvertIntegerFloat {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                source: decoded
                    .register_operand
                    .map(|register| crate::ScalarOperand::Register(ScalarRegister::General(register)))
                    .or_else(|| decoded.address.map(crate::ScalarOperand::Memory))
                    .ok_or(ScalarIrError::Invalid)?,
                wide: w,
                format: if pp == 2 {
                    FloatWidth::Single
                } else {
                    FloatWidth::Double
                },
                merge: Some(source),
            }),
            (1, 0x70, 1) if source == 0 => Self::binary(decoded, source, length, VexOperation::ShuffleDword),
            (1, 0x70, 2 | 3) if source == 0 => Self::binary(
                decoded,
                source,
                length,
                VexOperation::ShuffleWord { high: pp == 2 },
            ),
            (1, 0x2e | 0x2f, 0 | 1) if source == 0 => Ok(ScalarInstruction::VectorScalarCompare {
                left: decoded.register.ok_or(ScalarIrError::Invalid)?,
                right: Self::source(decoded)?,
                format: if pp == 1 { FloatWidth::Double } else { FloatWidth::Single },
                signaling_only: decoded.opcode == 0x2e,
            }),
            (1, 0xc6, 0) => Self::binary(decoded, source, length, VexOperation::ShuffleSingle),
            (1, 0xc6, 1) => Self::binary(decoded, source, length, VexOperation::ShuffleDouble),
            (1, 0x5b, 0..=2) if source == 0 => Ok(ScalarInstruction::VexDwordToSingle {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                source: Self::source(decoded)?,
                wide: length != 0,
                to_integer: pp != 0,
                truncate: pp == 2,
            }),
            (1, 0x5a, 0..=3) if (pp < 2 && source == 0) || (pp >= 2 && length == 0) => {
                Ok(ScalarInstruction::VexFloatWidth {
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    first: source,
                    source: Self::source(decoded)?,
                    destination_format: if matches!(pp, 0 | 2) {
                        FloatWidth::Double
                    } else {
                        FloatWidth::Single
                    },
                    packed: pp < 2,
                    wide: length != 0,
                })
            }
            (1, 0xc2, 0..=3) => Ok(ScalarInstruction::VexCompare {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: source,
                second: Self::source(decoded)?,
                format: if matches!(pp, 1 | 3) {
                    FloatWidth::Double
                } else {
                    FloatWidth::Single
                },
                scalar: matches!(pp, 2 | 3),
                wide: length != 0,
                predicate: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
            }),
            (3, 0x08..=0x0b, 1)
                if (decoded.opcode < 0x0a || length == 0) && (decoded.opcode >= 0x0a || source == 0) =>
            {
                Ok(ScalarInstruction::VexRound {
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    first: source,
                    source: Self::source(decoded)?,
                    format: if decoded.opcode & 1 == 0 {
                        FloatWidth::Single
                    } else {
                        FloatWidth::Double
                    },
                    scalar: decoded.opcode >= 0x0a,
                    wide: length != 0,
                    control: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
                })
            }
            (3, 0x4a..=0x4c, 1) => Ok(ScalarInstruction::VexBlend {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: source,
                second: Self::source(decoded)?,
                mask: (decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8 >> 4) & 15,
                lane: match decoded.opcode {
                    0x4a => 4,
                    0x4b => 8,
                    _ => 1,
                },
                wide: length != 0,
            }),
            (3, 0x02 | 0x0c..=0x0e, 1) if !w => Self::binary(
                decoded,
                source,
                length,
                match decoded.opcode {
                    0x0e => VexOperation::BlendWord,
                    0x0d => VexOperation::BlendQword,
                    _ => VexOperation::BlendDword,
                },
            ),
            (2, 0x13, 1) if !w && source == 0 => Ok(ScalarInstruction::VexHalfWiden {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                source: Self::source(decoded)?,
                wide: length != 0,
            }),
            (2, 0x00, 1) => Self::binary(decoded, source, length, VexOperation::ShuffleByte),
            (3, 0x1d, 1) if !w && source == 0 => Ok(ScalarInstruction::VexHalfNarrow {
                source: decoded.register.ok_or(ScalarIrError::Invalid)?,
                destination: Self::source(decoded)?,
                wide: length != 0,
                control: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
            }),
            (1, 0x7c | 0x7d | 0xd0, 1 | 3) => Ok(ScalarInstruction::VexPairArithmetic {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: source,
                second: Self::source(decoded)?,
                format: if pp == 1 {
                    FloatWidth::Double
                } else {
                    FloatWidth::Single
                },
                subtract: decoded.opcode == 0x7d,
                alternating: decoded.opcode == 0xd0,
                wide: length != 0,
            }),
            (1, 0x12, 3) => Self::binary(decoded, source, length, VexOperation::DuplicateDouble),
            (1, 0x12, 2) => Self::binary(decoded, source, length, VexOperation::DuplicateLowSingle),
            (1, 0x16, 2) => Self::binary(decoded, source, length, VexOperation::DuplicateHighSingle),
            (3, 0x06 | 0x46, 1) if length != 0 && !w => Self::binary(decoded, source, length, VexOperation::Permute128),
            (3, 0x00 | 0x01, 1) if length != 0 && w && source == 0 => {
                Self::binary(decoded, source, length, VexOperation::PermuteQword)
            }
            (3, 0x04 | 0x05, 1) if source == 0 => Self::binary(
                decoded,
                source,
                length,
                if decoded.opcode == 0x04 {
                    VexOperation::PermuteLaneDword { variable: false }
                } else {
                    VexOperation::PermuteLaneQword { variable: false }
                },
            ),
            (3, 0x21, 1) if length == 0 => Ok(ScalarInstruction::VexInsertSingle {
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: source,
                second: Self::source(decoded)?,
                control: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
            }),
            (2, 0x0c | 0x0d, 1) => Self::binary(
                decoded,
                source,
                length,
                if decoded.opcode == 0x0c {
                    VexOperation::PermuteLaneDword { variable: true }
                } else {
                    VexOperation::PermuteLaneQword { variable: true }
                },
            ),
            (1, 0xfc..=0xfe | 0xd4 | 0xf8..=0xfb, 1) => Self::binary(
                decoded,
                source,
                length,
                match decoded.opcode {
                    0xfc => VexOperation::AddByte,
                    0xfd => VexOperation::AddWord,
                    0xfe => VexOperation::AddDword,
                    0xd4 => VexOperation::AddQword,
                    0xf8 => VexOperation::SubtractByte,
                    0xf9 => VexOperation::SubtractWord,
                    0xfa => VexOperation::SubtractDword,
                    _ => VexOperation::SubtractQword,
                },
            ),
            (1, 0xd8..=0xd9 | 0xdc..=0xdd | 0xe8..=0xe9 | 0xec..=0xed, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Saturating {
                    subtract: matches!(decoded.opcode, 0xd8 | 0xd9 | 0xe8 | 0xe9),
                    unsigned: matches!(decoded.opcode, 0xd8 | 0xd9 | 0xdc | 0xdd),
                    word: decoded.opcode & 1 != 0,
                },
            ),
            (1, 0xda | 0xde | 0xea | 0xee, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Extrema {
                    maximum: matches!(decoded.opcode, 0xde | 0xee),
                    unsigned: matches!(decoded.opcode, 0xda | 0xde),
                    bytes: if matches!(decoded.opcode, 0xea | 0xee) { 2 } else { 1 },
                },
            ),
            (1, 0xe0 | 0xe3, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Average {
                    word: decoded.opcode == 0xe3,
                },
            ),
            (1, 0xf5, 1) => Self::binary(decoded, source, length, VexOperation::MultiplyAddWords),
            (1, 0xf6, 1) => Self::binary(decoded, source, length, VexOperation::SumAbsoluteDifferences),
            (1, 0x63 | 0x67 | 0x6b, 1) => Self::binary(
                decoded,
                source,
                length,
                match decoded.opcode {
                    0x63 => VexOperation::PackSignedWordByte,
                    0x67 => VexOperation::PackUnsignedWordByte,
                    _ => VexOperation::PackSignedDwordWord,
                },
            ),
            (2, 0x2b, 1) => Self::binary(decoded, source, length, VexOperation::PackUnsignedDwordWord),
            (1, 0x60..=0x62 | 0x68..=0x6a | 0x6c..=0x6d, 1) => {
                let operation = match decoded.opcode {
                    0x60 => VexOperation::UnpackLowByte,
                    0x61 => VexOperation::UnpackLowWord,
                    0x62 => VexOperation::UnpackLowDword,
                    0x6c => VexOperation::UnpackLowQword,
                    0x68 => VexOperation::UnpackHighByte,
                    0x69 => VexOperation::UnpackHighWord,
                    0x6a => VexOperation::UnpackHighDword,
                    0x6d => VexOperation::UnpackHighQword,
                    _ => return Err(ScalarIrError::Invalid),
                };
                Self::binary(decoded, source, length, operation)
            }
            (1, 0x14 | 0x15, 0 | 1) => Self::binary(
                decoded,
                source,
                length,
                match (decoded.opcode, pp) {
                    (0x14, 0) => VexOperation::UnpackLowDword,
                    (0x14, 1) => VexOperation::UnpackLowQword,
                    (0x15, 0) => VexOperation::UnpackHighDword,
                    (0x15, 1) => VexOperation::UnpackHighQword,
                    _ => unreachable!(),
                },
            ),
            (2, 0x40, 1) if !w => Self::binary(decoded, source, length, VexOperation::MultiplyLowDword),
            (3, 0x40, 1) => Self::binary(decoded, source, length, VexOperation::DotSingle),
            (3, 0x41, 1) if length == 0 => Self::binary(decoded, source, length, VexOperation::DotDouble),
            (3, 0x44, 1) if length == 0 && !w => Self::binary(decoded, source, length, VexOperation::CarrylessMultiply),
            (3, 0x42, 1) if !w => Self::binary(decoded, source, length, VexOperation::MultipleSad),
            (2, 0x38..=0x3f, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Extrema {
                    maximum: decoded.opcode >= 0x3c,
                    unsigned: matches!(decoded.opcode, 0x3a | 0x3b | 0x3e | 0x3f),
                    bytes: match decoded.opcode & 3 {
                        0 => 1,
                        2 => 2,
                        _ => 4,
                    },
                },
            ),
            (2, 0x04, 1) => Self::binary(decoded, source, length, VexOperation::MultiplyAddBytes),
            (2, 0x01..=0x03 | 0x05..=0x07, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Horizontal {
                    subtract: decoded.opcode >= 0x05,
                    saturating: matches!(decoded.opcode, 0x03 | 0x07),
                    dword: matches!(decoded.opcode, 0x02 | 0x06),
                },
            ),
            (2, 0x08..=0x0a, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Sign {
                    bytes: 1 << (decoded.opcode - 0x08),
                },
            ),
            (2, 0x1c..=0x1e, 1) if source == 0 => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Absolute {
                    bytes: 1 << (decoded.opcode - 0x1c),
                },
            ),
            (2, 0x0b, 1) => Self::binary(decoded, source, length, VexOperation::MultiplyHighRoundWord),
            (2, 0x41, 1) if length == 0 && source == 0 => {
                Self::binary(decoded, source, length, VexOperation::HorizontalMinimumWord)
            }
            (1, 0xd5 | 0xe4 | 0xe5 | 0xf4, 1) => Self::binary(
                decoded,
                source,
                length,
                match decoded.opcode {
                    0xd5 => VexOperation::MultiplyLowWord,
                    0xe4 => VexOperation::MultiplyHighWordUnsigned,
                    0xe5 => VexOperation::MultiplyHighWordSigned,
                    _ => VexOperation::MultiplyDwordUnsigned,
                },
            ),
            (2, 0x28, 1) if !w => Self::binary(decoded, source, length, VexOperation::MultiplyDwordSigned),
            (2, 0x16 | 0x36, 1) if length != 0 && !w => {
                Self::binary(decoded, source, length, VexOperation::PermuteDword)
            }
            (2, 0x45..=0x47, 1) => {
                let operation = match (decoded.opcode, w) {
                    (0x45, false) => VexOperation::ShiftRightVariableDword,
                    (0x45, true) => VexOperation::ShiftRightVariableQword,
                    (0x46, false) => VexOperation::ShiftArithmeticVariableDword,
                    (0x46, true) => return Err(ScalarIrError::Invalid),
                    (0x47, false) => VexOperation::ShiftLeftVariableDword,
                    (0x47, true) => VexOperation::ShiftLeftVariableQword,
                    _ => unreachable!(),
                };
                Self::binary(decoded, source, length, operation)
            }
            (2, 0x90..=0x93, 1) => {
                let destination = decoded.register.ok_or(ScalarIrError::Invalid)?;
                let address = decoded.address.ok_or(ScalarIrError::Invalid)?;
                let index = address.index.ok_or(ScalarIrError::Invalid)?;
                if destination == source || destination == index || source == index {
                    return Err(ScalarIrError::Invalid);
                }
                Ok(ScalarInstruction::VexGather {
                    destination,
                    mask: source,
                    index,
                    address,
                    element: if w { 8 } else { 4 },
                    index_bytes: if matches!(decoded.opcode, 0x90 | 0x92) { 4 } else { 8 },
                    wide: length != 0,
                })
            }
            (2, 0x78..=0x79 | 0x58..=0x59, 1) if !w => Self::binary(
                decoded,
                source,
                length,
                match decoded.opcode {
                    0x78 => VexOperation::BroadcastByte,
                    0x79 => VexOperation::BroadcastWord,
                    0x58 => VexOperation::BroadcastDword,
                    _ => VexOperation::BroadcastQword,
                },
            ),
            (2, 0x18, 1) => Self::binary(decoded, source, length, VexOperation::BroadcastDword),
            (2, 0x19, 1) => Self::binary(decoded, source, length, VexOperation::BroadcastQword),
            (2, 0x2a, 1) if source == 0 && decoded.address.is_some() => Ok(ScalarInstruction::VexVectorTransport {
                vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                operand: Self::source(decoded)?,
                store: false,
                wide: length != 0,
            }),
            (2, 0x17 | 0x0e | 0x0f, 1) if source == 0 => Ok(ScalarInstruction::VexVectorTest {
                left: decoded.register.ok_or(ScalarIrError::Invalid)?,
                right: Self::source(decoded)?,
                lane: match decoded.opcode {
                    0x0e => 4,
                    0x0f => 8,
                    _ => 0,
                },
                wide: length != 0,
            }),
            (2, 0x2c..=0x2f, 1) if !w && decoded.address.is_some() => Ok(ScalarInstruction::VexMaskedMemory {
                vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                mask: source,
                address: decoded.address.ok_or(ScalarIrError::Invalid)?,
                lane: if decoded.opcode & 1 == 0 { 4 } else { 8 },
                store: decoded.opcode >= 0x2e,
                wide: length != 0,
            }),
            (2, 0x8c | 0x8e, 1) if decoded.address.is_some() => Ok(ScalarInstruction::VexMaskedMemory {
                vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                mask: source,
                address: decoded.address.ok_or(ScalarIrError::Invalid)?,
                lane: if w { 8 } else { 4 },
                store: decoded.opcode == 0x8e,
                wide: length != 0,
            }),
            (2, 0xdb, 1) if length == 0 && !w && source == 0 => Ok(ScalarInstruction::VexAes {
                operation: crate::x86::X86AesOperation::InverseMix,
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: 0,
                second: Self::source(decoded)?,
            }),
            (2, 0xdc..=0xdf, 1) if length == 0 && !w => Ok(ScalarInstruction::VexAes {
                operation: match decoded.opcode {
                    0xdc => crate::x86::X86AesOperation::Encrypt,
                    0xdd => crate::x86::X86AesOperation::EncryptLast,
                    0xde => crate::x86::X86AesOperation::Decrypt,
                    _ => crate::x86::X86AesOperation::DecryptLast,
                },
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: source,
                second: Self::source(decoded)?,
            }),
            (3, 0xdf, 1) if length == 0 && !w && source == 0 => Ok(ScalarInstruction::VexAes {
                operation: crate::x86::X86AesOperation::KeyAssist(
                    decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
                ),
                destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                first: 0,
                second: Self::source(decoded)?,
            }),
            (2, 0x1a | 0x5a, 1) if length != 0 && !w && source == 0 && decoded.register_operand.is_none() => {
                Self::binary(decoded, source, length, VexOperation::Broadcast128)
            }
            (3, 0x18 | 0x38, 1) if length != 0 && !w => Self::binary(decoded, source, length, VexOperation::Insert128),
            (2, 0x20..=0x25 | 0x30..=0x35, 1) if source == 0 => {
                let (from, to) = match decoded.opcode & 0x0f {
                    0 => (1, 2),
                    1 => (1, 4),
                    2 => (1, 8),
                    3 => (2, 4),
                    4 => (2, 8),
                    _ => (4, 8),
                };
                Self::binary(
                    decoded,
                    source,
                    length,
                    VexOperation::Widen {
                        from,
                        to,
                        signed: decoded.opcode < 0x30,
                    },
                )
            }
            (3, 0x0f, 1) => Self::binary(decoded, source, length, VexOperation::Align),
            (1, 0x74..=0x76, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Compare {
                    comparison: crate::VectorComparison::Equal,
                    lane: 1 << (decoded.opcode - 0x74),
                },
            ),
            (1, 0x64..=0x66, 1) => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Compare {
                    comparison: crate::VectorComparison::SignedGreater,
                    lane: 1 << (decoded.opcode - 0x64),
                },
            ),
            (2, 0x29, 1) if !w => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Compare {
                    comparison: crate::VectorComparison::Equal,
                    lane: 8,
                },
            ),
            (2, 0x37, 1) if !w => Self::binary(
                decoded,
                source,
                length,
                VexOperation::Compare {
                    comparison: crate::VectorComparison::SignedGreater,
                    lane: 8,
                },
            ),
            (1, 0x2b, 0 | 1) | (1, 0xe7, 1) if source == 0 && decoded.address.is_some() => {
                Ok(ScalarInstruction::VexVectorTransport {
                    vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    operand: Self::source(decoded)?,
                    store: true,
                    wide: length != 0,
                })
            }
            (1, 0xd1..=0xd3 | 0xe1..=0xe2 | 0xf1..=0xf3, 1) => {
                let operation = match decoded.opcode {
                    0xd1..=0xd3 => VexImmediateShift::LogicalRight,
                    0xe1..=0xe2 => VexImmediateShift::ArithmeticRight,
                    0xf1..=0xf3 => VexImmediateShift::LogicalLeft,
                    _ => unreachable!(),
                };
                Ok(ScalarInstruction::VexScalarCountShift {
                    operation,
                    destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
                    source,
                    count: Self::source(decoded)?,
                    lane: match decoded.opcode {
                        0xd1 | 0xe1 | 0xf1 => 2,
                        0xd2 | 0xe2 | 0xf2 => 4,
                        _ => 8,
                    },
                    wide: length != 0,
                })
            }
            (1, 0x71..=0x73, 1) => {
                let extension = decoded.raw_reg.ok_or(ScalarIrError::Invalid)?;
                let operation = match (decoded.opcode, extension) {
                    (0x71..=0x73, 2) => VexImmediateShift::LogicalRight,
                    (0x71 | 0x72, 4) => VexImmediateShift::ArithmeticRight,
                    (0x71..=0x73, 6) => VexImmediateShift::LogicalLeft,
                    (0x73, 3) => VexImmediateShift::ByteRight,
                    (0x73, 7) => VexImmediateShift::ByteLeft,
                    _ => return Err(ScalarIrError::Invalid),
                };
                Ok(ScalarInstruction::VexImmediateShift {
                    operation,
                    destination: source,
                    source: decoded.register_operand.ok_or(ScalarIrError::Invalid)?,
                    lane: match decoded.opcode {
                        0x71 => 2,
                        0x72 => 4,
                        _ => 8,
                    },
                    wide: length != 0,
                    count: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
                })
            }
            (1, 0x7e, 1) => Ok(ScalarInstruction::VexVectorToGeneral {
                source: decoded.register.ok_or(ScalarIrError::Invalid)?,
                destination: ScalarRegister::General(decoded.register_operand.ok_or(ScalarIrError::Invalid)?),
                wide: w,
            }),
            (3, 0x19 | 0x39, 1) if length != 0 && !w && source == 0 => Ok(ScalarInstruction::VexExtract128 {
                source: decoded.register.ok_or(ScalarIrError::Invalid)?,
                destination: Self::source(decoded)?,
                high: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 & 1 != 0,
            }),
            (1, 0x2c | 0x2d, 2 | 3) if length == 0 => Ok(ScalarInstruction::ConvertFloatInteger {
                destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
                source: Self::source(decoded)?,
                wide: w,
                format: if pp == 2 {
                    FloatWidth::Single
                } else {
                    FloatWidth::Double
                },
                truncate: decoded.opcode == 0x2c,
            }),
            _ => Err(ScalarIrError::Unsupported),
        }
    }

    fn binary(
        decoded: &DecodedInstruction,
        first: u8,
        length: u8,
        operation: VexOperation,
    ) -> Result<ScalarInstruction, ScalarIrError> {
        Ok(ScalarInstruction::VexBinary {
            operation,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            first,
            second: Self::source(decoded)?,
            wide: length != 0,
            immediate: decoded.immediate.map_or(0, |value| value.0 as u8),
        })
    }

    fn source(decoded: &DecodedInstruction) -> Result<VectorSource, ScalarIrError> {
        decoded
            .register_operand
            .map(VectorSource::Register)
            .or_else(|| decoded.address.map(VectorSource::Memory))
            .ok_or(ScalarIrError::Invalid)
    }

    fn fma_form(opcode: u8) -> FmaForm {
        match opcode >> 4 {
            9 => FmaForm::Form132,
            10 => FmaForm::Form213,
            _ => FmaForm::Form231,
        }
    }
}
