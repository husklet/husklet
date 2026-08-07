use crate::{DecodedInstruction, FloatWidth, ScalarInstruction, ScalarIrError, X87StackOperation};

use super::memory::ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.raw_mod == Some(3) {
            let group = decoded.raw_reg.ok_or(ScalarIrError::Invalid)?;
            let source = decoded.raw_rm.ok_or(ScalarIrError::Invalid)?;
            if matches!(decoded.opcode, 0xd8 | 0xdc | 0xde) && !matches!(group, 2 | 3) {
                return Ok(ScalarInstruction::X87Arithmetic {
                    address: None,
                    source,
                    operation: group,
                    destination_source: decoded.opcode != 0xd8,
                    pop: decoded.opcode == 0xde,
                    format: FloatWidth::Double,
                    integer_bytes: 0,
                });
            }
            if matches!(decoded.opcode, 0xd8 | 0xdc | 0xde) && matches!(group, 2 | 3) {
                return Ok(ScalarInstruction::X87StatusCompare {
                    address: None,
                    source,
                    pop: if group == 3 {
                        1 + u8::from(decoded.opcode == 0xde && source == 1)
                    } else {
                        0
                    },
                    format: FloatWidth::Double,
                    ordered: true,
                });
            }
            if decoded.opcode == 0xdd && matches!(group, 4 | 5) {
                return Ok(ScalarInstruction::X87StatusCompare {
                    address: None,
                    source,
                    pop: u8::from(group == 5),
                    format: FloatWidth::Double,
                    ordered: false,
                });
            }
            if matches!(decoded.opcode, 0xda | 0xdb) && group <= 3 {
                return Ok(ScalarInstruction::X87ConditionalMove {
                    source,
                    condition: group,
                    negate: decoded.opcode == 0xdb,
                });
            }
            match (decoded.opcode, group, source) {
                (0xdb, 4, 3) => return Ok(ScalarInstruction::X87Initialize),
                (0xdf, 4, 0) => return Ok(ScalarInstruction::X87Status),
                (0xd9, 5, constant @ 0..=6) => return Ok(ScalarInstruction::X87Constant { constant }),
                _ => {}
            }
            if decoded.opcode == 0xdb && group == 4 {
                return Ok(ScalarInstruction::X87Unary { operation: 9, source });
            }
            if decoded.opcode == 0xd9
                && matches!(
                    (group, source),
                    (2, 0) | (4, 0 | 1 | 5) | (6 | 7, 0..=7)
                )
            {
                return Ok(ScalarInstruction::X87Unary {
                    operation: group,
                    source,
                });
            }
            if decoded.opcode == 0xdd && group == 0 {
                return Ok(ScalarInstruction::X87Unary { operation: 8, source });
            }
            let operation = match (decoded.opcode, group) {
                (0xd9, 0) => Some(X87StackOperation::Load),
                (0xd9, 1) => Some(X87StackOperation::Exchange),
                (0xdd, 2) => Some(X87StackOperation::Store),
                (0xdd, 3) => Some(X87StackOperation::StorePop),
                _ => None,
            };
            if let Some(operation) = operation {
                return Ok(ScalarInstruction::X87Stack { source, operation });
            }
            if matches!(decoded.opcode, 0xdb | 0xdf) && matches!(group, 5 | 6) {
                return Ok(ScalarInstruction::X87Compare {
                    source,
                    ordered: group == 6,
                    pop: decoded.opcode == 0xdf,
                });
            }
            return Err(ScalarIrError::Unsupported);
        }
        let address = decoded.address.ok_or(ScalarIrError::Invalid)?;
        match (decoded.opcode, decoded.raw_reg) {
            (0xdd, Some(7)) => Ok(ScalarInstruction::X87StatusStore { address }),
            (0xdd, Some(group @ (4 | 6))) => Ok(ScalarInstruction::X87Save {
                address,
                load: group == 4,
            }),
            (0xdb, Some(group @ 0..=3)) => Ok(ScalarInstruction::X87Integer {
                address,
                bytes: 4,
                load: group == 0,
                pop: group != 0 && group != 2,
                truncate: group == 1,
            }),
            (0xdd, Some(1)) => Ok(ScalarInstruction::X87Integer {
                address,
                bytes: 8,
                load: false,
                pop: true,
                truncate: true,
            }),
            (0xdf, Some(group @ (0..=3 | 5 | 7))) => Ok(ScalarInstruction::X87Integer {
                address,
                bytes: if matches!(group, 5 | 7) { 8 } else { 2 },
                load: matches!(group, 0 | 5),
                pop: !matches!(group, 0 | 2 | 5),
                truncate: group == 1,
            }),
            (opcode @ (0xd8 | 0xdc), Some(group @ (2 | 3))) => Ok(ScalarInstruction::X87StatusCompare {
                address: Some(address),
                source: 0,
                pop: u8::from(group == 3),
                format: if opcode == 0xd8 {
                    FloatWidth::Single
                } else {
                    FloatWidth::Double
                },
                ordered: true,
            }),
            (opcode @ (0xd8 | 0xdc), Some(operation)) if !matches!(operation, 2 | 3) => {
                Ok(ScalarInstruction::X87Arithmetic {
                    address: Some(address),
                    source: 0,
                    operation,
                    destination_source: false,
                    pop: false,
                    format: if opcode == 0xd8 {
                        FloatWidth::Single
                    } else {
                        FloatWidth::Double
                    },
                    integer_bytes: 0,
                })
            }
            (opcode @ (0xda | 0xde), Some(operation)) if !matches!(operation, 2 | 3) => {
                Ok(ScalarInstruction::X87Arithmetic {
                    address: Some(address),
                    source: 0,
                    operation,
                    destination_source: false,
                    pop: false,
                    format: FloatWidth::Double,
                    integer_bytes: if opcode == 0xda { 4 } else { 2 },
                })
            }
            (0xd9, Some(group @ (4 | 6))) => Ok(ScalarInstruction::X87Environment {
                address,
                load: group == 4,
            }),
            (0xd9, Some(5 | 7)) => super::Control::decode(decoded),
            (0xdb, Some(5 | 7)) => Ok(ScalarInstruction::X87Extended {
                address,
                load: decoded.raw_reg == Some(5),
            }),
            (0xd9, Some(group @ (0 | 2 | 3))) => Ok(ScalarInstruction::X87Float {
                address,
                format: FloatWidth::Single,
                store: group != 0,
                pop: group == 3,
            }),
            (0xdd, Some(group @ (0 | 2 | 3))) => Ok(ScalarInstruction::X87Float {
                address,
                format: FloatWidth::Double,
                store: group != 0,
                pop: group == 3,
            }),
            _ => Err(ScalarIrError::Unsupported),
        }
    }
}
