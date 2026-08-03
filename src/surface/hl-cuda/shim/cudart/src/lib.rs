//! Guest cdylib deployed as `libcudart.so.1` — the CUDA Runtime API drop-in.
//!
//! The exported `cuda*`/`__cuda*` surface is code-generated from `registry/cudart.manifest` (`build.rs`)
//! so it can never drift from the golden 62-entry set. The memory + device + stream basics have real
//! hand-written bodies in [`runtime`] that call the `hl_cuda` lowering services through a process-global
//! [`hl_gpu::RemoteCommandSink`] ([`state`]); the fatbin-registration launch tail are benign default
//! stubs ([`stub`]). The soname `libcudart.so.1` is baked by `build.rs`.

#![allow(non_snake_case)]

pub mod runtime;
pub mod state;
pub mod stub;

/// The CUDA `dim3` launch-geometry type (by-value `{x, y, z}`), referenced by the generated stubs for
/// `cudaLaunchKernel` / `__cudaPushCallConfiguration`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Dim3 {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

// The generated C-ABI export surface: every entry point not hand-written in `runtime`.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported CUDA Runtime API entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = CUDART_ENTRYPOINTS;

#[cfg(test)]
mod tests;
