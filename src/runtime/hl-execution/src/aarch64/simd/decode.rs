use crate::{
    Aarch64DecodeError, Aarch64Instruction, NarrowMode, SimdCopy, SimdLaneOperation, SimdLogic, SimdPermute,
    SimdReduceOperation, SimdShift, SimdUnary, SimdWideOperation,
};
pub(crate) struct Aarch64SimdDecoder;
impl Aarch64SimdDecoder {
    fn register(word: u32, shift: u32) -> u8 {
        u8::try_from(word >> shift & 31).expect("masked register field fits in u8")
    }

    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        if let Some(decoded) = crate::aarch64_simd_crypto::Crypto::decode(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_bf16::Bf16::decode_vector(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_reciprocal::Reciprocal::decode(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_narrow::NarrowOdd::decode(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_convert::Convert::decode(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_fcvtzs::Fcvtzs::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_scvtf::Scvtf::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_compare::Comparison::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = Self::fp_unary(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_arithmetic::Binary::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_fused::FusedAccumulator::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_fp_product::FpProduct::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_fp_reduce::FpReduce::decode(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_dot::DotProduct::decode(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_matrix::MatrixProduct::decode(word) {
            return Some(Ok(decoded));
        }
        if let Some(decoded) = crate::aarch64_simd_product::IntegerProduct::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_high_product::HighProduct::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_long_product::LongProduct::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_saturating_product::SaturatingProduct::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_saturating_unary::Saturation::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_scalar::Scalar::decode(word) {
            return Some(decoded);
        }
        if let Some(decoded) = crate::aarch64_simd_variable::VariableShift::decode(word) {
            return Some(decoded);
        }
        if word & 0x9fe0_8400 == 0x0e00_0400 {
            return Some(Self::copy(word));
        }
        if word & 0xbfe0_8400 == 0x2e00_0000 {
            return Some(Self::extract(word));
        }
        if word & 0xbf20_8c00 == 0x0e00_0000 {
            return Some(Ok(Self::table(word)));
        }
        if word & 0xbf20_8c00 == 0x0e00_0800 {
            return Some(Self::permute(word));
        }
        if word & 0x9ff8_0400 == 0x0f00_0400 {
            return Some(Self::immediate(word));
        }
        if word & 0x9f80_0400 == 0x0f00_0400 {
            return Some(Self::shift(word));
        }
        if let Some(decoded) = crate::aarch64_simd_pair::Pair::decode(word) {
            return Some(decoded);
        }
        if word & 0x9f3e_0c00 == 0x0e20_0800 {
            return Some(Self::unary(word));
        }
        if word & 0x9f3e_0c00 == 0x0e30_0800 {
            return Some(Self::reduce(word));
        }
        if word & 0x9f20_0c00 == 0x0e20_0000 {
            return Some(Self::wide(word));
        }
        if word & 0x9f20_0400 == 0x0e20_0400 {
            return Some(Self::three_same(word));
        }
        None
    }
    fn fp_unary(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        if word & 0x9f3e_0c00 != 0x0e20_0800 {
            return None;
        }
        let opcode = word >> 12 & 0x1f;
        if !matches!(opcode, 0x0f | 0x18 | 0x19) {
            return None;
        }
        let size = (word >> 22 & 3) as u8;
        let format = if size & 1 == 0 {
            crate::FpFormat::Single
        } else {
            crate::FpFormat::Double
        };
        let wide = word >> 30 & 1 != 0;
        if format == crate::FpFormat::Double && !wide {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let source = Self::register(word, 5);
        let destination = Self::register(word, 0);
        let lanes = if format == crate::FpFormat::Double {
            2
        } else if wide {
            4
        } else {
            2
        };
        let unsigned = word >> 29 & 1 != 0;
        let high = size >> 1 != 0;
        if opcode == 0x0f {
            return Some(Ok(Aarch64Instruction::SimdFpUnary {
                operation: if unsigned {
                    crate::FpUnaryOperation::Negate
                } else {
                    crate::FpUnaryOperation::Absolute
                },
                format,
                source,
                destination,
                lanes,
            }));
        }
        let (rounding, exact) = match (opcode, unsigned, high) {
            (0x18, false, false) => (crate::FpRoundingMode::NearestEven, false),
            (0x18, false, true) => (crate::FpRoundingMode::PositiveInfinity, false),
            (0x18, true, false) => (crate::FpRoundingMode::NearestAway, false),
            (0x18, true, true) => return Some(Err(Aarch64DecodeError::Reserved)),
            (0x19, false, false) => (crate::FpRoundingMode::NegativeInfinity, false),
            (0x19, false, true) => (crate::FpRoundingMode::Zero, false),
            (0x19, true, false) => (crate::FpRoundingMode::Current, true),
            (0x19, true, true) => (crate::FpRoundingMode::Current, false),
            _ => unreachable!(),
        };
        Some(Ok(Aarch64Instruction::SimdFpRoundIntegral {
            format,
            source,
            destination,
            lanes,
            rounding,
            exact,
        }))
    }
    fn copy(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let imm4 = (word >> 11 & 15) as u8;
        let imm5 = Self::register(word, 16);
        let (lane_bits, lane) = Self::element(imm5)?;
        let size = lane_bits.trailing_zeros() - 3;
        let operation = if word >> 29 & 1 != 0 {
            SimdCopy::InsertElement {
                source_lane: imm4 >> size,
            }
        } else {
            match imm4 {
                0 => SimdCopy::DuplicateElement { source_lane: lane },
                1 => SimdCopy::DuplicateGeneral,
                3 => SimdCopy::InsertGeneral,
                5 => SimdCopy::MoveSigned,
                7 => SimdCopy::MoveUnsigned,
                _ => return Err(Aarch64DecodeError::Reserved),
            }
        };
        let destination = Self::register(word, 0);
        let source = Self::register(word, 5);
        Ok(Aarch64Instruction::SimdCopy {
            operation,
            lane_bits,
            lane,
            source,
            destination,
            wide: word >> 30 & 1 != 0,
        })
    }

    fn extract(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let position = (word >> 11 & 15) as u8;
        let wide = word >> 30 & 1 != 0;
        if !wide && position >= 8 {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::SimdExtract {
            first: Self::register(word, 5),
            second: Self::register(word, 16),
            destination: Self::register(word, 0),
            position,
            wide,
        })
    }

    fn table(word: u32) -> Aarch64Instruction {
        Aarch64Instruction::SimdTable {
            first_table: Self::register(word, 5),
            table_count: ((word >> 13 & 3) + 1) as u8,
            indexes: Self::register(word, 16),
            destination: Self::register(word, 0),
            extend: word >> 12 & 1 != 0,
            wide: word >> 30 & 1 != 0,
        }
    }

    fn permute(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let size = (word >> 22 & 3) as u8;
        let wide = word >> 30 & 1 != 0;
        if size == 3 && !wide {
            return Err(Aarch64DecodeError::Reserved);
        }
        let operation = match word >> 12 & 7 {
            1 => SimdPermute::UnzipLow,
            5 => SimdPermute::UnzipHigh,
            2 => SimdPermute::TransposeLow,
            6 => SimdPermute::TransposeHigh,
            3 => SimdPermute::ZipLow,
            7 => SimdPermute::ZipHigh,
            _ => return Err(Aarch64DecodeError::Reserved),
        };
        Ok(Aarch64Instruction::SimdPermute {
            operation,
            lane_bits: 8 << size,
            left: Self::register(word, 5),
            right: Self::register(word, 16),
            destination: Self::register(word, 0),
            wide,
        })
    }

    fn immediate(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let operation = word >> 29 & 1;
        let cmode = word >> 12 & 15;
        let o2 = word >> 11 & 1;
        let wide = word >> 30 & 1 != 0;
        let immediate = u64::from((word >> 16 & 7) << 5 | (word >> 5 & 31));
        let pattern =
            Self::expand_immediate(operation, cmode, o2, wide, immediate).ok_or(Aarch64DecodeError::Reserved)?;
        let read_modify = cmode & 1 != 0 && (cmode >> 2 & 3) != 3;
        Ok(Aarch64Instruction::SimdImmediate {
            destination: Self::register(word, 0),
            pattern,
            invert: !read_modify && operation != 0 && (cmode >> 1 & 7) != 7,
            modify: read_modify.then_some(if operation == 0 {
                SimdLogic::Orr
            } else {
                SimdLogic::BitClear
            }),
            wide,
        })
    }

    fn three_same(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let opcode = word >> 11 & 31;
        let size = (word >> 22 & 3) as u8;
        let wide = word >> 30 & 1 != 0;
        if opcode == 3 {
            let operation = match (word >> 29 & 1, size) {
                (0, 0) => SimdLogic::And,
                (0, 1) => SimdLogic::BitClear,
                (0, 2) => SimdLogic::Orr,
                (1, 0) => SimdLogic::ExclusiveOr,
                (1, 1) => SimdLogic::BitSelect,
                (1, 2) => SimdLogic::BitInsertTrue,
                (1, 3) => SimdLogic::BitInsertFalse,
                _ => return Err(Aarch64DecodeError::Unsupported),
            };
            return Ok(Aarch64Instruction::SimdLogic {
                operation,
                left: Self::register(word, 5),
                right: Self::register(word, 16),
                destination: Self::register(word, 0),
                wide,
            });
        }
        let lane_operation = match (opcode, word >> 29 & 1 != 0) {
            (0x00, unsigned) => Some(SimdLaneOperation::HalvingAdd {
                unsigned,
                rounding: false,
            }),
            (0x02, unsigned) => Some(SimdLaneOperation::HalvingAdd {
                unsigned,
                rounding: true,
            }),
            (0x04, unsigned) => Some(SimdLaneOperation::HalvingSubtract { unsigned }),
            (0x06, unsigned) => Some(SimdLaneOperation::CompareGreater { unsigned }),
            (0x07, unsigned) => Some(SimdLaneOperation::CompareGreaterEqual { unsigned }),
            (0x0c, unsigned) => Some(SimdLaneOperation::Maximum { unsigned }),
            (0x0d, unsigned) => Some(SimdLaneOperation::Minimum { unsigned }),
            (0x11, true) => Some(SimdLaneOperation::CompareEqual),
            (0x11, false) => Some(SimdLaneOperation::TestBits),
            (0x12, subtract) => Some(SimdLaneOperation::MultiplyAccumulate { subtract }),
            (0x13, false) => Some(SimdLaneOperation::Multiply),
            (0x17, false) => Some(SimdLaneOperation::PairAdd),
            (0x14, unsigned) => Some(SimdLaneOperation::PairMaximum { unsigned }),
            (0x15, unsigned) => Some(SimdLaneOperation::PairMinimum { unsigned }),
            _ => None,
        };
        if let Some(operation) = lane_operation {
            if size == 3
                && (!wide
                    || matches!(
                        operation,
                        SimdLaneOperation::Multiply | SimdLaneOperation::MultiplyAccumulate { .. }
                    ))
            {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::SimdLane {
                operation,
                lane_bits: 8 << size,
                left: Self::register(word, 5),
                right: Self::register(word, 16),
                destination: Self::register(word, 0),
                wide,
            });
        }
        if opcode != 0x10 && opcode != 0x01 && opcode != 0x05 {
            return Err(Aarch64DecodeError::Unsupported);
        }
        if size == 3 && !wide {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::SimdAddSubtract {
            subtract: if opcode == 0x10 {
                word >> 29 & 1 != 0
            } else {
                opcode == 0x05
            },
            saturating: opcode != 0x10,
            unsigned: opcode != 0x10 && word >> 29 & 1 != 0,
            lane_bits: 8 << size,
            left: Self::register(word, 5),
            right: Self::register(word, 16),
            destination: Self::register(word, 0),
            wide,
        })
    }

    fn unary(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let size = (word >> 22 & 3) as u8;
        let opcode = word >> 12 & 31;
        let unsigned = word >> 29 & 1 != 0;
        let wide = word >> 30 & 1 != 0;
        if opcode == 0x02 {
            if size == 3 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::SimdWide {
                operation: SimdWideOperation::PairAddLong,
                signed: !unsigned,
                lane_bits: 8 << size,
                left: Self::register(word, 5),
                right: 0,
                destination: Self::register(word, 0),
                high: wide,
            });
        }
        if opcode == 0x13 {
            if !unsigned || size == 3 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::SimdWide {
                operation: SimdWideOperation::ShiftLong { amount: 8 << size },
                signed: false,
                lane_bits: 8 << size,
                left: Self::register(word, 5),
                right: 0,
                destination: Self::register(word, 0),
                high: wide,
            });
        }
        if matches!(opcode, 0x12 | 0x14) {
            if size == 3 {
                return Err(Aarch64DecodeError::Reserved);
            }
            let operation = match (opcode, unsigned) {
                (0x12, false) => SimdWideOperation::ShiftNarrow {
                    amount: 0,
                    rounding: false,
                    mode: NarrowMode::Truncate,
                },
                (0x12, true) => SimdWideOperation::SaturatingNarrow {
                    source_signed: true,
                    destination_signed: false,
                },
                (0x14, signedness) => SimdWideOperation::SaturatingNarrow {
                    source_signed: !signedness,
                    destination_signed: !signedness,
                },
                _ => unreachable!("narrowing opcodes were exhaustively matched"),
            };
            return Ok(Aarch64Instruction::SimdWide {
                operation,
                signed: !unsigned,
                lane_bits: 8 << size,
                left: Self::register(word, 5),
                right: 0,
                destination: Self::register(word, 0),
                high: wide,
            });
        }
        let operation = match (opcode, unsigned, size) {
            (0, false, _) => SimdUnary::Reverse { container_bytes: 8 },
            (0, true, _) => SimdUnary::Reverse { container_bytes: 4 },
            (1, false, _) => SimdUnary::Reverse { container_bytes: 2 },
            (4, false, 0..=2) => SimdUnary::CountLeadingSign,
            (4, true, 0..=2) => SimdUnary::CountLeadingZero,
            (5, false, 0) => SimdUnary::PopulationCount,
            (5, true, 0) => SimdUnary::Not,
            (5, true, 1) => SimdUnary::ReverseBits,
            (8, false, _) => SimdUnary::CompareGreaterZero,
            (8, true, _) => SimdUnary::CompareGreaterEqualZero,
            (9, false, _) => SimdUnary::CompareEqualZero,
            (9, true, _) => SimdUnary::CompareLessEqualZero,
            (10, false, _) => SimdUnary::CompareLessZero,
            (11, false, _) => SimdUnary::Absolute,
            (11, true, _) => SimdUnary::Negate,
            _ => return Err(Aarch64DecodeError::Reserved),
        };
        if matches!(operation, SimdUnary::Reverse { container_bytes } if (1_u8 << size) >= container_bytes)
            || size == 3 && !wide
        {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::SimdUnary {
            operation,
            lane_bits: if operation == SimdUnary::ReverseBits {
                8
            } else {
                8 << size
            },
            source: Self::register(word, 5),
            destination: Self::register(word, 0),
            wide,
        })
    }

    fn shift(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let immh = (word >> 19 & 15) as u8;
        if immh == 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let size = 7 - immh.leading_zeros() as u8;
        let lane_bits = 8 << size;
        let combined = immh << 3 | (word >> 16 & 7) as u8;
        let opcode = word >> 11 & 31;
        let unsigned = word >> 29 & 1 != 0;
        if opcode == 0x14 {
            if lane_bits == 64 {
                return Err(Aarch64DecodeError::Reserved);
            }
            return Ok(Aarch64Instruction::SimdWide {
                operation: SimdWideOperation::ShiftLong {
                    amount: combined - lane_bits,
                },
                signed: !unsigned,
                lane_bits,
                left: Self::register(word, 5),
                right: 0,
                destination: Self::register(word, 0),
                high: word >> 30 & 1 != 0,
            });
        }
        if matches!(opcode, 0x10..=0x13) {
            if lane_bits == 64 {
                return Err(Aarch64DecodeError::Reserved);
            }
            let mode = match (opcode, unsigned) {
                (0x10 | 0x11, false) => NarrowMode::Truncate,
                (0x10 | 0x11, true) => NarrowMode::Saturate {
                    source_signed: true,
                    destination_signed: false,
                },
                (0x12 | 0x13, signedness) => NarrowMode::Saturate {
                    source_signed: !signedness,
                    destination_signed: !signedness,
                },
                _ => unreachable!("narrowing opcodes were exhaustively matched"),
            };
            return Ok(Aarch64Instruction::SimdWide {
                operation: SimdWideOperation::ShiftNarrow {
                    amount: 2 * lane_bits - combined,
                    rounding: opcode & 1 != 0,
                    mode,
                },
                signed: false,
                lane_bits,
                left: Self::register(word, 5),
                right: 0,
                destination: Self::register(word, 0),
                high: word >> 30 & 1 != 0,
            });
        }
        let operation = match opcode {
            0x0a if !unsigned => SimdShift::Left,
            0x08 => SimdShift::Insert { left: false },
            0x0a if unsigned => SimdShift::Insert { left: true },
            0x00 | 0x02 | 0x04 | 0x06 => SimdShift::Right {
                signed: !unsigned,
                rounding: opcode == 0x04 || opcode == 0x06,
                accumulating: opcode == 0x02 || opcode == 0x06,
            },
            _ => return Err(Aarch64DecodeError::Unsupported),
        };
        let amount = match operation {
            SimdShift::Left => combined - lane_bits,
            SimdShift::Insert { left } => {
                if left {
                    combined - lane_bits
                } else {
                    2 * lane_bits - combined
                }
            }
            SimdShift::Right { .. } => 2 * lane_bits - combined,
        };
        let wide = word >> 30 & 1 != 0;
        if lane_bits == 64 && !wide {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::SimdShift {
            operation,
            amount,
            lane_bits,
            source: Self::register(word, 5),
            destination: Self::register(word, 0),
            wide,
        })
    }

    fn wide(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let size = (word >> 22 & 3) as u8;
        if size == 3 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let opcode = word >> 12 & 15;
        let rounding = word >> 29 & 1 != 0;
        let operation = match opcode {
            0 => SimdWideOperation::AddLong,
            1 => SimdWideOperation::AddWide,
            2 => SimdWideOperation::SubtractLong,
            3 => SimdWideOperation::SubtractWide,
            4 => SimdWideOperation::AddHighNarrow { rounding },
            6 => SimdWideOperation::SubtractHighNarrow { rounding },
            8 => SimdWideOperation::MultiplyAccumulateLong { subtract: false },
            10 => SimdWideOperation::MultiplyAccumulateLong { subtract: true },
            12 => SimdWideOperation::MultiplyLong,
            _ => return Err(Aarch64DecodeError::Unsupported),
        };
        Ok(Aarch64Instruction::SimdWide {
            operation,
            signed: !rounding,
            lane_bits: 8 << size,
            left: Self::register(word, 5),
            right: Self::register(word, 16),
            destination: Self::register(word, 0),
            high: word >> 30 & 1 != 0,
        })
    }

    fn reduce(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let size = (word >> 22 & 3) as u8;
        let wide = word >> 30 & 1 != 0;
        if size == 3 || size == 2 && !wide {
            return Err(Aarch64DecodeError::Reserved);
        }
        let unsigned = word >> 29 & 1 != 0;
        let operation = match word >> 12 & 31 {
            3 => SimdReduceOperation::AddLong { signed: !unsigned },
            0x0a => SimdReduceOperation::Maximum { unsigned },
            0x1a => SimdReduceOperation::Minimum { unsigned },
            0x1b if !unsigned => SimdReduceOperation::Add,
            _ => return Err(Aarch64DecodeError::Unsupported),
        };
        Ok(Aarch64Instruction::SimdReduce {
            operation,
            lane_bits: 8 << size,
            source: Self::register(word, 5),
            destination: Self::register(word, 0),
            wide,
        })
    }
}
