//! IEEE 754 binary16 ("half float") encode and decode.
//!
//! Two directions of one conversion, kept in one module so they cannot drift apart. It lives beside
//! [`TextureFormat`](super::enums::TextureFormat) because `Rgba16Float` is what makes it necessary: the
//! clear-colour packing there, the GL texture upload that fills such a plane, and the GL pixel readback
//! that empties one are three callers across two crates, and a half-float encoder written out once per
//! caller is the drift this codebase has already paid for elsewhere.
//!
//! A round trip through both directions must be the identity for every value half can represent, which is
//! what the exhaustive test below pins — and which caught a subnormal decode whose exponent was one too
//! low the first time it ran.

/// IEEE 754 binary16 → binary32. Subnormals and infinities/NaN are carried through rather than flushed,
/// because a half-float value is being read precisely when values outside `0..=1` matter.
pub fn to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exponent = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x3ff) as u32;
    let value = match exponent {
        0 if mantissa == 0 => sign,
        // Subnormal: the value is `mantissa * 2^-24` with no implicit leading one, so binary32 has to be
        // given a normalized form. With `k` the index of the mantissa's highest set bit, the value is
        // `2^(k-24) * (1 + rest/2^k)`, giving a biased exponent of `k + 103` and a mantissa field of
        // `rest << (23 - k)`. Written in terms of `leading_zeros` (`k = 31 - lz`) to avoid a loop.
        0 => {
            let lz = mantissa.leading_zeros();
            sign | ((134 - lz) << 23) | ((mantissa << (lz - 8)) & 0x7f_ffff)
        }
        0x1f => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 127 - 15) << 23) | (mantissa << 13),
    };
    f32::from_bits(value)
}

/// Shift `value` right by `shift`, rounding the discarded bits to NEAREST, ties to EVEN.
///
/// This was `(value >> shift) + ((value >> (shift - 1)) & 1)`, which rounds a tie AWAY FROM ZERO — while
/// the doc comment above claimed nearest-even and the hardware does nearest-even. A comment asserting the
/// rule the code does not implement is worse than no comment, because it is what the next reader checks
/// instead of the arithmetic; this one survived a 65536-case exhaustive round trip, which cannot see the
/// difference at all, since every value that round trip produces is exactly representable and never ties.
fn round_to_even(value: u32, shift: u32) -> u32 {
    let truncated = value >> shift;
    let round_bit = (value >> (shift - 1)) & 1;
    let sticky = value & ((1 << (shift - 1)) - 1);
    // Round up only when past the halfway point, or exactly at it with an odd result to make even.
    truncated + u32::from(round_bit == 1 && (sticky != 0 || truncated & 1 == 1))
}

/// IEEE 754 binary32 → binary16, rounding to nearest, ties to even. A magnitude above half's maximum saturates to
/// infinity and one below its smallest subnormal flushes to zero, which is what the range of the format
/// permits; NaN stays NaN rather than becoming an infinity.
pub fn from_f32(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x7f_ffff;
    if exponent == 0xff {
        // Infinity, or a NaN whose payload must not round away to infinity.
        return sign | 0x7c00 | if mantissa == 0 { 0 } else { 0x200 };
    }
    let unbiased = exponent - 127 + 15;
    if unbiased >= 0x1f {
        return sign | 0x7c00;
    }
    if unbiased <= 0 {
        // Subnormal half, or an underflow to zero.
        if unbiased < -10 {
            return sign;
        }
        let full = mantissa | 0x80_0000;
        let shift = (14 - unbiased) as u32;
        return sign | (round_to_even(full, shift) as u16);
    }
    // Rounding the mantissa up may carry into the exponent, which the addition below already handles;
    // a carry past the largest finite exponent lands on the infinity encoding, which is correct.
    sign | (((unbiased as u32) << 10) + round_to_even(mantissa, 13)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bit pattern half can hold decodes to a float that re-encodes to the same pattern. This is
    /// exhaustive because the domain is only 65536 values, so there is no sampling to argue about — and
    /// it is the assertion that keeps the two directions inverse as either one is edited.
    #[test]
    fn every_half_bit_pattern_round_trips() {
        for bits in 0u16..=0xffff {
            let value = to_f32(bits);
            if value.is_nan() {
                assert!(
                    from_f32(value).is_nan_half(),
                    "{bits:#06x} decoded to NaN and must re-encode to some NaN"
                );
                continue;
            }
            assert_eq!(from_f32(value), bits, "{bits:#06x} must round-trip");
        }
    }

    trait NanHalf {
        fn is_nan_half(self) -> bool;
    }

    impl NanHalf for u16 {
        fn is_nan_half(self) -> bool {
            self & 0x7c00 == 0x7c00 && self & 0x03ff != 0
        }
    }

    /// A tie rounds to EVEN, not away from zero.
    ///
    /// The exhaustive round trip above has no power here and neither did anything else: every value it
    /// produces is exactly representable in half, so no tie ever occurs and both rounding rules agree on
    /// all 65536 of them. The encoder rounded ties away from zero for its whole life while its doc comment
    /// said nearest-even, and the first thing that noticed was comparing float clears against real
    /// hardware. A halfway value is `n + 0.5` ULP: with an 11-bit significand at exponent 0, one ULP is
    /// 2^-10, so 1.0 + 2^-11 sits exactly between 1.0 and the next half, and must round DOWN to the even
    /// significand rather than up.
    #[test]
    fn a_tie_rounds_to_the_even_significand() {
        let ulp = 2.0f32.powi(-10);
        // 1.0 is even (significand 0x000); 1.0 + half an ulp ties and must stay at 1.0.
        assert_eq!(from_f32(1.0 + ulp / 2.0), 0x3c00, "tie down to the even significand");
        // 1.0 + 1 ulp is odd (significand 0x001); + half an ulp ties and must round UP to even 0x002.
        assert_eq!(
            from_f32(1.0 + ulp + ulp / 2.0),
            0x3c02,
            "tie up when rounding down would leave an odd significand"
        );
        // Just past the halfway point always rounds up regardless of parity.
        assert!(
            from_f32(1.0 + ulp * 0.51) > 0x3c00,
            "past the tie rounds up even from an even significand"
        );
        // Symmetric for negatives: ties go to even, not away from zero.
        assert_eq!(from_f32(-(1.0 + ulp / 2.0)), 0xbc00, "negative tie also goes to even");
    }

    /// The endpoints, named. A conversion that divides or shifts by the wrong constant is invisible in
    /// the middle of the range and obvious here.
    #[test]
    fn the_endpoints_encode_where_the_format_says() {
        assert_eq!(from_f32(0.0), 0x0000);
        assert_eq!(from_f32(1.0), 0x3c00);
        assert_eq!(from_f32(-1.0), 0xbc00);
        assert_eq!(from_f32(65504.0), 0x7bff, "half's largest finite value");
        assert_eq!(from_f32(65520.0), 0x7c00, "just above it saturates to infinity");
        assert_eq!(from_f32(6.103_515_6e-5), 0x0400, "smallest normal");
        assert_eq!(from_f32(5.960_464_5e-8), 0x0001, "smallest subnormal");
        assert_eq!(from_f32(2.9e-8), 0x0000, "below it flushes to zero");
        assert_eq!(to_f32(0x7bff), 65504.0);
        assert_eq!(to_f32(0x0001), 5.960_464_5e-8, "smallest subnormal decodes");
        assert!(to_f32(0x7c00).is_infinite());
        assert!(to_f32(0xfc00).is_infinite() && to_f32(0xfc00).is_sign_negative());
    }
}
