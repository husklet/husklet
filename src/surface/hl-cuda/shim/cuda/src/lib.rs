//! Guest cdylib deployed as `libcuda.so.1` — the CUDA Driver API drop-in.
//!
//! A CUDA app (or `libcudart`) that links `-lcuda` loads these `cu*` symbols as its driver. The exported
//! surface is code-generated from `registry/cuda_driver.manifest` (`build.rs`) so it can never drift from
//! the golden 132-entry `cu*` set. Bring-up + the compute path have real hand-written bodies in
//! [`driver`] that marshal the C ABI and call the `hl_cuda` lowering services through a process-global
//! [`hl_gpu::RemoteCommandSink`] over `$HL_GPU_EXEC` ([`state`]); the long tail are benign, correct-ABI
//! default stubs ([`stub`]) ported to real bodies incrementally without ever changing the surface.
//!
//! The soname `libcuda.so.1` is baked by `build.rs`; the DT_SONAME is what a guest app `DT_NEEDED`s.

// The generated + hand-written entry-point surface uses the CUDA C names verbatim (cuInit, …).
#![allow(non_snake_case)]

pub mod driver;
pub mod state;
pub mod stub;

// The generated C-ABI export surface: every `cu*` entry point not hand-written in `driver`.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported CUDA Driver API entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = CUDA_DRIVER_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_matches_the_census() {
        assert_eq!(
            CUDA_DRIVER_ENTRYPOINTS, 132,
            "CUDA driver surface drifted from the golden 132"
        );
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
    }
}
