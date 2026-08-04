use crate::{Aarch64CpuState, SimdReduceOperation};

pub(crate) fn execute(
    cpu: &Aarch64CpuState,
    operation: SimdReduceOperation,
    lane_bits: u8,
    source: u8,
    wide: bool,
) -> u128 {
    let lanes = if wide { 128 } else { 64 } / lane_bits;
    let mask = if lane_bits == 64 {
        u64::MAX
    } else {
        (1_u64 << lane_bits) - 1
    };
    match operation {
        SimdReduceOperation::AddLong { signed: is_signed } => {
            let result_bits = lane_bits * 2;
            let result_mask = if result_bits == 64 {
                u64::MAX
            } else {
                (1_u64 << result_bits) - 1
            };
            let mut total = 0_u64;
            for lane in 0..lanes {
                let value = cpu.vector_lane(source, lane_bits, lane);
                total = total.wrapping_add(ReduceMath::extend(is_signed, value, lane_bits));
            }
            u128::from(total & result_mask)
        }
        operation => {
            let mut result = cpu.vector_lane(source, lane_bits, 0);
            for lane in 1..lanes {
                let value = cpu.vector_lane(source, lane_bits, lane);
                result = ReduceMath::fold(operation, value, result, lane_bits, mask);
            }
            u128::from(result)
        }
    }
}

struct ReduceMath;

impl ReduceMath {
    fn fold(operation: SimdReduceOperation, value: u64, current: u64, bits: u8, mask: u64) -> u64 {
        match operation {
            SimdReduceOperation::Add => current.wrapping_add(value) & mask,
            SimdReduceOperation::Maximum { unsigned } => Self::maximum(unsigned, value, current, bits),
            SimdReduceOperation::Minimum { unsigned } => Self::minimum(unsigned, value, current, bits),
            SimdReduceOperation::AddLong { .. } => unreachable!(),
        }
    }

    fn signed(value: u64, bits: u8) -> i64 {
        if bits == 64 {
            value as i64
        } else {
            ((value << (64 - bits)) as i64) >> (64 - bits)
        }
    }

    fn extend(signed: bool, value: u64, bits: u8) -> u64 {
        if signed {
            Self::signed(value, bits) as u64
        } else {
            value
        }
    }

    fn maximum(unsigned: bool, value: u64, current: u64, bits: u8) -> u64 {
        let choose = if unsigned {
            value > current
        } else {
            Self::signed(value, bits) > Self::signed(current, bits)
        };
        [current, value][usize::from(choose)]
    }

    fn minimum(unsigned: bool, value: u64, current: u64, bits: u8) -> u64 {
        let choose = if unsigned {
            value < current
        } else {
            Self::signed(value, bits) < Self::signed(current, bits)
        };
        [current, value][usize::from(choose)]
    }
}
