use crate::{Aarch64CpuState, Nzcv};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalOperation {
    And,
    Orr,
    Eor,
    Ands,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveWideOperation {
    Not,
    Zero,
    Keep,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterExtension {
    UnsignedByte,
    UnsignedHalf,
    UnsignedWord,
    UnsignedDouble,
    SignedByte,
    SignedHalf,
    SignedWord,
    SignedDouble,
}

impl RegisterExtension {
    pub(crate) fn apply(self, value: u64) -> u64 {
        match self {
            Self::UnsignedByte => u64::from(value as u8),
            Self::UnsignedHalf => u64::from(value as u16),
            Self::UnsignedWord => u64::from(value as u32),
            Self::UnsignedDouble => value,
            Self::SignedByte => (value as i8 as i64) as u64,
            Self::SignedHalf => (value as i16 as i64) as u64,
            Self::SignedWord => (value as i32 as i64) as u64,
            Self::SignedDouble => value,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BranchCondition(pub u8);

impl BranchCondition {
    pub(crate) fn holds(self, flags: Nzcv) -> bool {
        let mut result = match self.0 >> 1 & 7 {
            0 => flags.zero(),
            1 => flags.carry(),
            2 => flags.negative(),
            3 => flags.overflow(),
            4 => flags.carry() && !flags.zero(),
            5 => flags.negative() == flags.overflow(),
            6 => flags.negative() == flags.overflow() && !flags.zero(),
            _ => true,
        };
        if self.0 & 1 != 0 && self.0 & 0xe != 0xe {
            result = !result;
        }
        result
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareOperand {
    Register(u8),
    Immediate(u8),
}

impl CompareOperand {
    pub(crate) fn read(self, cpu: &Aarch64CpuState) -> u64 {
        match self {
            Self::Register(register) => cpu.register(register),
            Self::Immediate(value) => u64::from(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitfieldOperation {
    Signed,
    Insert,
    Unsigned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultiplyOperation {
    Add,
    SignedLong,
    UnsignedLong,
    SignedHigh,
    UnsignedHigh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DivideOperation {
    Signed,
    Unsigned,
}

impl DivideOperation {
    pub(crate) fn apply(self, wide: bool, left: u64, right: u64) -> u64 {
        if (wide && right == 0) || (!wide && right as u32 == 0) {
            return 0;
        }
        match (self, wide) {
            (Self::Unsigned, true) => left / right,
            (Self::Unsigned, false) => u64::from((left as u32) / (right as u32)),
            (Self::Signed, true) if left == i64::MIN as u64 && right == u64::MAX => left,
            (Self::Signed, true) => ((left as i64) / (right as i64)) as u64,
            (Self::Signed, false) if left as u32 == i32::MIN as u32 && right as u32 == u32::MAX => {
                u64::from(left as u32)
            }
            (Self::Signed, false) => u64::from(((left as i32) / (right as i32)) as u32),
        }
    }
}
