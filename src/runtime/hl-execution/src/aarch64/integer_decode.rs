use super::decode::{Aarch64DecodeError, Aarch64Decoder};
use crate::{
    Aarch64BranchCondition, Aarch64Instruction, Aarch64Ir, Aarch64Shift, BitfieldOperation, CompareOperand,
    DivideOperation, LogicalOperation, MoveWideOperation, MultiplyOperation, RegisterExtension,
};

impl Aarch64Decoder {
    pub(super) fn ir(word: u32, instruction: Aarch64Instruction) -> Aarch64Ir {
        Aarch64Ir {
            word,
            wide: word >> 31 != 0,
            instruction,
        }
    }

    pub(super) fn byte_reverse(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let operation = word >> 10 & 0x3f;
        if !wide && operation == 3 {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::ByteReverse {
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            container_bytes: 1 << operation,
        })
    }

    pub(super) fn extract(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let n = word >> 22 & 1 != 0;
        let amount = (word >> 10 & 0x3f) as u8;
        if n != wide || (!wide && amount >= 32) {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::Extract {
            high: (word >> 5 & 31) as u8,
            low: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
            amount,
        })
    }

    pub(super) fn address(word: u32) -> Aarch64Instruction {
        let immediate = u64::from((word >> 5 & 0x7ffff) << 2 | (word >> 29 & 3));
        Aarch64Instruction::Address {
            destination: (word & 31) as u8,
            displacement: Self::sign_extend(immediate, 21),
            page: word >> 31 != 0,
        }
    }

    pub(super) fn add_immediate(word: u32) -> Aarch64Instruction {
        let mut immediate = u64::from(word >> 10 & 0xfff);
        if word >> 22 & 1 != 0 {
            immediate <<= 12;
        }
        Aarch64Instruction::AddSubtractImmediate {
            subtract: word >> 30 & 1 != 0,
            set_flags: word >> 29 & 1 != 0,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            immediate,
        }
    }

    pub(super) fn add_shifted(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let shift = match word >> 22 & 3 {
            0 => Aarch64Shift::Lsl,
            1 => Aarch64Shift::Lsr,
            2 => Aarch64Shift::Asr,
            _ => return Err(Aarch64DecodeError::Reserved),
        };
        let amount = (word >> 10 & 63) as u8;
        if !wide && amount >= 32 {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::AddSubtractShifted {
            subtract: word >> 30 & 1 != 0,
            set_flags: word >> 29 & 1 != 0,
            source: (word >> 5 & 31) as u8,
            operand: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
            shift,
            amount,
        })
    }

    pub(super) fn add_extended(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let option = (word >> 13 & 7) as u8;
        let amount = (word >> 10 & 7) as u8;
        if amount > 4 || (!wide && option & 2 != 0) {
            return Err(Aarch64DecodeError::Reserved);
        }
        let extension = match option {
            0 => RegisterExtension::UnsignedByte,
            1 => RegisterExtension::UnsignedHalf,
            2 => RegisterExtension::UnsignedWord,
            3 => RegisterExtension::UnsignedDouble,
            4 => RegisterExtension::SignedByte,
            5 => RegisterExtension::SignedHalf,
            6 => RegisterExtension::SignedWord,
            _ => RegisterExtension::SignedDouble,
        };
        Ok(Aarch64Instruction::AddSubtractExtended {
            subtract: word >> 30 & 1 != 0,
            set_flags: word >> 29 & 1 != 0,
            source: (word >> 5 & 31) as u8,
            operand: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
            extension,
            amount,
        })
    }

    pub(super) fn logical_immediate(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let mask = Self::bit_masks(
            wide,
            word >> 22 & 1 != 0,
            (word >> 10 & 63) as u8,
            (word >> 16 & 63) as u8,
            true,
        )?
        .0;
        Ok(Aarch64Instruction::LogicalImmediate {
            operation: Self::logical_operation((word >> 29 & 3) as u8),
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            mask,
        })
    }

    pub(super) fn logical_shifted(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let amount = (word >> 10 & 63) as u8;
        if !wide && amount >= 32 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let shift = match word >> 22 & 3 {
            0 => Aarch64Shift::Lsl,
            1 => Aarch64Shift::Lsr,
            2 => Aarch64Shift::Asr,
            _ => Aarch64Shift::Ror,
        };
        Ok(Aarch64Instruction::LogicalShifted {
            operation: Self::logical_operation((word >> 29 & 3) as u8),
            invert: word >> 21 & 1 != 0,
            source: (word >> 5 & 31) as u8,
            operand: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
            shift,
            amount,
        })
    }

    pub(super) fn move_wide(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let operation = match word >> 29 & 3 {
            0 => MoveWideOperation::Not,
            2 => MoveWideOperation::Zero,
            3 => MoveWideOperation::Keep,
            _ => return Err(Aarch64DecodeError::Reserved),
        };
        let shift = ((word >> 21 & 3) * 16) as u8;
        if !wide && shift >= 32 {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::MoveWide {
            operation,
            destination: (word & 31) as u8,
            immediate: (word >> 5 & 0xffff) as u16,
            shift,
        })
    }

    pub(super) fn multiply(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let operation = match word >> 21 & 7 {
            0 => MultiplyOperation::Add,
            1 if wide => MultiplyOperation::SignedLong,
            2 if wide && word >> 15 & 1 == 0 && word >> 10 & 31 == 31 => MultiplyOperation::SignedHigh,
            5 if wide => MultiplyOperation::UnsignedLong,
            6 if wide && word >> 15 & 1 == 0 && word >> 10 & 31 == 31 => MultiplyOperation::UnsignedHigh,
            _ => return Err(Aarch64DecodeError::Reserved),
        };
        Ok(Aarch64Instruction::Multiply {
            operation,
            subtract: word >> 15 & 1 != 0,
            source: (word >> 5 & 31) as u8,
            operand: (word >> 16 & 31) as u8,
            addend: (word >> 10 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(super) fn variable_shift(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 29 & 3 != 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let shift = match word >> 10 & 3 {
            0 => Aarch64Shift::Lsl,
            1 => Aarch64Shift::Lsr,
            2 => Aarch64Shift::Asr,
            _ => Aarch64Shift::Ror,
        };
        Ok(Aarch64Instruction::VariableShift {
            shift,
            source: (word >> 5 & 31) as u8,
            amount: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(super) fn divide(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 29 & 3 != 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::Divide {
            operation: if word >> 10 & 1 == 0 {
                DivideOperation::Unsigned
            } else {
                DivideOperation::Signed
            },
            source: (word >> 5 & 31) as u8,
            divisor: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(super) fn conditional_compare(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 29 & 1 == 0 || word >> 10 & 1 != 0 || word >> 4 & 1 != 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let value = (word >> 16 & 31) as u8;
        Ok(Aarch64Instruction::ConditionalCompare {
            subtract: word >> 30 & 1 != 0,
            source: (word >> 5 & 31) as u8,
            operand: if word >> 11 & 1 != 0 {
                CompareOperand::Immediate(value)
            } else {
                CompareOperand::Register(value)
            },
            condition: Aarch64BranchCondition((word >> 12 & 15) as u8),
            literal: (word & 15) as u8,
        })
    }

    pub(super) fn bitfield(word: u32, wide: bool) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let operation = match word >> 29 & 3 {
            0 => BitfieldOperation::Signed,
            1 => BitfieldOperation::Insert,
            2 => BitfieldOperation::Unsigned,
            _ => return Err(Aarch64DecodeError::Reserved),
        };
        let n = word >> 22 & 1 != 0;
        let rotate = (word >> 16 & 63) as u8;
        let sign_bit = (word >> 10 & 63) as u8;
        if n != wide || (!wide && (rotate | sign_bit) & 32 != 0) {
            return Err(Aarch64DecodeError::Reserved);
        }
        let (write_mask, top_mask) = Self::bit_masks(wide, n, sign_bit, rotate, false)?;
        Ok(Aarch64Instruction::Bitfield {
            operation,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            rotate,
            sign_bit,
            write_mask,
            top_mask,
        })
    }

    pub(super) fn branch_register(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        if word >> 16 & 31 != 31 || word >> 10 & 63 != 0 || word & 31 != 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let source = (word >> 5 & 31) as u8;
        match word >> 21 & 15 {
            0 => Ok(Aarch64Instruction::BranchRegister { source, link: false }),
            1 => Ok(Aarch64Instruction::BranchRegister { source, link: true }),
            2 => Ok(Aarch64Instruction::Return { source }),
            _ => Err(Aarch64DecodeError::Reserved),
        }
    }

    pub(super) fn logical_operation(value: u8) -> LogicalOperation {
        match value {
            0 => LogicalOperation::And,
            1 => LogicalOperation::Orr,
            2 => LogicalOperation::Eor,
            _ => LogicalOperation::Ands,
        }
    }

    pub(super) fn bit_masks(
        wide: bool,
        n: bool,
        imms: u8,
        immr: u8,
        immediate: bool,
    ) -> Result<(u64, u64), Aarch64DecodeError> {
        let combined = u32::from(n) << 6 | u32::from(!imms & 0x3f);
        let length = 31_u32
            .checked_sub(combined.leading_zeros())
            .ok_or(Aarch64DecodeError::Reserved)?;
        if length < 1 || (!wide && n) {
            return Err(Aarch64DecodeError::Reserved);
        }
        let levels = (1_u32 << length) - 1;
        if immediate && u32::from(imms) & levels == levels {
            return Err(Aarch64DecodeError::Reserved);
        }
        let size = 1_u32 << length;
        let set = (u32::from(imms) & levels) + 1;
        let rotate = u32::from(immr) & levels;
        let diff = (u32::from(imms).wrapping_sub(u32::from(immr))) & levels;
        let element_mask = if size == 64 { u64::MAX } else { (1_u64 << size) - 1 };
        let element = if set == 64 { u64::MAX } else { (1_u64 << set) - 1 };
        let rotated = if rotate == 0 {
            element & element_mask
        } else {
            (element >> rotate | element << (size - rotate)) & element_mask
        };
        let top = if diff + 1 == 64 {
            u64::MAX
        } else {
            (1_u64 << (diff + 1)) - 1
        };
        let mut mask = 0;
        let mut top_mask = 0;
        let mut offset = 0;
        while offset < 64 {
            mask |= rotated << offset;
            top_mask |= (top & element_mask) << offset;
            offset += size;
        }
        Ok(if wide {
            (mask, top_mask)
        } else {
            (u64::from(mask as u32), u64::from(top_mask as u32))
        })
    }

    pub(super) fn sign_extend(value: u64, bits: u32) -> i64 {
        ((value << (64 - bits)) as i64) >> (64 - bits)
    }
}
