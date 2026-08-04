use crate::{CpuState, ExecutionExit, GuestOperandMemory, VectorLane, VectorPackKind, VectorSource};

pub(crate) struct Pack;

impl Pack {
    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        destination: u8,
        source: VectorSource,
        kind: VectorPackKind,
        next: u64,
        instruction: u64,
    ) -> ExecutionExit {
        let right = match VectorLane::read(cpu, memory, source, next, instruction) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let left = cpu.vectors[usize::from(destination)];
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.vectors[usize::from(destination)] = Self::values(left, right, kind);
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn values(left: u128, right: u128, kind: VectorPackKind) -> u128 {
        let source_bits = if matches!(kind, VectorPackKind::SignedWords | VectorPackKind::UnsignedWords) {
            32
        } else {
            16
        };
        let lanes = 128 / source_bits;
        let mut result = 0;
        for (half, value) in [left, right].into_iter().enumerate() {
            for lane in 0..lanes {
                let source = Self::signed(value >> (lane * source_bits), source_bits);
                let output_bits = source_bits / 2;
                result |= Self::saturate(source, kind) << ((half * lanes + lane) * output_bits);
            }
        }
        result
    }

    fn signed(value: u128, bits: usize) -> i64 {
        let shift = 128 - bits;
        ((value << shift) as i128 >> shift) as i64
    }

    fn saturate(value: i64, kind: VectorPackKind) -> u128 {
        let (minimum, maximum, mask) = match kind {
            VectorPackKind::SignedBytes => (-128, 127, 0xff),
            VectorPackKind::UnsignedBytes => (0, 255, 0xff),
            VectorPackKind::SignedWords => (-32_768, 32_767, 0xffff),
            VectorPackKind::UnsignedWords => (0, 65_535, 0xffff),
        };
        (value.clamp(minimum, maximum) as u64 & mask) as u128
    }
}
