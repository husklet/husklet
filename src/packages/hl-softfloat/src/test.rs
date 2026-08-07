use crate::{Environment, ExceptionFlags, Format, NaNMode, RoundingMode, TininessMode, Value};

const HALF: Format = Format::Binary16;

fn half(bits: u16) -> Value {
    Value::from_bits(HALF, u64::from(bits))
}

#[test]
fn half_identities() {
    let environment = Environment::default();
    for bits in 1_u16..=u16::MAX {
        let exponent = bits >> 10 & 31;
        let fraction = bits & 0x03ff;
        if exponent == 31 || (exponent == 0 && fraction == 0) {
            continue;
        }
        assert_eq!(environment.add(half(bits), half(0)).value.bits(), u64::from(bits));
        assert_eq!(
            environment.multiply(half(bits), half(0x3c00)).value.bits(),
            u64::from(bits)
        );
    }
}

#[test]
fn ieee_arithmetic() {
    let environment = Environment::default();
    assert_eq!(environment.add(half(0x3c00), half(0x4000)).value, half(0x4200));
    assert_eq!(environment.subtract(half(0x3c00), half(0x3c00)).value, half(0));
    assert_eq!(environment.multiply(half(0x4200), half(0x4400)).value, half(0x4a00));
    assert_eq!(environment.divide(half(0x3c00), half(0x4200)).value, half(0x3555));
    assert_eq!(environment.square_root(half(0x4400)).value, half(0x4000));

    let invalid = environment.multiply(half(0x7c00), half(0));
    assert_eq!(invalid.value, half(0x7e00));
    assert!(invalid.flags.contains(ExceptionFlags::INVALID));
    let divided = environment.divide(half(0x3c00), half(0));
    assert_eq!(divided.value, half(0x7c00));
    assert!(divided.flags.contains(ExceptionFlags::DIVIDE_BY_ZERO));
}

#[test]
fn extended_product() {
    let environment = Environment::default();
    for (left, right, expected) in [
        (0x0000, 0x7c00, 0x4000),
        (0x8000, 0x7c00, 0xc000),
        (0xfc00, 0x0000, 0xc000),
    ] {
        let result = environment.multiply_extended(half(left), half(right));
        assert_eq!(result.value, half(expected));
        assert_eq!(result.flags, ExceptionFlags::default());
    }
    let flushed = Environment {
        flush_inputs: true,
        ..Environment::default()
    }
    .multiply_extended(half(1), half(0x7c00));
    assert_eq!(flushed.value, half(0x4000));
    assert!(flushed.flags.contains(ExceptionFlags::INPUT_DENORMAL));
    let ordinary = environment.multiply_extended(half(0x4200), half(0x4400));
    assert_eq!(ordinary, environment.multiply(half(0x4200), half(0x4400)));
}

#[test]
fn integer_rounding() {
    let cases = [
        (RoundingMode::NearestEven, 0x6800),
        (RoundingMode::NearestAway, 0x6801),
        (RoundingMode::TowardPositive, 0x6801),
        (RoundingMode::TowardNegative, 0x6800),
        (RoundingMode::TowardZero, 0x6800),
    ];
    for (rounding, expected) in cases {
        let environment = Environment {
            rounding,
            ..Environment::default()
        };
        let result = environment.from_unsigned(HALF, 2049);
        assert_eq!(result.value.bits(), expected);
        assert!(result.flags.contains(ExceptionFlags::INEXACT));
    }
}

#[test]
fn nan_controls() {
    let propagate = Environment::default().add(half(0x7d55), half(0x7e22));
    assert_eq!(propagate.value, half(0x7f55));
    assert!(propagate.flags.contains(ExceptionFlags::INVALID));

    let default_nan = Environment {
        nan: NaNMode::Default,
        ..Environment::default()
    }
    .add(half(0x7e22), half(0x3c00));
    assert_eq!(default_nan.value, half(0x7e00));

    let flushed = Environment {
        flush_inputs: true,
        flush_outputs: true,
        tininess: TininessMode::BeforeRounding,
        ..Environment::default()
    }
    .add(half(1), half(0));
    assert_eq!(flushed.value, half(0));
    assert!(flushed.flags.contains(ExceptionFlags::INPUT_DENORMAL));
}

#[test]
fn format_roundtrips() {
    let environment = Environment::default();
    let widened = environment.convert(half(0x3555), Format::Binary32);
    assert_eq!(widened.value.bits(), 0x3eaa_a000);
    assert_eq!(environment.convert(widened.value, HALF).value, half(0x3555));

    for value in [0_i64, 1, -1, 127, -128, 65_504, -65_504] {
        let encoded = environment.from_signed(Format::Binary64, value);
        let decoded = environment.to_signed(encoded.value, 64);
        assert_eq!(decoded.value as i64, value);
        assert_eq!(encoded.flags, ExceptionFlags::default());
        assert_eq!(decoded.flags, ExceptionFlags::default());
    }
}

#[test]
fn rational_reference() {
    for format in [Format::Binary16, Format::Binary32, Format::Binary64] {
        let limit = if format == Format::Binary16 { 31 } else { 4095 };
        IntegerReference::exercise_arithmetic(format, limit);
    }
}

#[test]
fn half_flags() {
    let environment = Environment::default();
    for bits in 0_u16..=u16::MAX {
        if bits >> 10 & 31 != 31 || bits.trailing_zeros() >= 10 {
            continue;
        }
        let result = environment.add(half(bits), half(0x3c00));
        assert_eq!(result.value.bits() >> 10 & 31, 31);
        assert_ne!(result.value.bits() & 0x0200, 0);
        assert_eq!(result.flags.contains(ExceptionFlags::INVALID), bits & 0x0200 == 0);
    }
    assert_eq!(environment.add(half(1), half(1)).value, half(2));
    assert_eq!(environment.add(half(0x03ff), half(1)).value, half(0x0400));
    let underflow = environment.divide(half(1), half(0x4000));
    assert_eq!(underflow.value, half(0));
    assert!(underflow.flags.contains(ExceptionFlags::UNDERFLOW));
    assert!(underflow.flags.contains(ExceptionFlags::INEXACT));
}

#[test]
fn square_root_vectors() {
    let environment = Environment::default();
    for root in 1_i64..=31 {
        let square = root * root;
        let input = Value::from_bits(HALF, exact_integer_bits(HALF, square));
        assert_eq!(
            environment.square_root(input).value.bits(),
            exact_integer_bits(HALF, root)
        );
    }
    assert_eq!(environment.square_root(half(0x4000)).value, half(0x3da8));
    assert_eq!(environment.square_root(half(0x43ff)).value, half(0x3fff));
    assert_eq!(environment.square_root(half(0x4401)).value, half(0x4000));
}

#[test]
fn division_boundaries() {
    for format in [Format::Binary16, Format::Binary32, Format::Binary64] {
        IntegerReference::exercise_division(format);
    }
    let before = Environment {
        tininess: TininessMode::BeforeRounding,
        ..Environment::default()
    }
    .divide(half(1), half(0x4000));
    let after = Environment {
        tininess: TininessMode::AfterRounding,
        ..Environment::default()
    }
    .divide(half(1), half(0x4000));
    assert!(before.flags.contains(ExceptionFlags::UNDERFLOW));
    assert!(after.flags.contains(ExceptionFlags::UNDERFLOW));
}

#[test]
fn fused_cancellation() {
    let environment = Environment::default();
    let fused = environment.fused_multiply_add(half(0x3c01), half(0x3bfe), half(0xbc00));
    assert_eq!(fused.value, half(0x8010));
    let separately = environment.add(environment.multiply(half(0x3c01), half(0x3bfe)).value, half(0xbc00));
    assert_eq!(separately.value, half(0));
    assert_ne!(fused.value, separately.value);

    let invalid = environment.fused_multiply_add(half(0x7c00), half(0), half(0x7e55));
    assert_eq!(invalid.value, half(0x7e00));
    assert!(invalid.flags.contains(ExceptionFlags::INVALID));
}

#[test]
fn fused_tininess() {
    for (rounding, expected) in [
        (RoundingMode::NearestEven, 0x3c00),
        (RoundingMode::NearestAway, 0x3c01),
        (RoundingMode::TowardPositive, 0x3c01),
        (RoundingMode::TowardNegative, 0x3c00),
        (RoundingMode::TowardZero, 0x3c00),
    ] {
        let environment = Environment {
            rounding,
            ..Environment::default()
        };
        let result = environment.fused_multiply_add(half(0x3c00), half(0x3c00), half(0x1000));
        assert_eq!(result.value, half(expected));
        assert!(result.flags.contains(ExceptionFlags::INEXACT));
    }
    for tininess in [TininessMode::BeforeRounding, TininessMode::AfterRounding] {
        let environment = Environment {
            tininess,
            ..Environment::default()
        };
        let result = environment.fused_multiply_add(half(1), half(0x3800), half(0));
        assert_eq!(result.value, half(0));
        assert!(result.flags.contains(ExceptionFlags::UNDERFLOW));
        assert!(result.flags.contains(ExceptionFlags::INEXACT));
    }
}

#[test]
fn selection_edges() {
    let environment = Environment::default();
    assert_eq!(environment.minimum(half(0), half(0x8000)).value, half(0x8000));
    assert_eq!(environment.maximum(half(0), half(0x8000)).value, half(0));
    assert_eq!(
        environment.minimum_number(half(0x7e55), half(0x3c00)).value,
        half(0x3c00)
    );
    assert_eq!(
        environment.compare(half(0xbc00), half(0x3c00), false).value,
        crate::Comparison::Less
    );
    assert_eq!(
        environment.compare(half(0x7e00), half(0), false).value,
        crate::Comparison::Unordered
    );
    assert!(environment.total_order(half(0x8000), half(0)).is_lt());

    let rounded = environment.round_to_integral(half(0x3e00), true);
    assert_eq!(rounded.value, half(0x4000));
    assert!(rounded.flags.contains(ExceptionFlags::INEXACT));
    let quiet = environment.round_to_integral(half(0x3e00), false);
    assert_eq!(quiet.value, half(0x4000));
    assert!(!quiet.flags.contains(ExceptionFlags::INEXACT));
}

struct IntegerReference;

impl IntegerReference {
    fn exercise_arithmetic(format: Format, limit: u32) {
        let environment = Environment::default();
        let mut state = 0x9e37_79b9_u32;
        for _ in 0..2_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let left = i64::from(state % (limit * 2 + 1)) - i64::from(limit);
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let right = i64::from(state % (limit * 2 + 1)) - i64::from(limit);
            let left_value = Value::from_bits(format, exact_integer_bits(format, left));
            let right_value = Value::from_bits(format, exact_integer_bits(format, right));
            assert_eq!(
                environment.add(left_value, right_value).value.bits(),
                exact_integer_bits(format, left + right)
            );
            assert_eq!(
                environment.subtract(left_value, right_value).value.bits(),
                exact_integer_bits(format, left - right)
            );
            Self::assert_product(environment, format, left, right, left_value, right_value);
            Self::assert_fused(environment, format, left, right, left_value, right_value);
        }
    }

    fn assert_product(
        environment: Environment,
        format: Format,
        left: i64,
        right: i64,
        left_value: Value,
        right_value: Value,
    ) {
        let product = left * right;
        if format == Format::Binary16 && !(-1024..=1024).contains(&product) {
            return;
        }
        let expected = if product == 0 && left.is_negative() ^ right.is_negative() {
            1_u64 << (format.width() - 1)
        } else {
            exact_integer_bits(format, product)
        };
        assert_eq!(environment.multiply(left_value, right_value).value.bits(), expected);
    }

    fn exercise_division(format: Format) {
        let environment = Environment::default();
        for numerator in 1_i64..=63 {
            for shift in 0..=5 {
                let denominator = 1_i64 << shift;
                Self::assert_division(environment, format, numerator, denominator);
            }
        }
    }

    fn assert_fused(
        environment: Environment,
        format: Format,
        left: i64,
        right: i64,
        left_value: Value,
        right_value: Value,
    ) {
        let expected = left * right + 1;
        if format == Format::Binary16 && !(-1024..=1024).contains(&expected) {
            return;
        }
        let one = Value::from_bits(format, exact_integer_bits(format, 1));
        assert_eq!(
            environment
                .fused_multiply_add(left_value, right_value, one)
                .value
                .bits(),
            exact_integer_bits(format, expected)
        );
    }

    fn assert_division(environment: Environment, format: Format, numerator: i64, denominator: i64) {
        if numerator % denominator != 0 {
            return;
        }
        let left = Value::from_bits(format, exact_integer_bits(format, numerator));
        let right = Value::from_bits(format, exact_integer_bits(format, denominator));
        assert_eq!(
            environment.divide(left, right).value.bits(),
            exact_integer_bits(format, numerator / denominator)
        );
    }
}

fn exact_integer_bits(format: Format, value: i64) -> u64 {
    if value == 0 {
        return 0;
    }
    let sign = value.is_negative();
    let magnitude = value.unsigned_abs();
    let top = 63 - magnitude.leading_zeros();
    let fraction_bits = match format {
        Format::Binary16 => 10,
        Format::Binary32 => 23,
        Format::Binary64 => 52,
    };
    let bias = match format {
        Format::Binary16 => 15,
        Format::Binary32 => 127,
        Format::Binary64 => 1023,
    };
    assert!(top <= fraction_bits);
    let fraction = (magnitude << (fraction_bits - top)) & ((1_u64 << fraction_bits) - 1);
    let width = format.width();
    u64::from(sign) << (width - 1) | u64::from(top + bias) << fraction_bits | fraction
}
