use crate::{FlagUpdate, IntegerWidth};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DivisionError {
    Zero,
    QuotientOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Division {
    pub quotient: u64,
    pub remainder: u64,
}

impl Division {
    pub fn unsigned(width: IntegerWidth, high: u64, low: u64, divisor: u64) -> Result<Self, DivisionError> {
        let mask = width.mask();
        let divisor = divisor & mask;
        if divisor == 0 {
            return Err(DivisionError::Zero);
        }
        let bits = mask.count_ones();
        let dividend = if bits == 8 {
            u128::from(low & 0xffff)
        } else {
            (u128::from(high & mask) << bits) | u128::from(low & mask)
        };
        let quotient = dividend / u128::from(divisor);
        if quotient > u128::from(mask) {
            return Err(DivisionError::QuotientOverflow);
        }
        Ok(Self {
            quotient: quotient as u64,
            remainder: (dividend % u128::from(divisor)) as u64,
        })
    }

    pub fn signed(width: IntegerWidth, high: u64, low: u64, divisor: u64) -> Result<Self, DivisionError> {
        let bits = width.mask().count_ones();
        let divisor = Multiplication::sign(divisor, bits);
        if divisor == 0 {
            return Err(DivisionError::Zero);
        }
        let dividend = if bits == 8 {
            Multiplication::sign(low, 16)
        } else if bits == 64 {
            (i128::from(high as i64) << 64) | i128::from(low)
        } else {
            Multiplication::sign(
                ((high as u128) << bits | u128::from(low & width.mask())) as u64,
                bits * 2,
            )
        };
        if dividend == i128::MIN && divisor == -1 {
            return Err(DivisionError::QuotientOverflow);
        }
        let quotient = dividend / divisor;
        let minimum = -(1_i128 << (bits - 1));
        let maximum = (1_i128 << (bits - 1)) - 1;
        if !(minimum..=maximum).contains(&quotient) {
            return Err(DivisionError::QuotientOverflow);
        }
        Ok(Self {
            quotient: quotient as u64 & width.mask(),
            remainder: (dividend % divisor) as u64 & width.mask(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Multiplication {
    pub low: u64,
    pub high: u64,
    pub flags: FlagUpdate,
}

impl Multiplication {
    #[must_use]
    pub fn widening(width: IntegerWidth, left: u64, right: u64, signed: bool) -> Self {
        let bits = width.mask().count_ones();
        let mask = width.mask();
        let product = if signed {
            (Self::sign(left & mask, bits) * Self::sign(right & mask, bits)) as u128
        } else {
            u128::from(left & mask) * u128::from(right & mask)
        };
        let low = product as u64 & mask;
        let high = (product >> bits) as u64 & mask;
        let overflow = if signed {
            Self::sign(low, bits) != product as i128
        } else {
            high != 0
        };
        Self {
            low,
            high,
            flags: FlagUpdate::overflow_and_carry(overflow),
        }
    }

    #[must_use]
    pub fn truncating(width: IntegerWidth, left: u64, right: u64) -> Self {
        let bits = width.mask().count_ones();
        let mask = width.mask();
        let product = Self::sign(left & mask, bits) * Self::sign(right & mask, bits);
        let low = product as u64 & width.mask();
        Self {
            low,
            high: 0,
            flags: FlagUpdate::overflow_and_carry(Self::sign(low, bits) != product),
        }
    }
    fn sign(value: u64, bits: u32) -> i128 {
        let shift = 128 - bits;
        (i128::from(value) << shift) >> shift
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitAction {
    Test,
    Set,
    Reset,
    Complement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitPlan {
    pub byte_delta: i64,
    pub bit: u8,
    pub prior: bool,
    pub proposed: u8,
}

impl BitPlan {
    #[must_use]
    pub fn memory(index: i64, byte: u8, action: BitAction) -> Self {
        let bit = (index & 7) as u8;
        let mask = 1 << bit;
        let proposed = match action {
            BitAction::Test => byte,
            BitAction::Set => byte | mask,
            BitAction::Reset => byte & !mask,
            BitAction::Complement => byte ^ mask,
        };
        Self {
            byte_delta: index >> 3,
            bit,
            prior: byte & mask != 0,
            proposed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitScan {
    pub result: Option<u64>,
    pub carry: bool,
    pub zero: bool,
}

impl BitScan {
    #[must_use]
    pub fn forward(width: IntegerWidth, source: u64) -> Self {
        Self::scan(width, source, false, false)
    }
    #[must_use]
    pub fn reverse(width: IntegerWidth, source: u64) -> Self {
        Self::scan(width, source, true, false)
    }
    #[must_use]
    pub fn trailing_zero_count(width: IntegerWidth, source: u64) -> Self {
        Self::scan(width, source, false, true)
    }
    #[must_use]
    pub fn leading_zero_count(width: IntegerWidth, source: u64) -> Self {
        Self::scan(width, source, true, true)
    }
    fn scan(width: IntegerWidth, source: u64, reverse: bool, count: bool) -> Self {
        let value = source & width.mask();
        let bits = width.mask().count_ones();
        if value == 0 {
            return Self {
                result: if count { Some(u64::from(bits)) } else { None },
                carry: count,
                zero: !count,
            };
        }
        let result = if reverse {
            u64::from(63 - value.leading_zeros())
        } else {
            u64::from(value.trailing_zeros())
        };
        // lzcnt returns a count, not the bit index the reverse scan produces, and ZF follows the
        // value the instruction writes back.
        let written = if count && reverse {
            u64::from(bits - 1) - result
        } else {
            result
        };
        Self {
            result: Some(written),
            carry: false,
            zero: count && written == 0,
        }
    }
}
