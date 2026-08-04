use crate::{AluOperation, DecodedInstruction, ScalarInstruction, ScalarIrError, ScalarOperand};

pub(crate) struct PrefixValidator;

impl PrefixValidator {
    pub(crate) fn validate(decoded: &DecodedInstruction, instruction: ScalarInstruction) -> Result<(), ScalarIrError> {
        if (decoded.prefixes.repne || decoded.prefixes.rep)
            && !matches!(
                instruction,
                ScalarInstruction::Nop
                    | ScalarInstruction::Undefined
                    | ScalarInstruction::AccumulatorExchange { .. }
                    | ScalarInstruction::String(_)
                    | ScalarInstruction::AccumulatorSignExtend { .. }
                    | ScalarInstruction::VectorLoad { .. }
                    | ScalarInstruction::AccumulatorHighExtend
                    | ScalarInstruction::PushFlags
                    | ScalarInstruction::PopFlags
                    | ScalarInstruction::ReadSelector { .. }
                    | ScalarInstruction::WriteSelector
                    | ScalarInstruction::WideCompareExchange { .. }
                    | ScalarInstruction::EndianMove { .. }
                    | ScalarInstruction::Crc32c { .. }
                    | ScalarInstruction::FlagControl(_)
                    | ScalarInstruction::VectorScalarMove { .. }
                    | ScalarInstruction::ConvertFloatInteger { .. }
                    | ScalarInstruction::ConvertIntegerFloat { .. }
                    | ScalarInstruction::ConvertFloatWidth { .. }
                    | ScalarInstruction::ConvertPackedSingle { .. }
                    | ScalarInstruction::ConvertPackedDouble { .. }
                    | ScalarInstruction::VectorFloatArithmetic { .. }
                    | ScalarInstruction::VectorPairArithmetic { .. }
                    | ScalarInstruction::VectorVariableShift { .. }
                    | ScalarInstruction::MmxVector { .. }
                    | ScalarInstruction::VectorDuplicate { .. }
                    | ScalarInstruction::PackedString { .. }
                    | ScalarInstruction::Ssse3 { .. }
                    | ScalarInstruction::VectorAlign { .. }
                    | ScalarInstruction::VectorFloatCompare { .. }
                    | ScalarInstruction::BitScan {
                        operation: crate::BitScanOperation::TrailingZeroCount
                            | crate::BitScanOperation::LeadingZeroCount,
                        ..
                    }
                    | ScalarInstruction::PopulationCount { .. }
                    | ScalarInstruction::VectorShuffle { .. }
                    | ScalarInstruction::VectorTransport { aligned: false, .. }
            )
        {
            return Err(ScalarIrError::Invalid);
        }
        if decoded.prefixes.lock {
            match instruction {
                ScalarInstruction::PushFlags
                | ScalarInstruction::PopFlags
                | ScalarInstruction::ReadSelector { .. }
                | ScalarInstruction::WriteSelector
                | ScalarInstruction::EndianMove { .. }
                | ScalarInstruction::FlagControl(_) => return Err(ScalarIrError::Invalid),
                ScalarInstruction::WideCompareExchange { .. } => {}
                ScalarInstruction::Alu {
                    operation: AluOperation::Test | AluOperation::Compare,
                    ..
                }
                | ScalarInstruction::CompareExchange {
                    destination: ScalarOperand::Register(_),
                    ..
                }
                | ScalarInstruction::ExchangeAdd {
                    destination: ScalarOperand::Register(_),
                    ..
                } => return Err(ScalarIrError::Invalid),
                ScalarInstruction::BitOperation {
                    action: crate::BitAction::Test,
                    ..
                }
                | ScalarInstruction::BitOperation {
                    destination: ScalarOperand::Register(_),
                    ..
                } => return Err(ScalarIrError::Invalid),
                ScalarInstruction::Increment {
                    operand: ScalarOperand::Register(_),
                    ..
                } => return Err(ScalarIrError::Invalid),
                ScalarInstruction::Alu {
                    destination: ScalarOperand::Memory(_),
                    ..
                }
                | ScalarInstruction::Exchange {
                    destination: ScalarOperand::Memory(_),
                    ..
                }
                | ScalarInstruction::ExchangeAdd {
                    destination: ScalarOperand::Memory(_),
                    ..
                }
                | ScalarInstruction::BitOperation {
                    destination: ScalarOperand::Memory(_),
                    ..
                }
                | ScalarInstruction::Increment {
                    operand: ScalarOperand::Memory(_),
                    ..
                }
                | ScalarInstruction::CompareExchange {
                    destination: ScalarOperand::Memory(_),
                    ..
                } => {}
                _ => return Err(ScalarIrError::Invalid),
            }
        }
        if decoded.prefixes.segment.is_some()
            && decoded.address.is_none()
            && !matches!(
                instruction,
                ScalarInstruction::Nop
                    | ScalarInstruction::AccumulatorExchange { .. }
                    | ScalarInstruction::String(_)
                    | ScalarInstruction::AccumulatorSignExtend { .. }
                    | ScalarInstruction::AccumulatorHighExtend
                    | ScalarInstruction::PushFlags
                    | ScalarInstruction::PopFlags
                    | ScalarInstruction::ReadSelector { .. }
                    | ScalarInstruction::WriteSelector
                    | ScalarInstruction::Iret
                    | ScalarInstruction::FlagControl(_)
                    | ScalarInstruction::Xlat { .. }
                    | ScalarInstruction::CountBranch { .. }
                    | ScalarInstruction::VectorBitwise { .. }
                    | ScalarInstruction::ConvertPackedSingle { .. }
                    | ScalarInstruction::ConvertPackedDouble { .. }
                    | ScalarInstruction::VectorFloatArithmetic { .. }
                    | ScalarInstruction::VectorPairArithmetic { .. }
                    | ScalarInstruction::VectorVariableShift { .. }
                    | ScalarInstruction::PackedString { .. }
                    | ScalarInstruction::Ssse3 { .. }
                    | ScalarInstruction::VectorAlign { .. }
            )
        {
            return Err(ScalarIrError::Invalid);
        }
        Ok(())
    }
}
