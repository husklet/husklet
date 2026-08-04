use crate::{DecodedInstruction, ScalarInstruction, ScalarIrError, VectorComparison, VectorSource};

pub struct Compare;

impl Compare {
    pub fn decode(decoded: &DecodedInstruction, map: u8) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Unsupported);
        }
        let (comparison, lane) = match (map, decoded.opcode) {
            (1, 0x74..=0x76) => (VectorComparison::Equal, 1 << (decoded.opcode - 0x74)),
            (1, 0x64..=0x66) => (VectorComparison::SignedGreater, 1 << (decoded.opcode - 0x64)),
            (2, 0x29) => (VectorComparison::Equal, 8),
            (2, 0x37) => (VectorComparison::SignedGreater, 8),
            _ => return Err(ScalarIrError::Unsupported),
        };
        let source = if let Some(register) = decoded.register_operand {
            VectorSource::Register(register)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::VectorCompare {
            comparison,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source,
            lane,
        })
    }
}
