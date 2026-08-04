use crate::{Aarch64CpuState, SimdLogic};

pub(crate) fn execute(
    cpu: &Aarch64CpuState,
    destination: u8,
    pattern: u64,
    invert: bool,
    modify: Option<SimdLogic>,
) -> u128 {
    let pattern = if invert { !pattern } else { pattern };
    let pattern = u128::from(pattern) | u128::from(pattern) << 64;
    match modify {
        None => pattern,
        Some(SimdLogic::Orr) => cpu.vector(destination) | pattern,
        Some(SimdLogic::BitClear) => cpu.vector(destination) & !pattern,
        Some(_) => unreachable!("modified immediate only encodes ORR or BIC"),
    }
}
