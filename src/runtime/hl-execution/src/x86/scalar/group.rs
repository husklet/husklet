use super::{Decoder, Error};
use crate::{
    AluOperation, ByteRegister, DecodedInstruction, RepeatCondition, ScalarInstruction, ScalarOperand, ScalarRegister,
    ShiftCount, ShiftOperation, StringInstruction, StringOperation, UnaryOperation,
};

impl Decoder {
    pub(super) fn alu_encoding(
        decoded: &DecodedInstruction,
        operation: AluOperation,
        form: u8,
    ) -> Result<ScalarInstruction, Error> {
        let byte = matches!(form, 0 | 2 | 4);
        let accumulator = ScalarOperand::Register(if byte {
            ScalarRegister::Byte(ByteRegister::Low(0))
        } else {
            ScalarRegister::General(0)
        });
        let (destination, source) = match form {
            0 | 1 => (Self::rm(decoded, byte)?, Self::reg(decoded, byte)?),
            2 | 3 => (Self::reg(decoded, byte)?, Self::rm(decoded, byte)?),
            4 | 5 => (accumulator, Self::immediate(decoded)?),
            _ => return Err(Error::Unsupported),
        };
        Ok(ScalarInstruction::Alu {
            operation,
            destination,
            source,
            locked: decoded.prefixes.lock,
        })
    }

    pub(super) fn group_one(decoded: &DecodedInstruction) -> Result<ScalarInstruction, Error> {
        let operation = Self::alu(decoded.raw_reg.ok_or(Error::Invalid)?)?;
        Ok(ScalarInstruction::Alu {
            operation,
            destination: Self::rm(decoded, decoded.opcode == 0x80)?,
            source: Self::immediate(decoded)?,
            locked: decoded.prefixes.lock,
        })
    }

    pub(super) fn group_two(decoded: &DecodedInstruction) -> Result<ScalarInstruction, Error> {
        let operation = match decoded.raw_reg.ok_or(Error::Invalid)? {
            0 => ShiftOperation::RotateLeft,
            1 => ShiftOperation::RotateRight,
            2 => ShiftOperation::CarryLeft,
            3 => ShiftOperation::CarryRight,
            4 | 6 => ShiftOperation::Left,
            5 => ShiftOperation::Right,
            7 => ShiftOperation::ArithmeticRight,
            _ => return Err(Error::Invalid),
        };
        let count = match decoded.opcode {
            0xc0 | 0xc1 => ShiftCount::Immediate(decoded.immediate.ok_or(Error::Invalid)?.0 as u8),
            0xd0 | 0xd1 => ShiftCount::One,
            _ => ShiftCount::Cl,
        };
        Ok(ScalarInstruction::Shift {
            operation,
            destination: Self::rm(decoded, decoded.opcode & 1 == 0)?,
            count,
        })
    }

    pub(super) fn group_three(decoded: &DecodedInstruction) -> Result<ScalarInstruction, Error> {
        let byte = decoded.opcode == 0xf6;
        let operand = Self::rm(decoded, byte)?;
        Ok(match decoded.raw_reg.ok_or(Error::Invalid)? {
            0 => ScalarInstruction::Alu {
                operation: AluOperation::Test,
                destination: operand,
                source: Self::immediate(decoded)?,
                locked: decoded.prefixes.lock,
            },
            1 => return Err(Error::Invalid),
            2 => ScalarInstruction::Unary {
                operation: UnaryOperation::Not,
                operand,
            },
            3 => ScalarInstruction::Unary {
                operation: UnaryOperation::Negate,
                operand,
            },
            4 | 5 => ScalarInstruction::Multiply {
                signed: decoded.raw_reg == Some(5),
                source: operand,
            },
            6 | 7 => ScalarInstruction::Divide {
                signed: decoded.raw_reg == Some(7),
                divisor: operand,
            },
            _ => return Err(Error::Invalid),
        })
    }

    pub(super) fn alu(value: u8) -> Result<AluOperation, Error> {
        Ok(match value {
            0 => AluOperation::Add,
            1 => AluOperation::Or,
            2 => AluOperation::Adc,
            3 => AluOperation::Sbb,
            4 => AluOperation::And,
            5 => AluOperation::Sub,
            6 => AluOperation::Xor,
            7 => AluOperation::Compare,
            _ => return Err(Error::Invalid),
        })
    }

    pub(super) fn string(decoded: &DecodedInstruction, opcode: u8) -> ScalarInstruction {
        let operation = match opcode {
            0xa4 | 0xa5 => StringOperation::Move,
            0xa6 | 0xa7 => StringOperation::Compare,
            0xaa | 0xab => StringOperation::Store,
            0xac | 0xad => StringOperation::Load,
            _ => StringOperation::Scan,
        };
        let compare = matches!(operation, StringOperation::Compare | StringOperation::Scan);
        let repeat = if decoded.prefixes.repne {
            if compare {
                RepeatCondition::WhileNotEqual
            } else {
                RepeatCondition::Count
            }
        } else if decoded.prefixes.rep {
            if compare {
                RepeatCondition::WhileEqual
            } else {
                RepeatCondition::Count
            }
        } else {
            RepeatCondition::None
        };
        let source = matches!(
            operation,
            StringOperation::Move | StringOperation::Load | StringOperation::Compare
        );
        ScalarInstruction::String(StringInstruction {
            operation,
            repeat,
            address_32: decoded.prefixes.address_32,
            source_segment: if source { decoded.prefixes.segment } else { None },
        })
    }
}
