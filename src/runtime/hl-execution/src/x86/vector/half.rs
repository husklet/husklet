use crate::{DecodedInstruction, ScalarInstruction, ScalarIrError, VectorSource};

pub(crate) struct HalfDecode;

impl HalfDecode {
    pub(crate) fn duplicate(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.prefixes.operand_16 || decoded.prefixes.rep == decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        if decoded.prefixes.repne && decoded.opcode != 0x12 {
            return Err(ScalarIrError::Invalid);
        }
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorDuplicate {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source,
            lane: if decoded.prefixes.repne { 8 } else { 4 },
            high: decoded.opcode == 0x16,
        })
    }

    pub(crate) fn unpack(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorUnpack {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source,
            lane: if decoded.prefixes.operand_16 { 8 } else { 4 },
            high: decoded.opcode == 0x15,
        })
    }

    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        let low = matches!(decoded.opcode, 0x12 | 0x13);
        let register = decoded.raw_mod == Some(3);
        if decoded.prefixes.rep
            || decoded.prefixes.repne
            || register && (matches!(decoded.opcode, 0x13 | 0x17) || decoded.prefixes.operand_16)
        {
            return Err(ScalarIrError::Invalid);
        }
        let source = if register {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorHalf {
            vector: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source,
            store: matches!(decoded.opcode, 0x13 | 0x17),
            high: !low,
        })
    }
}
