use crate::CpuState;

pub enum Staged<R, B> {
    Cpu(CpuState),
    Write(CpuState, R, u64, u64, u8),
    Batch(CpuState, B, [u64; 4], u64, u8),
    Sparse(CpuState, [Option<R>; 8], [u64; 8], u8, u64, u8),
    Exit(crate::ExecutionExit),
}
