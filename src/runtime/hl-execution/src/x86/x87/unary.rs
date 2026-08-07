#![allow(clippy::field_reassign_with_default)]

use crate::x86::real::Conversion;
use crate::{CpuState, ExecutionExit, ExtendedClass, ExtendedReal, FloatWidth};

use super::memory::ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn unary(cpu: &mut CpuState, operation: u8, source: u8, instruction: u64, next: u64) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let mut staged = cpu.clone();
        staged.x87_status &= !(1 << 9);
        if matches!((operation, source), (4, 0 | 1)) && staged.x87_classes[top] == ExtendedClass::Empty {
            if Self::raise_stack(&mut staged, false) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            staged.x87_values[top] = ExtendedReal::INDEFINITE;
            staged.x87_classes[top] = ExtendedClass::QuietNan;
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        match (operation, source) {
            (2, 0) => {}
            (4, 0) => staged.x87_values[top] = ExtendedReal::from_bits(staged.x87_values[top].bits() ^ (1_u128 << 79)),
            (4, 1) => staged.x87_values[top] = ExtendedReal::from_bits(staged.x87_values[top].bits() & !(1_u128 << 79)),
            (4, 5) => {
                let code = match staged.x87_classes[top] {
                    ExtendedClass::Unsupported => 0,
                    ExtendedClass::QuietNan | ExtendedClass::SignalingNan => 1,
                    ExtendedClass::Normal => 2,
                    ExtendedClass::Infinity => 3,
                    ExtendedClass::Zero => 4,
                    ExtendedClass::Empty => 5,
                    ExtendedClass::Denormal => 6,
                };
                staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 8));
                staged.x87_status |=
                    ((code >> 2) as u16) << 14 | (((code >> 1) & 1) as u16) << 10 | ((code & 1) as u16) << 8;
                if staged.x87_values[top].bits() >> 79 != 0 {
                    staged.x87_status |= 1 << 9;
                }
            }
            (6, 6 | 7) => {
                let current = (staged.x87_status >> 11) & 7;
                let adjusted = if source == 6 {
                    current.wrapping_sub(1) & 7
                } else {
                    current.wrapping_add(1) & 7
                };
                staged.x87_status = (staged.x87_status & !0x3800) | adjusted << 11;
            }
            (7, 2) => {
                if staged.x87_classes[top] == ExtendedClass::Empty {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let invalid = staged.x87_classes[top] == ExtendedClass::SignalingNan
                        || (value.is_sign_negative() && value != 0.0);
                    if invalid && Self::raise(&mut staged, 1) {
                        staged.rip = next;
                        *cpu = staged;
                        return ExecutionExit::Continue;
                    }
                    let mut environment = hl_softfloat::Environment::default();
                    environment.rounding = match staged.x87_control >> 10 & 3 {
                        0 => hl_softfloat::RoundingMode::NearestEven,
                        1 => hl_softfloat::RoundingMode::TowardNegative,
                        2 => hl_softfloat::RoundingMode::TowardPositive,
                        _ => hl_softfloat::RoundingMode::TowardZero,
                    };
                    let result = environment.square_root(hl_softfloat::Value::from_bits(
                        hl_softfloat::Format::Binary64,
                        value.to_bits(),
                    ));
                    let mut bits = result.value.bits();
                    let mut flags = crate::x86::scalar::arithmetic::Arithmetic::exceptions(result.flags) as u16;
                    let (narrowed, precision) = Self::precision(bits, staged.x87_control);
                    bits = narrowed;
                    flags |= precision;
                    if flags != 0 {
                        Self::raise(&mut staged, flags);
                    }
                    let (expanded, class) = Self::computed(bits, &[staged.x87_classes[top]]);
                    staged.x87_values[top] = expanded;
                    staged.x87_classes[top] = class;
                }
            }
            (6, 5) | (7, 0) => {
                let other = (top + 1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty || staged.x87_classes[other] == ExtendedClass::Empty
                {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let left = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let right = f64::from_bits(
                        Conversion::narrow(
                            staged.x87_values[other],
                            staged.x87_classes[other],
                            FloatWidth::Double,
                            0,
                        )
                        .bits,
                    );
                    if !left.is_nan() && !right.is_nan() && (left.is_infinite() || right == 0.0) {
                        if Self::raise(&mut staged, 1) {
                            *cpu = staged;
                            return ExecutionExit::UndefinedInstruction { instruction };
                        }
                        staged.x87_values[top] = ExtendedReal::INDEFINITE;
                        staged.x87_classes[top] = ExtendedClass::QuietNan;
                        staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
                    } else {
                        let ieee = operation == 6;
                        let spread = left.abs().log2().floor() - right.abs().log2().floor();
                        let (result, quotient, partial) =
                            if left.is_finite() && right.is_finite() && left != 0.0 && right != 0.0 && spread >= 64.0 {
                                let scaled = right * 8.0;
                                (if scaled.is_infinite() { left } else { left % scaled }, 0_i64, true)
                            } else {
                                let left_magnitude = left.abs();
                                let right_magnitude = right.abs();
                                let scaled = right_magnitude * 8.0;
                                let reduced = if scaled > left_magnitude {
                                    left_magnitude
                                } else {
                                    left_magnitude % scaled
                                };
                                let mut quotient = ((reduced / right_magnitude) as i64).min(7);
                                let truncated = left % right;
                                let result = if ieee {
                                    let half = right_magnitude * 0.5;
                                    #[allow(clippy::float_cmp)]
                                    let tie = truncated.abs() == half;
                                    if truncated.abs() > half || tie && quotient & 1 != 0 {
                                        quotient += 1;
                                        truncated - right_magnitude.copysign(left)
                                    } else {
                                        truncated
                                    }
                                } else {
                                    truncated
                                };
                                (result, quotient & 7, false)
                            };
                        let (value, class) = Conversion::expand(result.to_bits(), FloatWidth::Double);
                        staged.x87_values[top] = value;
                        staged.x87_classes[top] = class;
                        staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
                        if partial {
                            staged.x87_status |= 1 << 10;
                        } else {
                            staged.x87_status |= (((quotient >> 1) & 1) as u16) << 14;
                            staged.x87_status |= ((quotient & 1) as u16) << 9;
                            staged.x87_status |= (((quotient >> 2) & 1) as u16) << 8;
                        }
                    }
                }
            }
            (6, 4) => {
                let destination = top.wrapping_sub(1) & 7;
                let empty = staged.x87_classes[top] == ExtendedClass::Empty;
                if empty || staged.x87_classes[destination] != ExtendedClass::Empty {
                    if Self::raise_stack(&mut staged, !empty) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                    staged.x87_values[destination] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[destination] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let bits = value.to_bits();
                    let exponent = ((bits >> 52 & 0x7ff) as i64 - 1023) as f64;
                    let significand = f64::from_bits((bits & !(0x7ff_u64 << 52)) | (1023_u64 << 52));
                    let (exponent, exponent_class) = Conversion::expand(exponent.to_bits(), FloatWidth::Double);
                    let (significand, significand_class) =
                        Conversion::expand(significand.to_bits(), FloatWidth::Double);
                    staged.x87_values[top] = exponent;
                    staged.x87_classes[top] = exponent_class;
                    staged.x87_values[destination] = significand;
                    staged.x87_classes[destination] = significand_class;
                }
                staged.x87_status = (staged.x87_status & !0x3800) | (destination as u16) << 11;
            }
            (7, 5) => {
                let other = (top + 1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty || staged.x87_classes[other] == ExtendedClass::Empty
                {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let left = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let scale = f64::from_bits(
                        Conversion::narrow(
                            staged.x87_values[other],
                            staged.x87_classes[other],
                            FloatWidth::Double,
                            0,
                        )
                        .bits,
                    )
                    .trunc();
                    let result = Self::scale(left, scale);
                    let (value, class) =
                        Self::computed(result.to_bits(), &[staged.x87_classes[top], staged.x87_classes[other]]);
                    staged.x87_values[top] = value;
                    staged.x87_classes[top] = class;
                }
            }
            (6, 0) | (7, 6 | 7 | 4) => {
                if staged.x87_classes[top] == ExtendedClass::Empty {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let result = match (operation, source) {
                        (6, 0) => value.exp2() - 1.0,
                        (7, 4) => match (staged.x87_control >> 10) & 3 {
                            0 => value.round_ties_even(),
                            1 => value.floor(),
                            2 => value.ceil(),
                            _ => value.trunc(),
                        },
                        (7, 6) => value.sin(),
                        (7, 7) => value.cos(),
                        _ => unreachable!(),
                    };
                    if (operation, source) == (7, 4) && result.abs() > value.abs() {
                        staged.x87_status |= 1 << 9;
                    }
                    let (result, class) = Conversion::expand(result.to_bits(), FloatWidth::Double);
                    staged.x87_values[top] = result;
                    staged.x87_classes[top] = class;
                }
            }
            (6, 1 | 3) | (7, 1) => {
                let other = (top + 1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty || staged.x87_classes[other] == ExtendedClass::Empty
                {
                    Self::raise_stack(&mut staged, false);
                    staged.x87_values[other] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[other] = ExtendedClass::QuietNan;
                } else {
                    let left = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let right = f64::from_bits(
                        Conversion::narrow(
                            staged.x87_values[other],
                            staged.x87_classes[other],
                            FloatWidth::Double,
                            0,
                        )
                        .bits,
                    );
                    let result = match (operation, source) {
                        (6, 1) => right * left.log2(),
                        (6, 3) => right.atan2(left),
                        (7, 1) => right * (left + 1.0).log2(),
                        _ => unreachable!(),
                    };
                    let (result, class) = Conversion::expand(result.to_bits(), FloatWidth::Double);
                    staged.x87_values[other] = result;
                    staged.x87_classes[other] = class;
                }
                staged.x87_classes[top] = ExtendedClass::Empty;
                staged.x87_status = (staged.x87_status & !0x3800) | (other as u16) << 11;
            }
            (6, 2) | (7, 3) => {
                let destination = top.wrapping_sub(1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty
                    || staged.x87_classes[destination] != ExtendedClass::Empty
                {
                    let overflow = staged.x87_classes[destination] != ExtendedClass::Empty;
                    Self::raise_stack(&mut staged, overflow);
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                    staged.x87_values[destination] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[destination] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    if value.abs() >= 2_f64.powi(63) {
                        staged.x87_status |= 1 << 10;
                        staged.rip = next;
                        *cpu = staged;
                        return ExecutionExit::Continue;
                    }
                    let (old_result, pushed) = if operation == 6 {
                        (value.tan(), 1.0)
                    } else {
                        (value.sin(), value.cos())
                    };
                    let (old_result, old_class) = Conversion::expand(old_result.to_bits(), FloatWidth::Double);
                    let (pushed, pushed_class) = Conversion::expand(pushed.to_bits(), FloatWidth::Double);
                    staged.x87_values[top] = old_result;
                    staged.x87_classes[top] = old_class;
                    staged.x87_values[destination] = pushed;
                    staged.x87_classes[destination] = pushed_class;
                }
                staged.x87_status = (staged.x87_status & !0x3800) | (destination as u16) << 11;
            }
            (8, _) => staged.x87_classes[(top + usize::from(source)) & 7] = ExtendedClass::Empty,
            (9, 2) => staged.x87_status &= !0x80ff,
            (9, _) => {}
            _ => return ExecutionExit::UndefinedInstruction { instruction },
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }
}
