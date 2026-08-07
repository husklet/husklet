/// Staging outcome. Register and flag effects land in a caller-owned scratch
/// state, so only the deferred memory commit travels in the return value.
pub enum Staged<R, B> {
    Cpu,
    Write(R, u64, u64, u8),
    Batch(B, [u64; 4], u64, u8),
    Sparse([Option<R>; 8], [u64; 8], u8, u64, u8),
    Exit(crate::ExecutionExit),
}
