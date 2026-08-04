use crate::{DecodedInstruction, ScalarInstruction, ScalarIrError, ScalarRegister};

pub struct Mask;

impl Mask {
    pub fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.raw_mod != Some(3) || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Unsupported);
        }
        let lane = match (decoded.opcode, decoded.prefixes.operand_16) {
            (0x50, false) => 4,
            (0x50, true) => 8,
            (0xd7, true) => 1,
            _ => return Err(ScalarIrError::Unsupported),
        };
        Ok(ScalarInstruction::VectorMask {
            destination: ScalarRegister::General(decoded.register.ok_or(ScalarIrError::Invalid)?),
            source: decoded.register_operand.ok_or(ScalarIrError::Invalid)?,
            lane,
        })
    }
}
