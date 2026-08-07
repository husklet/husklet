/// Staging outcome. Register and flag effects land in a caller-owned scratch
/// state, so only the deferred memory commit travels in the return value.
pub enum Staged<R, B> {
    Cpu,
    Write(R, u64, u64, u8),
    Batch(B, [u64; 4], u64, u8),
    Sparse(Box<SparseWrites<R>>, u8, u64, u8),
    Exit(crate::ExecutionExit),
}

/// Boxed by `Staged::Sparse` so the rare masked-store variant does not set the
/// size of every staging outcome the interpreter returns.
pub struct SparseWrites<R> {
    pub reservations: [Option<R>; 8],
    pub values: [u64; 8],
}
