use super::{Decoder, Error};
use crate::{
    DecodedInstruction, EffectiveAddress, ScalarInstruction, ScalarOperand, ScalarRegister, ScalarWidth, VectorSource,
};

impl Decoder {
    pub(crate) fn rm(decoded: &DecodedInstruction, byte: bool) -> Result<ScalarOperand, Error> {
        if let Some(address) = decoded.address {
            return Ok(ScalarOperand::Memory(address));
        }
        let raw = decoded.raw_rm.ok_or(Error::Invalid)?;
        Self::register(decoded, raw, Self::rex_b(decoded) != 0, byte)
    }

    pub(crate) fn reg(decoded: &DecodedInstruction, byte: bool) -> Result<ScalarOperand, Error> {
        let raw = decoded.raw_reg.ok_or(Error::Invalid)?;
        Self::register(decoded, raw, decoded.rex().is_some_and(|rex| rex.r), byte)
    }

    pub(super) fn register(
        decoded: &DecodedInstruction,
        raw: u8,
        extended: bool,
        byte: bool,
    ) -> Result<ScalarOperand, Error> {
        let register = if byte {
            ScalarRegister::Byte(decoded.byte_register(raw, extended).ok_or(Error::Invalid)?)
        } else {
            ScalarRegister::General(raw | (u8::from(extended) << 3))
        };
        Ok(ScalarOperand::Register(register))
    }

    pub(crate) fn general_reg(decoded: &DecodedInstruction) -> Result<ScalarRegister, Error> {
        Ok(ScalarRegister::General(decoded.register.ok_or(Error::Invalid)?))
    }

    pub(super) fn opcode_reg(decoded: &DecodedInstruction, raw: u8, byte: bool) -> Result<ScalarOperand, Error> {
        Self::register(decoded, raw, Self::rex_b(decoded) != 0, byte)
    }

    pub(super) fn immediate(decoded: &DecodedInstruction) -> Result<ScalarOperand, Error> {
        Ok(ScalarOperand::Immediate(decoded.immediate.ok_or(Error::Invalid)?.0))
    }

    pub(super) fn vector_source(decoded: &DecodedInstruction) -> Result<VectorSource, Error> {
        if let Some(register) = decoded.register_operand {
            return Ok(VectorSource::Register(register));
        }
        decoded.address.map(VectorSource::Memory).ok_or(Error::Invalid)
    }

    pub(super) fn moffs(decoded: &DecodedInstruction) -> Result<ScalarOperand, Error> {
        let displacement = decoded.immediate.ok_or(Error::Invalid)?.0;
        Ok(ScalarOperand::Memory(EffectiveAddress {
            displacement,
            address_32: decoded.prefixes.address_32,
            segment: decoded.prefixes.segment,
            ..EffectiveAddress::default()
        }))
    }

    pub(super) fn target(decoded: &DecodedInstruction, address: u64) -> Result<u64, Error> {
        let displacement = decoded.immediate.ok_or(Error::Invalid)?.0;
        Ok(address
            .wrapping_add(u64::from(decoded.length))
            .wrapping_add(displacement as u64))
    }

    pub(super) fn width(decoded: &DecodedInstruction, byte: bool) -> ScalarWidth {
        if byte {
            return ScalarWidth::Byte;
        }
        if decoded.rex().is_some_and(|rex| rex.w) {
            ScalarWidth::Qword
        } else if decoded.prefixes.operand_16 {
            ScalarWidth::Word
        } else {
            ScalarWidth::Dword
        }
    }

    pub(super) fn accumulator_source_width(decoded: &DecodedInstruction) -> ScalarWidth {
        if decoded.rex().is_some_and(|rex| rex.w) {
            ScalarWidth::Dword
        } else if decoded.prefixes.operand_16 {
            ScalarWidth::Byte
        } else {
            ScalarWidth::Word
        }
    }

    pub(super) fn instruction_width(decoded: &DecodedInstruction, instruction: ScalarInstruction) -> ScalarWidth {
        if matches!(instruction, ScalarInstruction::ByteSwap { .. }) {
            return if decoded.rex().is_some_and(|rex| rex.w) {
                ScalarWidth::Qword
            } else {
                ScalarWidth::Dword
            };
        }
        if let ScalarInstruction::CompareExchange { source, .. } = instruction {
            return if matches!(source, ScalarRegister::Byte(_)) {
                ScalarWidth::Byte
            } else {
                Self::width(decoded, false)
            };
        }
        if matches!(
            instruction,
            ScalarInstruction::ReadSelector {
                destination: ScalarOperand::Memory(_),
                ..
            }
        ) {
            return ScalarWidth::Word;
        }
        if matches!(instruction, ScalarInstruction::BitOperation { .. }) {
            return Self::width(decoded, false);
        }
        if matches!(instruction, ScalarInstruction::VectorMove { .. }) {
            let wide = match decoded.rex() {
                Some(rex) => rex.w,
                None => false,
            };
            return if wide { ScalarWidth::Qword } else { ScalarWidth::Dword };
        }
        if matches!(
            instruction,
            ScalarInstruction::Push { .. }
                | ScalarInstruction::Pop { .. }
                | ScalarInstruction::PushFlags
                | ScalarInstruction::PopFlags
                | ScalarInstruction::Call { .. }
                | ScalarInstruction::CallIndirect { .. }
                | ScalarInstruction::Return { .. }
                | ScalarInstruction::Leave { .. }
        ) {
            let rex_wide = match decoded.rex() {
                Some(rex) => rex.w,
                None => false,
            };
            return if decoded.prefixes.operand_16 && !rex_wide {
                ScalarWidth::Word
            } else {
                ScalarWidth::Qword
            };
        }
        if matches!(instruction, ScalarInstruction::VectorMask { .. }) {
            return ScalarWidth::Dword;
        }
        if matches!(
            instruction,
            ScalarInstruction::Increment { .. } | ScalarInstruction::DoubleShift { .. }
        ) {
            let byte = matches!(instruction, ScalarInstruction::Increment { .. }) && decoded.opcode == 0xfe;
            return Self::width(decoded, byte);
        }
        if matches!(
            instruction,
            ScalarInstruction::MoveZeroExtend { .. } | ScalarInstruction::AccumulatorSignExtend { .. }
        ) {
            return Self::width(decoded, false);
        }
        let byte = matches!(
            decoded.opcode,
            0x00 | 0x02
                | 0x04
                | 0x08
                | 0x0a
                | 0x0c
                | 0x10
                | 0x12
                | 0x14
                | 0x18
                | 0x1a
                | 0x1c
                | 0x20
                | 0x22
                | 0x24
                | 0x28
                | 0x2a
                | 0x2c
                | 0x30
                | 0x32
                | 0x34
                | 0x38
                | 0x3a
                | 0x3c
                | 0x80
                | 0x84
                | 0x86
                | 0x88
                | 0x8a
                | 0xa0
                | 0xa2
                | 0xa4
                | 0xa6
                | 0xa8
                | 0xaa
                | 0xac
                | 0xae
                | 0xb0..=0xb7 | 0xc0 | 0xc6 | 0xd0 | 0xd2 | 0xf6
        );
        Self::width(decoded, byte)
    }

    pub(super) fn rex_b(decoded: &DecodedInstruction) -> u8 {
        u8::from(decoded.rex().is_some_and(|rex| rex.b)) << 3
    }
}
