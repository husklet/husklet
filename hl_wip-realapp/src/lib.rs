//! `hl_wip-realapp` — the crate carries no library code; the proof lives entirely in the integration
//! test `tests/vecadd.rs`, which loads the REAL staged `libcuda.so.1` and runs a CUDA vecadd against a
//! host `CpuExecutor` served over a unix socket. See that test for the full end-to-end flow.
