use std::{error::Error as StdError, fmt};
pub(crate) mod arithmetic;
pub(crate) mod compare;
pub(crate) mod conversion;
mod group;
pub(super) mod ir;
mod operand;
mod prefix;
pub(crate) mod string;
pub(crate) mod transport;
pub(super) mod vector;
use crate::{
    AluOperation, BitScanOperation, BranchCondition, ByteRegister, ControlFlag, DecodedInstruction, Encoding,
    FloatArithmetic, FloatWidth, ScalarInstruction, ScalarIr, ScalarOperand, ScalarRegister, ScalarWidth,
    VectorArithmetic, VectorDecode, VectorOperation, VectorPackKind, X86Decoder,
};
pub struct Decoder;
impl Decoder {
    pub fn decode(bytes: &[u8], address: u64) -> Result<ScalarIr, Error> {
        let decoded = X86Decoder::decode(bytes).map_err(Error::Structural)?;
        if matches!(decoded.encoding, Encoding::Vex { .. }) {
            let instruction = crate::x86::vex::Decoder::decode(&decoded)?;
            return Ok(ScalarIr {
                length: decoded.length,
                width: if matches!(
                    instruction,
                    ScalarInstruction::RotateRightNoFlags { .. }
                        | ScalarInstruction::IsolateBit { .. }
                        | ScalarInstruction::AndNotGeneral { .. }
                        | ScalarInstruction::ZeroHighBits { .. }
                        | ScalarInstruction::VariableShift { .. }
                        | ScalarInstruction::MultiplyExtended { .. }
                        | ScalarInstruction::TransferBits { .. }
                ) && matches!(decoded.encoding, Encoding::Vex { w: true, .. })
                {
                    ScalarWidth::Qword
                } else {
                    ScalarWidth::Dword
                },
                instruction,
            });
        }
        let Encoding::Legacy { map, .. } = decoded.encoding else {
            return Err(Error::Unsupported);
        };
        let instruction = match map {
            0 => Self::one_byte(&decoded, address)?,
            1 => Self::two_byte(&decoded, address)?,
            2 if matches!(decoded.opcode, 0xf0 | 0xf1) && decoded.prefixes.repne => {
                let source_width = if decoded.opcode == 0xf0 {
                    ScalarWidth::Byte
                } else {
                    Self::width(&decoded, false)
                };
                ScalarInstruction::Crc32c {
                    destination: Self::general_reg(&decoded)?,
                    source: Self::rm(&decoded, source_width == ScalarWidth::Byte)?,
                    source_width,
                }
            }
            2 if matches!(decoded.opcode, 0xf0 | 0xf1) => ScalarInstruction::EndianMove {
                register: Self::general_reg(&decoded)?,
                address: decoded.address.ok_or(Error::Invalid)?,
                store: decoded.opcode == 0xf1,
            },
            2 if decoded.opcode == 0x00 => VectorDecode::byte_shuffle(&decoded)?,
            2 if matches!(decoded.opcode, 0x01..=0x0b | 0x1c..=0x1e) => VectorDecode::ssse3(&decoded)?,
            2 if decoded.opcode == 0x17 => VectorDecode::test(&decoded)?,
            2 if matches!(decoded.opcode, 0xdb..=0xdf) => crate::x86::vector::Aes::decode(&decoded, map)?,
            2 if matches!(decoded.opcode, 0xc8..=0xcd) => crate::x86::vector::Sha::decode(&decoded, map)?,
            2 if matches!(decoded.opcode, 0x10 | 0x14 | 0x15) => VectorDecode::blend(&decoded, true)?,
            2 if matches!(decoded.opcode, 0x20..=0x25 | 0x28 | 0x30..=0x35 | 0x38..=0x40) => {
                VectorDecode::sse41(&decoded)?
            }
            2 if decoded.opcode == 0x2a && decoded.prefixes.operand_16 && decoded.address.is_some() => {
                ScalarInstruction::VectorTransport {
                    vector: decoded.register.ok_or(Error::Invalid)?,
                    operand: Self::vector_source(&decoded)?,
                    store: false,
                    aligned: true,
                }
            }
            2 if decoded.opcode == 0x41 => VectorDecode::horizontal_minimum(&decoded)?,
            2 if decoded.opcode == 0x2b => ScalarInstruction::VectorPack {
                destination: decoded.register.ok_or(Error::Invalid)?,
                source: Self::vector_source(&decoded)?,
                kind: VectorPackKind::UnsignedWords,
            },
            2 => crate::x86::vector::Compare::decode(&decoded, map)?,
            3 if decoded.opcode == 0x0f => VectorDecode::align(&decoded)?,
            3 if matches!(decoded.opcode, 0x08..=0x0b) => VectorDecode::round(&decoded)?,
            3 if matches!(decoded.opcode, 0x0c..=0x0e) => VectorDecode::blend(&decoded, false)?,
            3 if matches!(decoded.opcode, 0x14..=0x17 | 0x20..=0x22) => Self::lane_transfer(&decoded)?,
            3 if matches!(decoded.opcode, 0x40..=0x42) => VectorDecode::immediate_sse41(&decoded)?,
            3 if decoded.opcode == 0x44 => VectorDecode::carryless_multiply(&decoded)?,
            3 if matches!(decoded.opcode, 0x60..=0x63) => string::PackedString::decode(&decoded)?,
            3 if decoded.opcode == 0xdf => crate::x86::vector::Aes::decode(&decoded, map)?,
            3 if decoded.opcode == 0xcc => crate::x86::vector::Sha::decode(&decoded, map)?,
            _ => return Err(Error::Unsupported),
        };
        prefix::PrefixValidator::validate(&decoded, instruction)?;
        Ok(ScalarIr {
            length: decoded.length,
            width: Self::instruction_width(&decoded, instruction),
            instruction,
        })
    }
    fn lane_transfer(decoded: &DecodedInstruction) -> Result<ScalarInstruction, Error> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(Error::Invalid);
        }
        if decoded.opcode == 0x21 {
            return Ok(ScalarInstruction::VectorInsertSingle {
                destination: decoded.register.ok_or(Error::Invalid)?,
                source: Self::vector_source(decoded)?,
                control: decoded.immediate.ok_or(Error::Invalid)?.0 as u8,
            });
        }
        let bytes = match decoded.opcode {
            0x14 | 0x20 => 1,
            0x15 => 2,
            0x16 | 0x22 if matches!(decoded.encoding, Encoding::Legacy { rex: Some(rex), .. } if rex.w) => 8,
            _ => 4,
        };
        let lane = decoded.immediate.ok_or(Error::Invalid)?.0 as u8 & (16 / bytes - 1);
        let operand = if decoded.raw_mod == Some(3) {
            ScalarOperand::Register(ScalarRegister::General(decoded.register_operand.ok_or(Error::Invalid)?))
        } else {
            ScalarOperand::Memory(decoded.address.ok_or(Error::Invalid)?)
        };
        if matches!(decoded.opcode, 0x20 | 0x22) {
            Ok(ScalarInstruction::VectorLaneInsert {
                destination: decoded.register.ok_or(Error::Invalid)?,
                source: operand,
                bytes,
                lane,
            })
        } else {
            Ok(ScalarInstruction::VectorLaneExtract {
                source: decoded.register.ok_or(Error::Invalid)?,
                destination: operand,
                bytes,
                lane,
            })
        }
    }
    fn one_byte(decoded: &DecodedInstruction, address: u64) -> Result<ScalarInstruction, Error> {
        let op = decoded.opcode;
        if op < 0x40 {
            let operation = Self::alu(op >> 3)?;
            return Self::alu_encoding(decoded, operation, op & 7);
        }
        match op {
            0x50..=0x57 => Ok(ScalarInstruction::Push {
                source: ScalarOperand::Register(ScalarRegister::General((op & 7) | Self::rex_b(decoded))),
            }),
            0x58..=0x5f => Ok(ScalarInstruction::Pop {
                destination: ScalarOperand::Register(ScalarRegister::General((op & 7) | Self::rex_b(decoded))),
            }),
            0x63 => Ok(ScalarInstruction::MoveSignExtend {
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, false)?,
                source_width: if decoded.prefixes.operand_16 {
                    ScalarWidth::Word
                } else {
                    ScalarWidth::Dword
                },
            }),
            0x68 | 0x6a => Ok(ScalarInstruction::Push {
                source: Self::immediate(decoded)?,
            }),
            0x69 | 0x6b => Ok(ScalarInstruction::TruncatedMultiply {
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, false)?,
                multiplier: Some(decoded.immediate.ok_or(Error::Invalid)?.0),
            }),
            0x70..=0x7f => Ok(ScalarInstruction::JumpConditional {
                condition: BranchCondition(op & 15),
                target: Self::target(decoded, address)?,
            }),
            0x80 | 0x81 | 0x83 => Self::group_one(decoded),
            0x84 | 0x85 => Ok(ScalarInstruction::Alu {
                operation: AluOperation::Test,
                destination: Self::rm(decoded, op == 0x84)?,
                source: Self::reg(decoded, op == 0x84)?,
                locked: decoded.prefixes.lock,
            }),
            0x86 | 0x87 => Ok(ScalarInstruction::Exchange {
                destination: Self::rm(decoded, op == 0x86)?,
                source: match Self::reg(decoded, op == 0x86)? {
                    ScalarOperand::Register(register) => register,
                    _ => unreachable!(),
                },
            }),
            0x88 | 0x89 => Ok(ScalarInstruction::Move {
                destination: Self::rm(decoded, op == 0x88)?,
                source: Self::reg(decoded, op == 0x88)?,
            }),
            0x8a | 0x8b => Ok(ScalarInstruction::Move {
                destination: Self::reg(decoded, op == 0x8a)?,
                source: Self::rm(decoded, op == 0x8a)?,
            }),
            0x8c => Ok(ScalarInstruction::ReadSelector {
                destination: Self::rm(decoded, false)?,
                value: match decoded.raw_reg.ok_or(Error::Invalid)? & 7 {
                    1 => 0x33,
                    2 => 0x2b,
                    _ => 0,
                },
            }),
            0x8e => Ok(ScalarInstruction::WriteSelector),
            0x8d => {
                let address = decoded.address.ok_or(Error::Invalid)?;
                Ok(ScalarInstruction::Lea {
                    destination: Self::general_reg(decoded)?,
                    address,
                })
            }
            0x8f if decoded.raw_reg == Some(0) => Ok(ScalarInstruction::Pop {
                destination: Self::rm(decoded, false)?,
            }),
            0x90..=0x97 => {
                let source = ScalarRegister::General((op & 7) | Self::rex_b(decoded));
                if source == ScalarRegister::General(0) {
                    Ok(ScalarInstruction::Nop)
                } else {
                    Ok(ScalarInstruction::AccumulatorExchange { source })
                }
            }
            0x98 => Ok(ScalarInstruction::AccumulatorSignExtend {
                source_width: Self::accumulator_source_width(decoded),
            }),
            0x99 => Ok(ScalarInstruction::AccumulatorHighExtend),
            0x9c => Ok(ScalarInstruction::PushFlags),
            0x9d => Ok(ScalarInstruction::PopFlags),
            0x9e => Ok(ScalarInstruction::FlagsFromAh),
            0x9f => Ok(ScalarInstruction::AhFromFlags),
            0xa0 | 0xa1 => Ok(ScalarInstruction::Move {
                destination: ScalarOperand::Register(if op == 0xa0 {
                    ScalarRegister::Byte(ByteRegister::Low(0))
                } else {
                    ScalarRegister::General(0)
                }),
                source: Self::moffs(decoded)?,
            }),
            0xa2 | 0xa3 => Ok(ScalarInstruction::Move {
                destination: Self::moffs(decoded)?,
                source: ScalarOperand::Register(if op == 0xa2 {
                    ScalarRegister::Byte(ByteRegister::Low(0))
                } else {
                    ScalarRegister::General(0)
                }),
            }),
            0xa4..=0xa7 | 0xaa..=0xaf => Ok(Self::string(decoded, op)),
            0xa8 | 0xa9 => Ok(ScalarInstruction::Alu {
                operation: AluOperation::Test,
                destination: ScalarOperand::Register(if op == 0xa8 {
                    ScalarRegister::Byte(ByteRegister::Low(0))
                } else {
                    ScalarRegister::General(0)
                }),
                source: Self::immediate(decoded)?,
                locked: decoded.prefixes.lock,
            }),
            0xb0..=0xb7 => Ok(ScalarInstruction::Move {
                destination: Self::opcode_reg(decoded, op & 7, true)?,
                source: Self::immediate(decoded)?,
            }),
            0xb8..=0xbf => Ok(ScalarInstruction::Move {
                destination: Self::opcode_reg(decoded, op & 7, false)?,
                source: Self::immediate(decoded)?,
            }),
            0xc2 | 0xc3 => Ok(ScalarInstruction::Return {
                pop_bytes: if op == 0xc2 {
                    decoded.immediate.ok_or(Error::Invalid)?.0 as u16
                } else {
                    0
                },
            }),
            0xcc => Ok(ScalarInstruction::Breakpoint),
            0xcf if decoded.rex().is_some_and(|rex| rex.w) => Ok(ScalarInstruction::Iret),
            0xc9 => Ok(ScalarInstruction::Leave {
                address_32: decoded.prefixes.address_32,
            }),
            0xc6 | 0xc7 if decoded.raw_reg == Some(0) => Ok(ScalarInstruction::Move {
                destination: Self::rm(decoded, op == 0xc6)?,
                source: Self::immediate(decoded)?,
            }),
            0xc0 | 0xc1 | 0xd0..=0xd3 => Self::group_two(decoded),
            0xd7 => Ok(ScalarInstruction::Xlat {
                address_32: decoded.prefixes.address_32,
                segment: decoded.prefixes.segment,
            }),
            0x9b => crate::x86::x87::Control::decode(decoded),
            0xd8 | 0xd9 | 0xda | 0xdb | 0xdc | 0xdd | 0xde | 0xdf => crate::x86::x87::ExtendedMemory::decode(decoded),
            0xe8 => Ok(ScalarInstruction::Call {
                target: Self::target(decoded, address)?,
            }),
            0xe0..=0xe3 => Ok(ScalarInstruction::CountBranch {
                target: Self::target(decoded, address)?,
                address_32: decoded.prefixes.address_32,
                decrement: op != 0xe3,
                zero: match op {
                    0xe0 => Some(false),
                    0xe1 => Some(true),
                    _ => None,
                },
            }),
            0xe9 | 0xeb => Ok(ScalarInstruction::Jump {
                target: Self::target(decoded, address)?,
            }),
            0xf5 => Ok(ScalarInstruction::FlagControl(ControlFlag::ComplementCarry)),
            0xf8 => Ok(ScalarInstruction::FlagControl(ControlFlag::ClearCarry)),
            0xf9 => Ok(ScalarInstruction::FlagControl(ControlFlag::SetCarry)),
            0xfc => Ok(ScalarInstruction::FlagControl(ControlFlag::ClearDirection)),
            0xfd => Ok(ScalarInstruction::FlagControl(ControlFlag::SetDirection)),
            0xff if decoded.raw_reg == Some(2) => Ok(ScalarInstruction::CallIndirect {
                target: Self::rm(decoded, false)?,
            }),
            0xff if decoded.raw_reg == Some(4) => Ok(ScalarInstruction::JumpIndirect {
                target: Self::rm(decoded, false)?,
            }),
            0xff if decoded.raw_reg == Some(6) => Ok(ScalarInstruction::Push {
                source: Self::rm(decoded, false)?,
            }),
            0xfe | 0xff if matches!(decoded.raw_reg, Some(0 | 1)) => Ok(ScalarInstruction::Increment {
                operand: Self::rm(decoded, op == 0xfe)?,
                decrement: decoded.raw_reg == Some(1),
                locked: decoded.prefixes.lock,
            }),
            0xf6 | 0xf7 => Self::group_three(decoded),
            _ => Err(Error::Unsupported),
        }
    }

    fn two_byte(decoded: &DecodedInstruction, address: u64) -> Result<ScalarInstruction, Error> {
        match decoded.opcode {
            0x05 => Ok(ScalarInstruction::Syscall),
            0x0b => Ok(ScalarInstruction::Undefined),
            0x31 => Ok(ScalarInstruction::TimestampCounter { auxiliary: false }),
            0x01 if decoded.modrm == Some(0xf9) => Ok(ScalarInstruction::TimestampCounter { auxiliary: true }),
            0x0d | 0x18..=0x1f => Ok(ScalarInstruction::Nop),
            0xae if decoded.raw_mod == Some(3) && matches!(decoded.raw_reg, Some(5..=7)) => {
                Ok(ScalarInstruction::Nop)
            }
            0xae if matches!(decoded.raw_reg, Some(0..=3)) => crate::x86::fxsave::Fxsave::decode(decoded),
            0x12 | 0x16 if decoded.prefixes.rep || decoded.prefixes.repne => {
                crate::x86::vector::HalfDecode::duplicate(decoded)
            }
            0x12 | 0x13 | 0x16 | 0x17 => crate::x86::vector::HalfDecode::decode(decoded),
            0x14 | 0x15 => crate::x86::vector::HalfDecode::unpack(decoded),
            0x10 | 0x11 if decoded.prefixes.repne || decoded.prefixes.rep => transport::Transport::decode(decoded),
            0x2a if decoded.prefixes.repne || decoded.prefixes.rep => conversion::Conversion::from_integer(decoded),
            0x2a => Ok(ScalarInstruction::MmxConvertToFloat {
                destination: decoded.register.ok_or(Error::Invalid)?,
                source: Self::vector_source(decoded)?,
                double: decoded.prefixes.operand_16,
            }),
            0x2c | 0x2d if decoded.prefixes.repne || decoded.prefixes.rep => {
                conversion::Conversion::to_integer(decoded)
            }
            0x2c | 0x2d => Ok(ScalarInstruction::MmxConvertFromFloat {
                destination: decoded.raw_reg.ok_or(Error::Invalid)?,
                source: Self::vector_source(decoded)?,
                double: decoded.prefixes.operand_16,
                truncate: decoded.opcode == 0x2c,
            }),
            0x51..=0x53 | 0x58 | 0x59 | 0x5c..=0x5f => arithmetic::Arithmetic::decode(decoded),
            0x7c | 0x7d | 0xd0 => arithmetic::Arithmetic::pair_decode(decoded),
            0x5a => conversion::Conversion::width(decoded),
            0x5b => conversion::Conversion::packed_single(decoded),
            0xe6 => conversion::Conversion::packed_double(decoded),
            0xc2 => compare::Comparison::mask_decode(decoded),
            0x2e | 0x2f if !decoded.prefixes.rep && !decoded.prefixes.repne => compare::Comparison::decode(decoded),
            0x50 => crate::x86::vector::Mask::decode(decoded),
            0x10 | 0x11 if !decoded.prefixes.rep && !decoded.prefixes.repne => Ok(ScalarInstruction::VectorTransport {
                vector: decoded.register.ok_or(Error::Invalid)?,
                operand: Self::vector_source(decoded)?,
                store: decoded.opcode == 0x11,
                aligned: false,
            }),
            0x28 | 0x29 if !decoded.prefixes.rep && !decoded.prefixes.repne => Ok(ScalarInstruction::VectorTransport {
                vector: decoded.register.ok_or(Error::Invalid)?,
                operand: Self::vector_source(decoded)?,
                store: decoded.opcode == 0x29,
                aligned: true,
            }),
            0x2b if !decoded.prefixes.rep && !decoded.prefixes.repne && decoded.address.is_some() => {
                Ok(ScalarInstruction::VectorTransport {
                    vector: decoded.register.ok_or(Error::Invalid)?,
                    operand: Self::vector_source(decoded)?,
                    store: true,
                    aligned: true,
                })
            }
            0x40..=0x4f => Ok(ScalarInstruction::ConditionalMove {
                condition: BranchCondition(decoded.opcode & 15),
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, false)?,
            }),
            _ if crate::x86::mmx::Mmx::accepts(decoded) => crate::x86::mmx::Mmx::decode(decoded),
            0x63 | 0x67 | 0x6b => VectorDecode::pack(decoded),
            0x6e | 0x7e if decoded.prefixes.operand_16 => Ok(ScalarInstruction::VectorMove {
                vector: decoded.register.ok_or(Error::Invalid)?,
                scalar: Self::rm(decoded, false)?,
                to_vector: decoded.opcode == 0x6e,
            }),
            0x6f | 0x7f if decoded.prefixes.operand_16 || (decoded.prefixes.rep && !decoded.prefixes.repne) => {
                Ok(ScalarInstruction::VectorTransport {
                    vector: decoded.register.ok_or(Error::Invalid)?,
                    operand: Self::vector_source(decoded)?,
                    store: decoded.opcode == 0x7f,
                    aligned: decoded.prefixes.operand_16,
                })
            }
            0x70 => VectorDecode::shuffle(decoded),
            0xc6 => VectorDecode::shuffle(decoded),
            0xc5 if decoded.raw_mod == Some(3) && !decoded.prefixes.rep && !decoded.prefixes.repne => {
                let lane = decoded.immediate.ok_or(Error::Invalid)?.0 as u8;
                if decoded.prefixes.operand_16 {
                    Ok(ScalarInstruction::VectorLaneExtract {
                        source: decoded.register_operand.ok_or(Error::Invalid)?,
                        destination: ScalarOperand::Register(ScalarRegister::General(
                            decoded.register.ok_or(Error::Invalid)?,
                        )),
                        bytes: 2,
                        lane: lane & 7,
                    })
                } else {
                    Ok(ScalarInstruction::MmxExtractWord {
                        source: decoded.raw_rm.ok_or(Error::Invalid)?,
                        destination: ScalarRegister::General(decoded.register.ok_or(Error::Invalid)?),
                        lane: lane & 3,
                    })
                }
            }
            0x71..=0x73 => VectorDecode::immediate_shift(decoded),
            0x64..=0x66 | 0x74..=0x76 => crate::x86::vector::Compare::decode(decoded, 1),
            0x7e if decoded.prefixes.rep && !decoded.prefixes.repne => Ok(ScalarInstruction::VectorLoad {
                destination: decoded.register.ok_or(Error::Invalid)?,
                source: Self::vector_source(decoded)?,
            }),
            0x60..=0x62 | 0x68..=0x6a | 0x6c | 0x6d if decoded.prefixes.operand_16 => {
                let lane = match decoded.opcode {
                    0x60 | 0x68 => 1,
                    0x61 | 0x69 => 2,
                    0x62 | 0x6a => 4,
                    0x6c | 0x6d => 8,
                    _ => unreachable!(),
                };
                Ok(ScalarInstruction::VectorUnpack {
                    destination: decoded.register.ok_or(Error::Invalid)?,
                    source: Self::vector_source(decoded)?,
                    lane,
                    high: matches!(decoded.opcode, 0x68..=0x6a | 0x6d),
                })
            }
            0xd6 if decoded.prefixes.operand_16 => Ok(ScalarInstruction::VectorStore {
                source: decoded.register.ok_or(Error::Invalid)?,
                destination: Self::vector_source(decoded)?,
            }),
            0xd6 if decoded.raw_mod == Some(3) && (decoded.prefixes.rep ^ decoded.prefixes.repne) => {
                Ok(ScalarInstruction::MmxVector {
                    mmx: if decoded.prefixes.rep {
                        decoded.raw_rm.ok_or(Error::Invalid)?
                    } else {
                        decoded.raw_reg.ok_or(Error::Invalid)?
                    },
                    vector: if decoded.prefixes.rep {
                        decoded.register.ok_or(Error::Invalid)?
                    } else {
                        decoded.register_operand.ok_or(Error::Invalid)?
                    },
                    to_vector: decoded.prefixes.rep,
                })
            }
            0xd6 => Ok(ScalarInstruction::Undefined),
            0xe7 if decoded.prefixes.operand_16 && decoded.address.is_some() => {
                Ok(ScalarInstruction::VectorTransport {
                    vector: decoded.register.ok_or(Error::Invalid)?,
                    operand: Self::vector_source(decoded)?,
                    store: true,
                    aligned: true,
                })
            }
            0xd7
                if decoded.raw_mod == Some(3)
                    && !decoded.prefixes.operand_16
                    && !decoded.prefixes.rep
                    && !decoded.prefixes.repne =>
            {
                Ok(ScalarInstruction::MmxMask {
                    destination: ScalarRegister::General(decoded.register.ok_or(Error::Invalid)?),
                    source: decoded.raw_rm.ok_or(Error::Invalid)?,
                })
            }
            0xd7 => crate::x86::vector::Mask::decode(decoded),
            0xf7 if decoded.raw_mod == Some(3) && !decoded.prefixes.rep && !decoded.prefixes.repne => {
                Ok(ScalarInstruction::VectorMaskedStore {
                    source: if decoded.prefixes.operand_16 {
                        decoded.register.ok_or(Error::Invalid)?
                    } else {
                        decoded.raw_reg.ok_or(Error::Invalid)?
                    },
                    mask: if decoded.prefixes.operand_16 {
                        decoded.register_operand.ok_or(Error::Invalid)?
                    } else {
                        decoded.raw_rm.ok_or(Error::Invalid)?
                    },
                    mmx: !decoded.prefixes.operand_16,
                    address: crate::EffectiveAddress {
                        base: Some(7),
                        address_32: decoded.prefixes.address_32,
                        segment: decoded.prefixes.segment,
                        ..Default::default()
                    },
                })
            }
            0x54..=0x57 if !decoded.prefixes.rep && !decoded.prefixes.repne => {
                let operation = match decoded.opcode {
                    0x54 => VectorOperation::And,
                    0x55 => VectorOperation::AndNot,
                    0x56 => VectorOperation::Or,
                    _ => VectorOperation::Xor,
                };
                Ok(ScalarInstruction::VectorBitwise {
                    operation,
                    destination: decoded.register.ok_or(Error::Invalid)?,
                    source: Self::vector_source(decoded)?,
                })
            }
            0xdb | 0xdf | 0xeb | 0xef if decoded.prefixes.operand_16 => {
                let operation = match decoded.opcode {
                    0xdb => VectorOperation::And,
                    0xdf => VectorOperation::AndNot,
                    0xeb => VectorOperation::Or,
                    _ => VectorOperation::Xor,
                };
                Ok(ScalarInstruction::VectorBitwise {
                    operation,
                    destination: decoded.register.ok_or(Error::Invalid)?,
                    source: Self::vector_source(decoded)?,
                })
            }
            0xd1..=0xd3 | 0xe1 | 0xe2 | 0xf1..=0xf3 if decoded.prefixes.operand_16 => {
                let lane = match decoded.opcode {
                    0xd1 | 0xe1 | 0xf1 => 2,
                    0xd2 | 0xe2 | 0xf2 => 4,
                    _ => 8,
                };
                let kind = match decoded.opcode {
                    0xe1 | 0xe2 => crate::VectorShiftKind::ArithmeticRight,
                    0xd1..=0xd3 => crate::VectorShiftKind::LogicalRight,
                    _ => crate::VectorShiftKind::Left,
                };
                Ok(ScalarInstruction::VectorVariableShift {
                    vector: decoded.register.ok_or(Error::Invalid)?,
                    count: Self::vector_source(decoded)?,
                    lane,
                    kind,
                })
            }
            0xda | 0xde | 0xea | 0xee if decoded.prefixes.operand_16 => VectorDecode::extremum(decoded),
            0xe0 | 0xe3 if decoded.prefixes.operand_16 => Ok(ScalarInstruction::VectorInteger {
                operation: VectorArithmetic::Average,
                lane: if decoded.opcode == 0xe3 { 2 } else { 1 },
                destination: decoded.register.ok_or(Error::Invalid)?,
                source: Self::vector_source(decoded)?,
            }),
            0xd4 | 0xdc | 0xf6 | 0xf8..=0xfe if decoded.prefixes.operand_16 => {
                let (operation, lane) = match decoded.opcode {
                    0xfc => (VectorArithmetic::Add, 1),
                    0xfd => (VectorArithmetic::Add, 2),
                    0xfe => (VectorArithmetic::Add, 4),
                    0xd4 => (VectorArithmetic::Add, 8),
                    0xf8 => (VectorArithmetic::Subtract, 1),
                    0xf9 => (VectorArithmetic::Subtract, 2),
                    0xfa => (VectorArithmetic::Subtract, 4),
                    0xfb => (VectorArithmetic::Subtract, 8),
                    0xf6 => (VectorArithmetic::SumAbsoluteDifferences, 1),
                    0xdc => (VectorArithmetic::AddUnsignedSaturating, 1),
                    _ => unreachable!(),
                };
                Ok(ScalarInstruction::VectorInteger {
                    operation,
                    lane,
                    destination: decoded.register.ok_or(Error::Invalid)?,
                    source: Self::vector_source(decoded)?,
                })
            }
            0xf4 => VectorDecode::unsigned_multiply(decoded),
            0xd5 | 0xe4 | 0xe5 | 0xf5 => VectorDecode::word_multiply(decoded),
            0x80..=0x8f => Ok(ScalarInstruction::JumpConditional {
                condition: BranchCondition(decoded.opcode & 15),
                target: Self::target(decoded, address)?,
            }),
            0x90..=0x9f => Ok(ScalarInstruction::SetConditional {
                condition: BranchCondition(decoded.opcode & 15),
                destination: Self::rm(decoded, true)?,
            }),
            0x77 => Ok(ScalarInstruction::MmxEmpty),
            0xa2 => Ok(ScalarInstruction::Cpuid),
            0xa3 | 0xab | 0xb3 | 0xbb => crate::x86::bit_decode::BitDecode::register(decoded),
            0xa4 | 0xa5 | 0xac | 0xad => crate::x86::double_shift::DoubleShift::decode(decoded),
            0xaf => Ok(ScalarInstruction::TruncatedMultiply {
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, false)?,
                multiplier: None,
            }),
            0xb0 | 0xb1 => {
                let byte = decoded.opcode == 0xb0;
                let ScalarOperand::Register(source) = Self::reg(decoded, byte)? else {
                    unreachable!()
                };
                Ok(ScalarInstruction::CompareExchange {
                    destination: Self::rm(decoded, byte)?,
                    source,
                    locked: decoded.prefixes.lock,
                })
            }
            0xc7 if decoded.raw_reg == Some(1) => Ok(ScalarInstruction::WideCompareExchange {
                address: decoded.address.ok_or(Error::Invalid)?,
                wide: decoded.rex().is_some_and(|rex| rex.w),
                locked: decoded.prefixes.lock,
            }),
            0xbc | 0xbd => Ok(ScalarInstruction::BitScan {
                operation: match (
                    decoded.opcode,
                    decoded.prefixes.rep && crate::GuestFeaturePolicy::interpreter().bmi1(),
                ) {
                    (0xbc, true) => BitScanOperation::TrailingZeroCount,
                    (0xbc, false) => BitScanOperation::Forward,
                    (0xbd, true) => BitScanOperation::LeadingZeroCount,
                    _ => BitScanOperation::Reverse,
                },
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, false)?,
            }),
            0xb8 if decoded.prefixes.rep && !decoded.prefixes.repne => Ok(ScalarInstruction::PopulationCount {
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, false)?,
            }),
            0xc8..=0xcf => Ok(ScalarInstruction::ByteSwap {
                register: ScalarRegister::General((decoded.opcode & 7) | Self::rex_b(decoded)),
            }),
            0xba => crate::x86::bit_decode::BitDecode::immediate(decoded),
            0xc0 | 0xc1 => Ok(ScalarInstruction::ExchangeAdd {
                destination: Self::rm(decoded, decoded.opcode == 0xc0)?,
                source: match Self::reg(decoded, decoded.opcode == 0xc0)? {
                    ScalarOperand::Register(register) => register,
                    _ => unreachable!(),
                },
                locked: decoded.prefixes.lock,
            }),
            0xc4 if decoded.prefixes.operand_16 && !decoded.prefixes.rep && !decoded.prefixes.repne => {
                Ok(ScalarInstruction::VectorInsertWord {
                    destination: decoded.register.ok_or(Error::Invalid)?,
                    source: Self::rm(decoded, false)?,
                    lane: decoded.immediate.ok_or(Error::Invalid)?.0 as u8 & 7,
                })
            }
            0xc4 if !decoded.prefixes.rep && !decoded.prefixes.repne => Ok(ScalarInstruction::MmxInsertWord {
                destination: decoded.raw_reg.ok_or(Error::Invalid)?,
                source: Self::rm(decoded, false)?,
                lane: decoded.immediate.ok_or(Error::Invalid)?.0 as u8 & 3,
            }),
            0xbe | 0xbf => Ok(ScalarInstruction::MoveSignExtend {
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, decoded.opcode == 0xbe)?,
                source_width: if decoded.opcode == 0xbe {
                    ScalarWidth::Byte
                } else {
                    ScalarWidth::Word
                },
            }),
            0xb6 | 0xb7 => Ok(ScalarInstruction::MoveZeroExtend {
                destination: Self::general_reg(decoded)?,
                source: Self::rm(decoded, decoded.opcode == 0xb6)?,
                source_width: if decoded.opcode == 0xb6 {
                    ScalarWidth::Byte
                } else {
                    ScalarWidth::Word
                },
            }),
            _ => Err(Error::Unsupported),
        }
    }

    pub(crate) fn float_format(decoded: &DecodedInstruction) -> Result<FloatWidth, Error> {
        match (decoded.prefixes.rep, decoded.prefixes.repne) {
            (true, false) => Ok(FloatWidth::Single),
            (false, true) => Ok(FloatWidth::Double),
            _ => Err(Error::Invalid),
        }
    }

    pub(crate) fn float_operation(opcode: u8) -> Result<FloatArithmetic, Error> {
        match opcode {
            0x51 => Ok(FloatArithmetic::SquareRoot),
            0x52 => Ok(FloatArithmetic::ReciprocalSquareRoot),
            0x53 => Ok(FloatArithmetic::Reciprocal),
            0x58 => Ok(FloatArithmetic::Add),
            0x59 => Ok(FloatArithmetic::Multiply),
            0x5c => Ok(FloatArithmetic::Subtract),
            0x5d => Ok(FloatArithmetic::Minimum),
            0x5e => Ok(FloatArithmetic::Divide),
            0x5f => Ok(FloatArithmetic::Maximum),
            _ => Err(Error::Invalid),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Structural(crate::DecodeError),
    Invalid,
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl StdError for Error {}
