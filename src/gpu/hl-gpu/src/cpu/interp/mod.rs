//! The neutral kernel-IR **interpreter** — runs a [`KernelProgram`] over a launch grid on the CPU with
//! no GPU. This is the compute half of the reference `CpuExecutor` oracle: a compiled kernel (produced by
//! a driver's PTX/GLSL/SPIR-V front-end elsewhere) is executed here per-thread over host memory.
//!
//! Ported from the interpreter tail of `hl-gpu/src/ptx.rs` (`Val` / `execute` / `run_block` / `run_until`
//! / `shared_addr` / `atomic_rmw` / `read_scalar` / `write_scalar`) — NOT the PTX text front-end, which is
//! a driver-adapter concern (`hl-cuda`) and does not live in this crate. Layering: [`control`] owns the
//! grid/CTA driver + `bar.sync` rendezvous, [`exec`] owns the per-thread instruction step, [`memory`] owns
//! the raw byte/pointer/atomic primitives.
//!
//! [`KernelProgram`]: crate::protocol::model::kernel::KernelProgram

mod control;
mod exec;
mod memory;

pub use control::execute;
