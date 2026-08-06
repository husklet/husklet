use crate::{Aarch64CpuState, SimdLaneOperation};

pub(crate) fn execute(
    cpu: &Aarch64CpuState,
    operation: SimdLaneOperation,
    lane_bits: u8,
    left: u8,
    right: u8,
    destination: u8,
    wide: bool,
) -> u128 {
    let lanes = if wide { 128 } else { 64 } / lane_bits;
    let mask = if lane_bits == 64 {
        u64::MAX
    } else {
        (1_u64 << lane_bits) - 1
    };
    let pairwise = matches!(
        operation,
        SimdLaneOperation::PairAdd | SimdLaneOperation::PairMaximum { .. } | SimdLaneOperation::PairMinimum { .. }
    );
    let signed = |value: u64| {
        if lane_bits == 64 {
            value as i64
        } else {
            ((value << (64 - lane_bits)) as i64) >> (64 - lane_bits)
        }
    };
    let mut result = 0_u128;
    for lane in 0..lanes {
        let (a, b) = LaneMath::operands(cpu, pairwise, lane, lanes, lane_bits, left, right);
        let signed_a = signed(a);
        let signed_b = signed(b);
        let value = match operation {
            SimdLaneOperation::Multiply => a.wrapping_mul(b),
            SimdLaneOperation::MultiplyAccumulate { subtract } => {
                let base = cpu.vector_lane(destination, lane_bits, lane);
                LaneMath::accumulate(subtract, base, a.wrapping_mul(b))
            }
            SimdLaneOperation::CompareGreater { unsigned } => {
                u64::from(LaneMath::greater(unsigned, a, b, signed_a, signed_b)) * mask
            }
            SimdLaneOperation::CompareGreaterEqual { unsigned } => {
                u64::from(LaneMath::greater_equal(unsigned, a, b, signed_a, signed_b)) * mask
            }
            SimdLaneOperation::CompareEqual => u64::from(a == b) * mask,
            SimdLaneOperation::TestBits => u64::from(a & b != 0) * mask,
            SimdLaneOperation::Maximum { unsigned } => {
                LaneMath::select(LaneMath::greater(unsigned, a, b, signed_a, signed_b), a, b)
            }
            SimdLaneOperation::Minimum { unsigned } => {
                LaneMath::select(LaneMath::less(unsigned, a, b, signed_a, signed_b), a, b)
            }
            SimdLaneOperation::PairAdd => a.wrapping_add(b),
            SimdLaneOperation::PairMaximum { unsigned } => {
                LaneMath::select(LaneMath::greater(unsigned, a, b, signed_a, signed_b), a, b)
            }
            SimdLaneOperation::PairMinimum { unsigned } => {
                LaneMath::select(LaneMath::less(unsigned, a, b, signed_a, signed_b), a, b)
            }
            SimdLaneOperation::HalvingAdd { unsigned, rounding } => {
                if unsigned {
                    ((u128::from(a) + u128::from(b) + u128::from(rounding)) >> 1) as u64
                } else {
                    ((i128::from(signed_a) + i128::from(signed_b) + i128::from(rounding)) >> 1) as u64
                }
            }
            SimdLaneOperation::HalvingSubtract { unsigned } => {
                if unsigned {
                    a.wrapping_sub(b) >> 1
                } else {
                    ((i128::from(signed_a) - i128::from(signed_b)) >> 1) as u64
                }
            }
        } & mask;
        result |= u128::from(value) << (u32::from(lane) * u32::from(lane_bits));
    }
    result
}

pub(crate) fn element(
    cpu: &Aarch64CpuState,
    operation: SimdLaneOperation,
    lane_bits: u8,
    left: u8,
    right: u8,
    index: u8,
    destination: u8,
    wide: bool,
) -> u128 {
    let lanes = if wide { 128 } else { 64 } / lane_bits;
    let mask = (1_u64 << lane_bits) - 1;
    let element = cpu.vector_lane(right, lane_bits, index);
    let mut result = 0_u128;
    for lane in 0..lanes {
        let product = cpu.vector_lane(left, lane_bits, lane).wrapping_mul(element) & mask;
        let value = match operation {
            SimdLaneOperation::Multiply => product,
            SimdLaneOperation::MultiplyAccumulate { subtract } => {
                LaneMath::accumulate(subtract, cpu.vector_lane(destination, lane_bits, lane), product) & mask
            }
            _ => unreachable!(),
        };
        result |= u128::from(value) << (u32::from(lane) * u32::from(lane_bits));
    }
    result
}

struct LaneMath;
impl LaneMath {
    fn accumulate(subtract: bool, base: u64, product: u64) -> u64 {
        if subtract {
            base.wrapping_sub(product)
        } else {
            base.wrapping_add(product)
        }
    }

    fn operands(
        cpu: &Aarch64CpuState,
        pairwise: bool,
        lane: u8,
        lanes: u8,
        lane_bits: u8,
        left: u8,
        right: u8,
    ) -> (u64, u64) {
        if pairwise {
            let second_half = lane >= lanes / 2;
            let source = [left, right][usize::from(second_half)];
            let local = lane - u8::from(second_half) * (lanes / 2);
            (
                cpu.vector_lane(source, lane_bits, local * 2),
                cpu.vector_lane(source, lane_bits, local * 2 + 1),
            )
        } else {
            (
                cpu.vector_lane(left, lane_bits, lane),
                cpu.vector_lane(right, lane_bits, lane),
            )
        }
    }

    fn greater(unsigned: bool, a: u64, b: u64, signed_a: i64, signed_b: i64) -> bool {
        if unsigned { a > b } else { signed_a > signed_b }
    }
    fn greater_equal(unsigned: bool, a: u64, b: u64, signed_a: i64, signed_b: i64) -> bool {
        if unsigned { a >= b } else { signed_a >= signed_b }
    }
    fn less(unsigned: bool, a: u64, b: u64, signed_a: i64, signed_b: i64) -> bool {
        if unsigned { a < b } else { signed_a < signed_b }
    }
    fn select(condition: bool, yes: u64, no: u64) -> u64 {
        [no, yes][usize::from(condition)]
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64Decoder, Aarch64Instruction};

    #[test]
    fn minimum_maximum_exhaustive_encodings() {
        for (opcode, operation, unsigned) in [
            (0x0c_u32, SimdLaneOperation::Maximum { unsigned: false }, false),
            (0x0d, SimdLaneOperation::Minimum { unsigned: false }, false),
            (0x0c, SimdLaneOperation::Maximum { unsigned: true }, true),
            (0x0d, SimdLaneOperation::Minimum { unsigned: true }, true),
        ] {
            for size in 0..3_u32 {
                for wide in [false, true] {
                    for registers in 0..32_768_u32 {
                        let destination = registers & 31;
                        let left = registers >> 5 & 31;
                        let right = registers >> 10 & 31;
                        let word = 0x0e20_0400
                            | u32::from(wide) << 30
                            | u32::from(unsigned) << 29
                            | size << 22
                            | opcode << 11
                            | right << 16
                            | left << 5
                            | destination;
                        assert_eq!(
                            Aarch64Decoder::decode(word).unwrap().instruction,
                            Aarch64Instruction::SimdLane {
                                operation,
                                lane_bits: 8 << size,
                                left: left as u8,
                                right: right as u8,
                                destination: destination as u8,
                                wide,
                            }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn minimum_maximum_signedness_widths_aliases_and_inactive_half() {
        for lane_bits in [8_u8, 16, 32] {
            for wide in [false, true] {
                for unsigned in [false, true] {
                    for maximum in [false, true] {
                        let mask = (1_u64 << lane_bits) - 1;
                        let sign = 1_u64 << (lane_bits - 1);
                        let mut cpu = Aarch64CpuState::default();
                        let lanes = if wide { 128 } else { 64 } / lane_bits;
                        let mut left = 0_u128;
                        let mut right = 0_u128;
                        for lane in 0..lanes {
                            let (a, b) = if lane & 1 == 0 { (sign, 1) } else { (mask, sign - 1) };
                            left |= u128::from(a) << (u32::from(lane) * u32::from(lane_bits));
                            right |= u128::from(b) << (u32::from(lane) * u32::from(lane_bits));
                        }
                        cpu.set_vector(1, left);
                        cpu.set_vector(2, right);
                        cpu.set_vector(3, u128::MAX);
                        let operation = match (maximum, unsigned) {
                            (true, unsigned) => SimdLaneOperation::Maximum { unsigned },
                            (false, unsigned) => SimdLaneOperation::Minimum { unsigned },
                        };
                        let result = execute(&cpu, operation, lane_bits, 1, 2, 3, wide);
                        for lane in 0..lanes {
                            let a = cpu.vector_lane(1, lane_bits, lane);
                            let b = cpu.vector_lane(2, lane_bits, lane);
                            let signed = |value: u64| ((value << (64 - lane_bits)) as i64) >> (64 - lane_bits);
                            let choose_a = if unsigned {
                                if maximum { a > b } else { a < b }
                            } else if maximum {
                                signed(a) > signed(b)
                            } else {
                                signed(a) < signed(b)
                            };
                            assert_eq!(
                                (result >> (u32::from(lane) * u32::from(lane_bits))) as u64 & mask,
                                if choose_a { a } else { b }
                            );
                        }
                        if !wide {
                            assert_eq!(result >> 64, 0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn halving_encodings() {
        for (opcode, unsigned, operation) in [
            (
                0x00_u32,
                false,
                SimdLaneOperation::HalvingAdd {
                    unsigned: false,
                    rounding: false,
                },
            ),
            (
                0x00,
                true,
                SimdLaneOperation::HalvingAdd {
                    unsigned: true,
                    rounding: false,
                },
            ),
            (
                0x02,
                false,
                SimdLaneOperation::HalvingAdd {
                    unsigned: false,
                    rounding: true,
                },
            ),
            (
                0x02,
                true,
                SimdLaneOperation::HalvingAdd {
                    unsigned: true,
                    rounding: true,
                },
            ),
            (0x04, false, SimdLaneOperation::HalvingSubtract { unsigned: false }),
            (0x04, true, SimdLaneOperation::HalvingSubtract { unsigned: true }),
        ] {
            for size in 0..4_u32 {
                for wide in [false, true] {
                    let word = 0x0e20_0400
                        | u32::from(wide) << 30
                        | u32::from(unsigned) << 29
                        | size << 22
                        | opcode << 11
                        | 2 << 16
                        | 1 << 5;
                    if size == 3 && !wide {
                        assert!(Aarch64Decoder::decode(word).is_err());
                    } else {
                        assert_eq!(
                            Aarch64Decoder::decode(word).unwrap().instruction,
                            Aarch64Instruction::SimdLane {
                                operation,
                                lane_bits: 8 << size,
                                left: 1,
                                right: 2,
                                destination: 0,
                                wide,
                            }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn halving_boundaries() {
        for lane_bits in [8_u8, 16, 32, 64] {
            let mask = if lane_bits == 64 {
                u64::MAX
            } else {
                (1_u64 << lane_bits) - 1
            };
            let sign = 1_u64 << (lane_bits - 1);
            let mut cpu = Aarch64CpuState::default();
            cpu.set_vector(1, u128::from(mask) | u128::from(sign) << lane_bits);
            cpu.set_vector(2, u128::from(1_u64) | u128::from(sign - 1) << lane_bits);
            for (operation, first, second) in [
                (
                    SimdLaneOperation::HalvingAdd {
                        unsigned: true,
                        rounding: false,
                    },
                    sign,
                    (mask - 1) >> 1,
                ),
                (
                    SimdLaneOperation::HalvingAdd {
                        unsigned: true,
                        rounding: true,
                    },
                    sign,
                    sign,
                ),
                (
                    SimdLaneOperation::HalvingAdd {
                        unsigned: false,
                        rounding: false,
                    },
                    0,
                    mask,
                ),
                (
                    SimdLaneOperation::HalvingAdd {
                        unsigned: false,
                        rounding: true,
                    },
                    0,
                    0,
                ),
                (SimdLaneOperation::HalvingSubtract { unsigned: true }, (mask - 1) >> 1, 0),
                (SimdLaneOperation::HalvingSubtract { unsigned: false }, mask, sign),
            ] {
                let result = execute(&cpu, operation, lane_bits, 1, 2, 0, true);
                assert_eq!(result as u64 & mask, first & mask, "bits={lane_bits} op={operation:?}");
                assert_eq!(
                    (result >> lane_bits) as u64 & mask,
                    second & mask,
                    "bits={lane_bits} op={operation:?}"
                );
            }
        }
    }
}
