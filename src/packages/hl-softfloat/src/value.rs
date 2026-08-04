use core::ops::{BitOr, BitOrAssign};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Binary16,
    Binary32,
    Binary64,
}

impl Format {
    pub const fn width(self) -> u8 {
        match self {
            Self::Binary16 => 16,
            Self::Binary32 => 32,
            Self::Binary64 => 64,
        }
    }

    pub(crate) const fn fraction_bits(self) -> u8 {
        match self {
            Self::Binary16 => 10,
            Self::Binary32 => 23,
            Self::Binary64 => 52,
        }
    }

    pub(crate) const fn exponent_bits(self) -> u8 {
        match self {
            Self::Binary16 => 5,
            Self::Binary32 => 8,
            Self::Binary64 => 11,
        }
    }

    pub(crate) const fn bias(self) -> i32 {
        match self {
            Self::Binary16 => 15,
            Self::Binary32 => 127,
            Self::Binary64 => 1023,
        }
    }

    pub(crate) const fn precision(self) -> u8 {
        self.fraction_bits() + 1
    }

    pub(crate) const fn minimum_exponent(self) -> i32 {
        1 - self.bias()
    }

    pub(crate) const fn maximum_exponent(self) -> i32 {
        self.bias()
    }

    pub(crate) const fn sign_mask(self) -> u64 {
        1_u64 << (self.width() - 1)
    }

    pub(crate) const fn fraction_mask(self) -> u64 {
        (1_u64 << self.fraction_bits()) - 1
    }

    pub(crate) const fn exponent_mask(self) -> u64 {
        (1_u64 << self.exponent_bits()) - 1
    }

    pub(crate) const fn value_mask(self) -> u64 {
        if self.width() == 64 {
            u64::MAX
        } else {
            (1_u64 << self.width()) - 1
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Value {
    format: Format,
    bits: u64,
}

impl Value {
    pub const fn from_bits(format: Format, bits: u64) -> Self {
        Self {
            format,
            bits: bits & format.value_mask(),
        }
    }

    pub const fn format(self) -> Format {
        self.format
    }

    pub const fn bits(self) -> u64 {
        self.bits
    }

    pub(crate) fn order_key(self) -> u64 {
        let sign = self.format.sign_mask();
        if self.bits & sign != 0 {
            !self.bits & self.format.value_mask()
        } else {
            self.bits | sign
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExceptionFlags(u8);

impl ExceptionFlags {
    pub const INVALID: Self = Self(1 << 0);
    pub const DIVIDE_BY_ZERO: Self = Self(1 << 1);
    pub const OVERFLOW: Self = Self(1 << 2);
    pub const UNDERFLOW: Self = Self(1 << 3);
    pub const INEXACT: Self = Self(1 << 4);
    pub const INPUT_DENORMAL: Self = Self(1 << 5);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 != 0
    }
}

impl BitOr for ExceptionFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for ExceptionFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoundingMode {
    NearestEven,
    TowardPositive,
    TowardNegative,
    TowardZero,
    NearestAway,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TininessMode {
    BeforeRounding,
    AfterRounding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NaNMode {
    PropagatePayload,
    Default,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    Less,
    Equal,
    Greater,
    Unordered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Result<T> {
    pub value: T,
    pub flags: ExceptionFlags,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Class {
    Zero,
    Subnormal,
    Normal,
    Infinity,
    QuietNaN,
    SignalingNaN,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Operand {
    pub sign: bool,
    pub exponent: i32,
    pub significand: u128,
    pub class: Class,
    pub bits: u64,
}
