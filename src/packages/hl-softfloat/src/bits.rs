use crate::{Operand, RoundingMode};

pub(crate) struct BitArithmetic;

impl BitArithmetic {
    pub(crate) fn shift_right_jam(value: u128, distance: u32) -> u128 {
        if distance == 0 {
            value
        } else if distance < 128 {
            (value >> distance) | u128::from(value << (128 - distance) != 0)
        } else {
            u128::from(value != 0)
        }
    }

    pub(crate) fn should_increment(mode: RoundingMode, sign: bool, discarded: u8, odd: bool) -> bool {
        match mode {
            RoundingMode::NearestEven => discarded > 4 || (discarded == 4 && odd),
            RoundingMode::NearestAway => discarded >= 4,
            RoundingMode::TowardPositive => !sign && discarded != 0,
            RoundingMode::TowardNegative => sign && discarded != 0,
            RoundingMode::TowardZero => false,
        }
    }

    pub(crate) fn larger(left: Operand, right: Operand) -> (Operand, Operand) {
        if (left.exponent, left.significand) >= (right.exponent, right.significand) {
            (left, right)
        } else {
            (right, left)
        }
    }

    pub(crate) fn integer_square_root(value: u128) -> (u128, u128) {
        let mut remainder = 0_u128;
        let mut root = 0_u128;
        for pair in (0..64).rev() {
            remainder = (remainder << 2) | (value >> (pair * 2) & 3);
            let trial = (root << 2) | 1;
            root <<= 1;
            if remainder >= trial {
                remainder -= trial;
                root |= 1;
            }
        }
        (root, remainder)
    }

    pub(crate) fn normalize(mut significand: u128, mut exponent: i32, fraction_bits: u8) -> (u128, i32) {
        while significand >> fraction_bits == 0 {
            significand <<= 1;
            exponent -= 1;
        }
        (significand, exponent)
    }
}
