use crate::FpFormat;

/// Architectural `VFPExpandImm` operation shared by scalar and `AdvSIMD` FMOV.
pub(crate) struct ImmediateEncoding;

impl ImmediateEncoding {
    pub(crate) fn expand(format: FpFormat, immediate: u8) -> u64 {
        let sign = u64::from(immediate >> 7);
        let exponent = u64::from(immediate >> 4 & 7);
        let fraction = u64::from(immediate & 15);
        match format {
            FpFormat::Half => sign << 15 | Self::exponent(exponent, 5) << 10 | fraction << 6,
            FpFormat::Single => sign << 31 | Self::exponent(exponent, 8) << 23 | fraction << 19,
            FpFormat::Double => sign << 63 | Self::exponent(exponent, 11) << 52 | fraction << 48,
        }
    }

    pub(crate) fn splat(format: FpFormat, immediate: u8) -> u64 {
        let value = Self::expand(format, immediate);
        match format {
            FpFormat::Half => value * 0x0001_0001_0001_0001,
            FpFormat::Single => value << 32 | value,
            FpFormat::Double => value,
        }
    }

    fn exponent(encoded: u64, bits: u8) -> u64 {
        let high = u64::from(encoded & 4 == 0);
        let repeated = if encoded & 4 == 0 { 0 } else { (1_u64 << (bits - 3)) - 1 };
        high << (bits - 1) | repeated << 2 | encoded & 3
    }
}
