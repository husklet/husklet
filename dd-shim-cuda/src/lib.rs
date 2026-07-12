//! dd-shim-cuda — the guest CUDA Driver API shim, in Rust (increment-1 SCAFFOLD).
//!
//! Builds the single shared object deployed as `libcuda.so.1` (the CUDA Driver API soname). A CUDA
//! app — or `libcudart` — that links `-lcuda` runs unmodified: every `cu*` symbol below is exported
//! with the CUDA Driver API C ABI. The compute path (memory alloc/copy, PTX module load, kernel
//! launch) lowers into a `dd-gpu` IR stream and — through [`dd_shim_common::transport`] — reaches the
//! host executor as the SAME IR the host decodes with the SAME Rust code (no hand-rolled second
//! encoder). This mirrors dd-shim-gl's increment-1 structure exactly.
//!
//! ## Coverage
//! The exported entry-point *surface* is code-generated from a committed manifest (`build.rs` +
//! `registry/`), extracted from dd's clean-room `dd-gpu/cuda/cuda_shim.c` driver-API surface, so it is
//! the full `cu*` set dd ships (132 entry points), not a hand-picked few. Entry points in
//! [`build::IMPLEMENTED`](../build.rs) have real hand-written bodies in [`driver`]; the rest are
//! generated spec-faithful default stubs (correct ABI, `CUDA_SUCCESS` return, `DD_SHIM_DEBUG`-traced)
//! ported to real bodies incrementally — the shrinking long tail.
//!
//! ## What is real vs stub (scaffold)
//! *Real bring-up:* `cuInit`, `cuDriverGetVersion`, `cuDeviceGet*`, `cuCtxCreate/Destroy/…`.
//! *Real IR wiring:* `cuMemAlloc_v2`/`cuMemFree_v2`/`cuMemcpyHtoD_v2`, `cuModuleLoadData`,
//! `cuModuleGetFunction`, `cuLaunchKernel` (→ compute pipeline + dispatch) — all through the shared
//! [`dd_gpu::cuda::CudaContext`] mapping. *Deferred:* `cuMemcpyDtoH_v2` device→host readback and true
//! dispatch execution need a **host PTX/compute backend**, which does not exist yet — that is the
//! future work this scaffold is structured for. See `docs/rendering/SHIM_RUST_ARCHITECTURE.md`.

// The generated + hand-written entry-point surface uses the CUDA C names verbatim (cuInit, …).
#![allow(non_snake_case)]

// The shared IR + transport foundation. Re-exported so this crate's modules (and readers) see that the
// IR type is dd-gpu's, not a local copy.
pub use dd_shim_common as common;

pub mod driver;
pub mod result;
pub mod state;
pub mod stub;

// The generated C-ABI export surface (every `cu*` entry point not in `IMPLEMENTED`).
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported CUDA Driver API entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = CUDA_DRIVER_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_large() {
        // The manifest-driven surface must be the full clean-room `cu*` set, not a hand-picked few.
        assert!(
            CUDA_DRIVER_ENTRYPOINTS >= 120,
            "CUDA driver surface too small: {CUDA_DRIVER_ENTRYPOINTS}"
        );
        // Every entry point is either hand-implemented or a generated stub.
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_COUNT, TOTAL_ENTRYPOINTS);
        // The 26 hand-written bring-up + IR-wiring entry points (build.rs IMPLEMENTED).
        assert_eq!(IMPLEMENTED_COUNT, 26, "hand-written entry-point count drifted from build.rs");
        assert!(GENERATED_STUBS > 0, "expected a generated long tail");
    }

    // Count of hand-written entry points (kept in sync with build.rs IMPLEMENTED via the census).
    const IMPLEMENTED_COUNT: usize = TOTAL_ENTRYPOINTS - GENERATED_STUBS;

    /// Anti-drift round-trip (mirrors dd-shim-gl's `framebuilder_encodes_the_shared_contract`): drive
    /// the REAL exported `cu*` entry points through a full alloc → H2D → PTX-module → launch sequence,
    /// then decode the accumulated IR bytes with the HOST's own `dd_gpu::ir` decoder. Same bytes, same
    /// code path — the guest producer and host executor cannot drift.
    #[test]
    fn launch_path_encodes_the_shared_ir_contract() {
        use core::ffi::c_void;
        use dd_gpu::ir::{decode_stream, Cmd};

        state::reset();

        assert_eq!(driver::cuInit(0), result::CUDA_SUCCESS);
        let mut dev = -1i32;
        assert_eq!(driver::cuDeviceGet(&mut dev, 0), result::CUDA_SUCCESS);
        assert_eq!(dev, 0);
        let mut ctx: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuCtxCreate_v2(&mut ctx, 0, dev), result::CUDA_SUCCESS);
        assert!(!ctx.is_null());

        // cuMemAlloc a, b, c
        let (mut a, mut b, mut c) = (0u64, 0u64, 0u64);
        assert_eq!(driver::cuMemAlloc_v2(&mut a, 1024), result::CUDA_SUCCESS);
        assert_eq!(driver::cuMemAlloc_v2(&mut b, 1024), result::CUDA_SUCCESS);
        assert_eq!(driver::cuMemAlloc_v2(&mut c, 1024), result::CUDA_SUCCESS);
        assert!(a != 0 && b != 0 && c != 0);

        // cuMemcpyHtoD into a
        let host: Vec<u8> = (0..1024u32).map(|i| i as u8).collect();
        assert_eq!(
            driver::cuMemcpyHtoD_v2(a, host.as_ptr() as *const c_void, host.len()),
            result::CUDA_SUCCESS
        );

        // cuModuleLoadData(PTX) + cuModuleGetFunction("vecadd")
        let ptx = std::ffi::CString::new(dd_gpu::ptx::VECADD_PTX).unwrap();
        let mut module: *mut c_void = core::ptr::null_mut();
        assert_eq!(
            driver::cuModuleLoadData(&mut module, ptx.as_ptr() as *const c_void),
            result::CUDA_SUCCESS
        );
        let fname = std::ffi::CString::new("vecadd").unwrap();
        let mut func: *mut c_void = core::ptr::null_mut();
        assert_eq!(
            driver::cuModuleGetFunction(&mut func, module, fname.as_ptr()),
            result::CUDA_SUCCESS
        );
        assert!(!func.is_null());

        // cuLaunchKernel<<<4,256>>>(a, b, c, n) — kernelParams: each slot points at the arg value.
        let mut n: u32 = 256;
        let mut params: [*mut c_void; 4] = [
            &mut a as *mut u64 as *mut c_void,
            &mut b as *mut u64 as *mut c_void,
            &mut c as *mut u64 as *mut c_void,
            &mut n as *mut u32 as *mut c_void,
        ];
        assert_eq!(
            driver::cuLaunchKernel(
                func, 4, 1, 1, 256, 1, 1, 0, core::ptr::null_mut(),
                params.as_mut_ptr(), core::ptr::null_mut(),
            ),
            result::CUDA_SUCCESS
        );

        // Decode the shim-encoded IR with the HOST decoder — the anti-drift gate.
        let bytes = state::with(|s| s.frame.finish());
        let cmds = decode_stream(&bytes).expect("host decoder must accept shim-encoded IR");

        // Structure: three device allocations, one host upload, a kernel shader + compute pipeline, and
        // a compute dispatch — the CUDA compute model expressed in dd-gpu IR.
        let buffers = cmds.iter().filter(|c| matches!(c, Cmd::CreateBuffer(..))).count();
        assert!(buffers >= 3, "expected >=3 CreateBuffer (a,b,c[,params]), got {buffers}");
        assert!(cmds.iter().any(|c| matches!(c, Cmd::WriteBuffer { .. })), "H2D -> WriteBuffer");
        assert!(cmds.iter().any(|c| matches!(c, Cmd::CreateShader { .. })), "kernel -> CreateShader");
        assert!(
            cmds.iter().any(|c| matches!(c, Cmd::CreateComputePipeline(..))),
            "launch -> CreateComputePipeline"
        );
        let dispatched = cmds.iter().any(|c| match c {
            Cmd::Submit(cb) => cb
                .encoder
                .iter()
                .any(|e| matches!(e, dd_gpu::ir::Enc::Dispatch { .. })),
            _ => false,
        });
        assert!(dispatched, "launch -> a compute Dispatch inside a Submit");
    }
}
