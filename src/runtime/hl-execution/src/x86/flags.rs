#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerWidth {
    Byte,
    Word,
    Dword,
    Qword,
}

impl IntegerWidth {
    const fn bits(self) -> u32 {
        match self {
            Self::Byte => 8,
            Self::Word => 16,
            Self::Dword => 32,
            Self::Qword => 64,
        }
    }
    #[must_use]
    pub const fn mask(self) -> u64 {
        if self.bits() == 64 {
            u64::MAX
        } else {
            (1_u64 << self.bits()) - 1
        }
    }
    const fn truncate(self, value: u64) -> u64 {
        value & self.mask()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Flag {
    Carry = 0,
    Parity = 2,
    Auxiliary = 4,
    Zero = 6,
    Sign = 7,
    Overflow = 11,
}

impl Flag {
    const fn mask(self) -> u16 {
        1 << self as u8
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FlagState(u16);

impl FlagState {
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }
    #[must_use]
    pub const fn contains(self, flag: Flag) -> bool {
        self.0 & flag.mask() != 0
    }
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }
    #[must_use]
    pub const fn with(self, flag: Flag, value: bool) -> Self {
        let mask = flag.mask();
        Self((self.0 & !mask) | if value { mask } else { 0 })
    }
    #[must_use]
    pub const fn apply(self, update: FlagUpdate) -> Self {
        Self((self.0 & !update.defined) | (update.values & update.defined))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlagUpdate {
    values: u16,
    defined: u16,
    undefined: u16,
}

impl FlagUpdate {
    #[must_use]
    pub const fn values(self) -> u16 {
        self.values
    }
    #[must_use]
    pub const fn defined(self) -> u16 {
        self.defined
    }
    #[must_use]
    pub const fn undefined(self) -> u16 {
        self.undefined
    }
    #[must_use]
    pub const fn preserved(self, flag: Flag) -> bool {
        (self.defined | self.undefined) & flag.mask() == 0
    }
    #[must_use]
    pub const fn overflow_and_carry(value: bool) -> Self {
        let mask = Flag::Carry.mask() | Flag::Overflow.mask();
        Self {
            values: if value { mask } else { 0 },
            defined: mask,
            undefined: Flag::Parity.mask() | Flag::Auxiliary.mask() | Flag::Zero.mask() | Flag::Sign.mask(),
        }
    }

    #[must_use]
    pub fn truncated_multiply(width: IntegerWidth, result: u64, overflow: bool) -> Self {
        let result = width.truncate(result);
        let mut values = Arithmetic::common(width, result);
        if overflow {
            values |= Flag::Carry.mask() | Flag::Overflow.mask();
        }
        let defined =
            Flag::Carry.mask() | Flag::Overflow.mask() | Flag::Parity.mask() | Flag::Zero.mask() | Flag::Sign.mask();
        Self {
            values,
            defined,
            undefined: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Arithmetic {
    pub result: u64,
    pub flags: FlagUpdate,
}

impl Arithmetic {
    #[must_use]
    pub fn shift_left_double(width: IntegerWidth, value: u64, fill: u64, count: u8) -> Self {
        Self::double_shift(width, value, fill, count, false)
    }

    #[must_use]
    pub fn shift_right_double(width: IntegerWidth, value: u64, fill: u64, count: u8) -> Self {
        Self::double_shift(width, value, fill, count, true)
    }

    fn double_shift(width: IntegerWidth, value: u64, fill: u64, count: u8, right: bool) -> Self {
        let bits = width.bits();
        let masked = u32::from(count) & if bits == 64 { 63 } else { 31 };
        let value = width.truncate(value);
        let fill = width.truncate(fill);
        if masked == 0 || masked > bits {
            return Self::preserved(value);
        }
        let result = if masked == bits {
            fill
        } else if right {
            width.truncate((value >> masked) | (fill << (bits - masked)))
        } else {
            width.truncate((value << masked) | (fill >> (bits - masked)))
        };
        let carry = if right {
            value >> (masked - 1) & 1 != 0
        } else {
            value >> (bits - masked) & 1 != 0
        };
        let overflow = result >> (bits - 1) & 1 != value >> (bits - 1) & 1;
        let defined = Flag::Carry.mask()
            | Flag::Parity.mask()
            | Flag::Zero.mask()
            | Flag::Sign.mask()
            | if masked == 1 { Flag::Overflow.mask() } else { 0 };
        let values = Self::common(width, result)
            | if carry { Flag::Carry.mask() } else { 0 }
            | if masked == 1 && overflow {
                Flag::Overflow.mask()
            } else {
                0
            };
        Self {
            result,
            flags: FlagUpdate {
                values,
                defined,
                undefined: Flag::Auxiliary.mask() | if masked == 1 { 0 } else { Flag::Overflow.mask() },
            },
        }
    }

    #[must_use]
    pub fn retained_sub_nzcv(width: IntegerWidth, left: u64, right: u64) -> u64 {
        let operation = Self::sub(width, left, right, false);
        let negative = operation.flags.values & Flag::Sign.mask() != 0;
        let zero = operation.flags.values & Flag::Zero.mask() != 0;
        let no_borrow = operation.flags.values & Flag::Carry.mask() == 0;
        let overflow = operation.flags.values & Flag::Overflow.mask() != 0;
        (u64::from(negative) << 31)
            | (u64::from(zero) << 30)
            | (u64::from(no_borrow) << 29)
            | (u64::from(overflow) << 28)
    }

    #[must_use]
    pub fn add(width: IntegerWidth, left: u64, right: u64, carry: bool) -> Self {
        let mask = width.mask();
        let a = left & mask;
        let b = right & mask;
        let c = u64::from(carry);
        let result = a.wrapping_add(b).wrapping_add(c) & mask;
        let carry = (a as u128) + (b as u128) + (c as u128) > mask as u128;
        let overflow = (!(a ^ b) & (a ^ result)) >> (width.bits() - 1) & 1 != 0;
        Self::full(width, result, carry, overflow, (a ^ b ^ result) & 0x10 != 0)
    }

    #[must_use]
    pub fn sub(width: IntegerWidth, left: u64, right: u64, borrow: bool) -> Self {
        let mask = width.mask();
        let a = left & mask;
        let b = right & mask;
        let c = u64::from(borrow);
        let result = a.wrapping_sub(b).wrapping_sub(c) & mask;
        let carry = (a as u128) < (b as u128) + (c as u128);
        let overflow = ((a ^ b) & (a ^ result)) >> (width.bits() - 1) & 1 != 0;
        Self::full(width, result, carry, overflow, (a ^ b ^ result) & 0x10 != 0)
    }

    #[must_use]
    pub fn logic(width: IntegerWidth, result: u64) -> Self {
        let result = width.truncate(result);
        let defined = Flag::Carry.mask()
            | Flag::Overflow.mask()
            | Flag::Parity.mask()
            | Flag::Auxiliary.mask()
            | Flag::Zero.mask()
            | Flag::Sign.mask();
        let values = Self::common(width, result);
        Self {
            result,
            flags: FlagUpdate {
                values,
                defined,
                undefined: 0,
            },
        }
    }

    #[must_use]
    pub fn increment(width: IntegerWidth, value: u64) -> Self {
        let mut operation = Self::add(width, value, 1, false);
        operation.flags.defined &= !Flag::Carry.mask();
        operation
    }

    #[must_use]
    pub fn decrement(width: IntegerWidth, value: u64) -> Self {
        let mut operation = Self::sub(width, value, 1, false);
        operation.flags.defined &= !Flag::Carry.mask();
        operation
    }

    fn full(width: IntegerWidth, result: u64, carry: bool, overflow: bool, auxiliary: bool) -> Self {
        let defined = Flag::Carry.mask()
            | Flag::Parity.mask()
            | Flag::Auxiliary.mask()
            | Flag::Zero.mask()
            | Flag::Sign.mask()
            | Flag::Overflow.mask();
        let values = Self::common(width, result)
            | if carry { Flag::Carry.mask() } else { 0 }
            | if overflow { Flag::Overflow.mask() } else { 0 }
            | if auxiliary { Flag::Auxiliary.mask() } else { 0 };
        Self {
            result,
            flags: FlagUpdate {
                values,
                defined,
                undefined: 0,
            },
        }
    }

    fn common(width: IntegerWidth, result: u64) -> u16 {
        let parity = (result as u8).count_ones().is_multiple_of(2);
        (if parity { Flag::Parity.mask() } else { 0 })
            | (if result == 0 { Flag::Zero.mask() } else { 0 })
            | (if result >> (width.bits() - 1) & 1 != 0 {
                Flag::Sign.mask()
            } else {
                0
            })
    }

    #[must_use]
    pub fn shift_left(width: IntegerWidth, value: u64, count: u8) -> Self {
        Self::shift(width, value, count, Shift::Left)
    }

    #[must_use]
    pub fn shift_right(width: IntegerWidth, value: u64, count: u8) -> Self {
        Self::shift(width, value, count, Shift::Right)
    }

    #[must_use]
    pub fn shift_arithmetic_right(width: IntegerWidth, value: u64, count: u8) -> Self {
        Self::shift(width, value, count, Shift::ArithmeticRight)
    }

    #[must_use]
    pub fn rotate_left(width: IntegerWidth, value: u64, count: u8) -> Self {
        Self::rotate(width, value, count, false)
    }

    #[must_use]
    pub fn rotate_right(width: IntegerWidth, value: u64, count: u8) -> Self {
        Self::rotate(width, value, count, true)
    }

    #[must_use]
    pub fn rotate_carry_left(width: IntegerWidth, value: u64, count: u8, carry: bool) -> Self {
        Self::rotate_carry(width, value, count, carry, false)
    }

    #[must_use]
    pub fn rotate_carry_right(width: IntegerWidth, value: u64, count: u8, carry: bool) -> Self {
        Self::rotate_carry(width, value, count, carry, true)
    }

    fn shift(width: IntegerWidth, value: u64, count: u8, operation: Shift) -> Self {
        let bits = width.bits();
        let masked = u32::from(count) & if bits == 64 { 63 } else { 31 };
        let value = width.truncate(value);
        if masked == 0 {
            return Self::preserved(value);
        }
        let carry_defined = masked <= bits;
        let (result, carry) = match operation {
            Shift::Left => (
                if masked < bits {
                    width.truncate(value << masked)
                } else {
                    0
                },
                carry_defined && value >> (bits - masked) & 1 != 0,
            ),
            Shift::Right => (
                if masked < bits { value >> masked } else { 0 },
                carry_defined && value >> (masked - 1) & 1 != 0,
            ),
            Shift::ArithmeticRight => {
                let signed = ((value << (64 - bits)) as i64) >> (64 - bits);
                let carry = if masked > bits {
                    value >> (bits - 1) & 1 != 0
                } else {
                    value >> (masked - 1) & 1 != 0
                };
                (width.truncate((signed >> masked.min(bits)) as u64), carry)
            }
        };
        let overflow = match operation {
            Shift::Left => result >> (bits - 1) & 1 != u64::from(carry),
            Shift::Right => value >> (bits - 1) & 1 != 0,
            Shift::ArithmeticRight => false,
        };
        let defined = Flag::Carry.mask()
            | Flag::Parity.mask()
            | Flag::Zero.mask()
            | Flag::Sign.mask()
            | if masked == 1 { Flag::Overflow.mask() } else { 0 };
        let values = Self::common(width, result)
            | if carry { Flag::Carry.mask() } else { 0 }
            | if overflow { Flag::Overflow.mask() } else { 0 };
        Self {
            result,
            flags: FlagUpdate {
                values,
                defined,
                undefined: Flag::Auxiliary.mask() | if masked == 1 { 0 } else { Flag::Overflow.mask() },
            },
        }
    }

    fn rotate(width: IntegerWidth, value: u64, count: u8, right: bool) -> Self {
        let bits = width.bits();
        let masked = u32::from(count) & if bits == 64 { 63 } else { 31 };
        let effective = masked % bits;
        let value = width.truncate(value);
        if masked == 0 {
            return Self::preserved(value);
        }
        let result = width.truncate(if right {
            (value >> effective) | (value << (bits - effective))
        } else {
            (value << effective) | (value >> (bits - effective))
        });
        let carry = if right {
            result >> (bits - 1) & 1 != 0
        } else {
            result & 1 != 0
        };
        let overflow = if right {
            (result >> (bits - 1) ^ result >> (bits - 2)) & 1 != 0
        } else {
            result >> (bits - 1) & 1 != u64::from(carry)
        };
        Self::rotate_flags(result, carry, overflow, masked)
    }

    fn rotate_carry(width: IntegerWidth, value: u64, count: u8, carry: bool, right: bool) -> Self {
        let bits = width.bits();
        let masked = u32::from(count) & if bits == 64 { 63 } else { 31 };
        let effective = masked % (bits + 1);
        let value = width.truncate(value);
        if effective == 0 {
            return Self::preserved(value);
        }
        let combined = (u128::from(carry) << bits) | u128::from(value);
        let total = bits + 1;
        let mask = (1_u128 << total) - 1;
        let rotated = if right {
            (combined >> effective) | (combined << (total - effective) & mask)
        } else {
            (combined << effective & mask) | (combined >> (total - effective))
        };
        let result = width.truncate(rotated as u64);
        let carry = rotated >> bits & 1 != 0;
        let overflow = if right {
            (result >> (bits - 1) ^ result >> (bits - 2)) & 1 != 0
        } else {
            result >> (bits - 1) & 1 != u64::from(carry)
        };
        Self::rotate_flags(result, carry, overflow, masked)
    }

    fn rotate_flags(result: u64, carry: bool, overflow: bool, masked: u32) -> Self {
        let mut defined = Flag::Carry.mask();
        let mut undefined = Flag::Overflow.mask();
        if masked == 1 {
            defined |= Flag::Overflow.mask();
            undefined = 0;
        }
        let values = if carry { Flag::Carry.mask() } else { 0 }
            | if overflow && masked == 1 {
                Flag::Overflow.mask()
            } else {
                0
            };
        Self {
            result,
            flags: FlagUpdate {
                values,
                defined,
                undefined,
            },
        }
    }

    fn preserved(result: u64) -> Self {
        Self {
            result,
            flags: FlagUpdate {
                values: 0,
                defined: 0,
                undefined: 0,
            },
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn logical_operations_clear_auxiliary() {
        let original = FlagState::from_bits(Flag::Auxiliary.mask());
        assert!(
            !original
                .apply(Arithmetic::logic(IntegerWidth::Byte, 0x8b).flags)
                .contains(Flag::Auxiliary)
        );
    }

    #[test]
    fn multi_bit_shifts_preserve_undefined_overflow() {
        type Shift = fn(IntegerWidth, u64, u8) -> Arithmetic;
        let original = FlagState::from_bits(Flag::Carry.mask() | Flag::Overflow.mask());
        for width in [
            IntegerWidth::Byte,
            IntegerWidth::Word,
            IntegerWidth::Dword,
            IntegerWidth::Qword,
        ] {
            for operation in [
                Arithmetic::shift_left,
                Arithmetic::shift_right,
                Arithmetic::shift_arithmetic_right,
            ] as [Shift; 3]
            {
                let update = operation(width, 0x8000_0000_0000_0080, 2).flags;
                assert_eq!(update.defined() & Flag::Overflow.mask(), 0);
                assert_ne!(update.undefined() & Flag::Overflow.mask(), 0);
                assert!(original.apply(update).contains(Flag::Overflow));
                assert!(!FlagState::default().apply(update).contains(Flag::Overflow));
            }
        }
    }

    #[test]
    fn one_bit_shifts_define_overflow() {
        type Shift = fn(IntegerWidth, u64, u8) -> Arithmetic;
        for width in [
            IntegerWidth::Byte,
            IntegerWidth::Word,
            IntegerWidth::Dword,
            IntegerWidth::Qword,
        ] {
            for operation in [
                Arithmetic::shift_left,
                Arithmetic::shift_right,
                Arithmetic::shift_arithmetic_right,
            ] as [Shift; 3]
            {
                let update = operation(width, width.mask(), 1).flags;
                assert_ne!(update.defined() & Flag::Overflow.mask(), 0);
                assert_eq!(update.undefined() & Flag::Overflow.mask(), 0);
            }
        }
    }

    #[test]
    fn full_width_rotate_still_updates_carry() {
        let original = FlagState::from_bits(0);
        let rotated = Arithmetic::rotate_left(IntegerWidth::Byte, 0x81, 8);
        assert_eq!(rotated.result, 0x81);
        assert!(original.apply(rotated.flags).contains(Flag::Carry));
    }
}

#[derive(Clone, Copy)]
enum Shift {
    Left,
    Right,
    ArithmeticRight,
}
