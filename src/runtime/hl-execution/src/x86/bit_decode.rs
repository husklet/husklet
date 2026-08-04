use crate::{BitAction, DecodedInstruction, ScalarInstruction, ScalarIrError, ScalarOperand};

pub(crate) struct BitDecode;

impl BitDecode {
    pub(crate) fn register(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        Ok(ScalarInstruction::BitOperation {
            action: Self::action(decoded.opcode)?,
            destination: crate::x86::scalar::Decoder::rm(decoded, false)?,
            index: crate::x86::scalar::Decoder::reg(decoded, false)?,
            locked: decoded.prefixes.lock,
        })
    }

    pub(crate) fn immediate(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let action = match decoded.raw_reg.ok_or(ScalarIrError::Invalid)? {
            4 => BitAction::Test,
            5 => BitAction::Set,
            6 => BitAction::Reset,
            7 => BitAction::Complement,
            _ => return Err(ScalarIrError::Invalid),
        };
        Ok(ScalarInstruction::BitOperation {
            action,
            destination: crate::x86::scalar::Decoder::rm(decoded, false)?,
            index: ScalarOperand::Immediate(
                decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8 as i64
                    & if decoded.rex().is_some_and(|rex| rex.w) { 63 } else { 31 },
            ),
            locked: decoded.prefixes.lock,
        })
    }

    fn action(opcode: u8) -> Result<BitAction, ScalarIrError> {
        match opcode {
            0xa3 => Ok(BitAction::Test),
            0xab => Ok(BitAction::Set),
            0xb3 => Ok(BitAction::Reset),
            0xbb => Ok(BitAction::Complement),
            _ => Err(ScalarIrError::Invalid),
        }
    }
}
