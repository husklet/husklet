use crate::{
    DecodedInstruction, ScalarInstruction, ScalarIrError, VectorArithmetic, VectorPackKind, VectorShiftKind,
    VectorShuffleMode, VectorSource,
};

pub struct Decode;

impl Decode {
    pub fn round(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let (format, packed) = match decoded.opcode {
            0x08 => (crate::FloatWidth::Single, true),
            0x09 => (crate::FloatWidth::Double, true),
            0x0a => (crate::FloatWidth::Single, false),
            0x0b => (crate::FloatWidth::Double, false),
            _ => return Err(ScalarIrError::Unsupported),
        };
        Ok(ScalarInstruction::VectorRound {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
            format,
            packed,
            control: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
        })
    }

    pub fn word_multiply(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let operation = match decoded.opcode {
            0xd5 => VectorArithmetic::MultiplyLowWords,
            0xe4 => VectorArithmetic::MultiplyHighWords { signed: false },
            0xe5 => VectorArithmetic::MultiplyHighWords { signed: true },
            0xf5 => VectorArithmetic::MultiplyAddWords,
            _ => return Err(ScalarIrError::Unsupported),
        };
        Ok(ScalarInstruction::VectorInteger {
            operation,
            lane: 2,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
        })
    }

    pub fn horizontal_minimum(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::VectorHorizontalMinimum {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
        })
    }

    pub fn immediate_sse41(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let destination = decoded.register.ok_or(ScalarIrError::Invalid)?;
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        let control = decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8;
        match decoded.opcode {
            0x40 | 0x41 => Ok(ScalarInstruction::VectorDot {
                destination,
                source,
                control,
                format: if decoded.opcode == 0x40 {
                    crate::FloatWidth::Single
                } else {
                    crate::FloatWidth::Double
                },
            }),
            0x42 => Ok(ScalarInstruction::VectorSad {
                destination,
                source,
                control,
            }),
            _ => Err(ScalarIrError::Unsupported),
        }
    }

    pub fn carryless_multiply(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::CarrylessMultiply {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
            control: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
        })
    }

    pub fn blend(decoded: &DecodedInstruction, implicit: bool) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let lane = match decoded.opcode {
            0x10 => 1,
            0x14 => 4,
            0x15 => 8,
            0x0c => 4,
            0x0d => 8,
            0x0e => 2,
            _ => return Err(ScalarIrError::Unsupported),
        };
        Ok(ScalarInstruction::VectorBlend {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
            lane,
            selectors: if implicit {
                0
            } else {
                decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u16
            },
            implicit,
        })
    }

    pub fn sse41(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let destination = decoded.register.ok_or(ScalarIrError::Invalid)?;
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        if matches!(decoded.opcode, 0x20..=0x25 | 0x30..=0x35) {
            let shape = decoded.opcode & 0x0f;
            let (source_lane, destination_lane) = match shape {
                0 => (1, 2),
                1 => (1, 4),
                2 => (1, 8),
                3 => (2, 4),
                4 => (2, 8),
                5 => (4, 8),
                _ => unreachable!(),
            };
            return Ok(ScalarInstruction::VectorExtend {
                destination,
                source,
                source_lane,
                destination_lane,
                signed: decoded.opcode < 0x30,
            });
        }
        let (operation, lane) = match decoded.opcode {
            0x28 => (VectorArithmetic::SignedMultiplyEvenDwords, 4),
            0x38 => (VectorArithmetic::SignedMinimum, 1),
            0x39 => (VectorArithmetic::SignedMinimum, 4),
            0x3a => (VectorArithmetic::UnsignedMinimum, 2),
            0x3b => (VectorArithmetic::UnsignedMinimum, 4),
            0x3c => (VectorArithmetic::SignedMaximum, 1),
            0x3d => (VectorArithmetic::SignedMaximum, 4),
            0x3e => (VectorArithmetic::UnsignedMaximum, 2),
            0x3f => (VectorArithmetic::UnsignedMaximum, 4),
            0x40 => (VectorArithmetic::MultiplyLowDwords, 4),
            _ => return Err(ScalarIrError::Unsupported),
        };
        Ok(ScalarInstruction::VectorInteger {
            operation,
            destination,
            source,
            lane,
        })
    }

    pub fn test(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::VectorTest {
            left: decoded.register.ok_or(ScalarIrError::Invalid)?,
            right: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
        })
    }

    pub fn align(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::VectorAlign {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
            count: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
        })
    }

    pub fn ssse3(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let (operation, lane) = match decoded.opcode {
            0x01 => (
                crate::x86::Ssse3Operation::Horizontal {
                    subtract: false,
                    saturating: false,
                },
                2,
            ),
            0x02 => (
                crate::x86::Ssse3Operation::Horizontal {
                    subtract: false,
                    saturating: false,
                },
                4,
            ),
            0x03 => (
                crate::x86::Ssse3Operation::Horizontal {
                    subtract: false,
                    saturating: true,
                },
                2,
            ),
            0x04 => (crate::x86::Ssse3Operation::MultiplyAdd, 2),
            0x05 => (
                crate::x86::Ssse3Operation::Horizontal {
                    subtract: true,
                    saturating: false,
                },
                2,
            ),
            0x06 => (
                crate::x86::Ssse3Operation::Horizontal {
                    subtract: true,
                    saturating: false,
                },
                4,
            ),
            0x07 => (
                crate::x86::Ssse3Operation::Horizontal {
                    subtract: true,
                    saturating: true,
                },
                2,
            ),
            0x08..=0x0a => (crate::x86::Ssse3Operation::Sign, 1 << (decoded.opcode - 0x08)),
            0x0b => (crate::x86::Ssse3Operation::RoundedMultiply, 2),
            0x1c..=0x1e => (crate::x86::Ssse3Operation::Absolute, 1 << (decoded.opcode - 0x1c)),
            _ => return Err(ScalarIrError::Unsupported),
        };
        Ok(ScalarInstruction::Ssse3 {
            operation,
            lane,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
        })
    }
    pub fn byte_shuffle(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::VectorByteShuffle {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            control: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
        })
    }

    pub fn unsigned_multiply(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::VectorInteger {
            operation: VectorArithmetic::UnsignedMultiplyEvenDwords,
            lane: 4,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source: if decoded.raw_mod == Some(3) {
                VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
            } else {
                VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
            },
        })
    }

    pub fn shuffle(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let prefix_count =
            u8::from(decoded.prefixes.operand_16) + u8::from(decoded.prefixes.rep) + u8::from(decoded.prefixes.repne);
        let mode = match decoded.opcode {
            0x70 if prefix_count == 1 => {
                if decoded.prefixes.operand_16 {
                    VectorShuffleMode::Dwords
                } else if decoded.prefixes.repne {
                    VectorShuffleMode::LowWords
                } else {
                    VectorShuffleMode::HighWords
                }
            }
            0xc6 if !decoded.prefixes.rep && !decoded.prefixes.repne => {
                if decoded.prefixes.operand_16 {
                    VectorShuffleMode::PackedDouble
                } else {
                    VectorShuffleMode::PackedSingle
                }
            }
            _ => return Err(ScalarIrError::Invalid),
        };
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorShuffle {
            mode,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source,
            selectors: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
        })
    }

    pub fn pack(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let kind = match decoded.opcode {
            0x63 => VectorPackKind::SignedBytes,
            0x67 => VectorPackKind::UnsignedBytes,
            0x6b => VectorPackKind::SignedWords,
            _ => return Err(ScalarIrError::Unsupported),
        };
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorPack {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source,
            kind,
        })
    }

    pub fn immediate_shift(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne || decoded.raw_mod != Some(3)
        {
            return Err(ScalarIrError::Invalid);
        }
        let extension = decoded.raw_reg.ok_or(ScalarIrError::Invalid)?;
        let vector = decoded.register_operand.ok_or(ScalarIrError::Invalid)?;
        let count = decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8;
        if decoded.opcode == 0x73 && matches!(extension, 3 | 7) {
            return Ok(ScalarInstruction::VectorByteShift {
                vector,
                left: extension == 7,
                count,
            });
        }
        let kind = match extension {
            2 => VectorShiftKind::LogicalRight,
            4 if decoded.opcode != 0x73 => VectorShiftKind::ArithmeticRight,
            6 => VectorShiftKind::Left,
            _ => return Err(ScalarIrError::Invalid),
        };
        Ok(ScalarInstruction::VectorLaneShift {
            vector,
            kind,
            count,
            lane: 1 << (decoded.opcode - 0x70),
        })
    }

    pub fn extremum(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let (operation, lane) = match decoded.opcode {
            0xda => (VectorArithmetic::UnsignedMinimum, 1),
            0xde => (VectorArithmetic::UnsignedMaximum, 1),
            0xea => (VectorArithmetic::SignedMinimum, 2),
            0xee => (VectorArithmetic::SignedMaximum, 2),
            _ => return Err(ScalarIrError::Unsupported),
        };
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorInteger {
            operation,
            lane,
            source,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
        })
    }
}
