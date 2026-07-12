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
//! ## What is real (functional)
//! *Real bring-up:* `cuInit`, `cuDriverGetVersion`, `cuDeviceGet*`, `cuCtxCreate/Destroy/…`.
//! *Real IR wiring + EXECUTION:* `cuMemAlloc_v2`/`cuMemFree_v2`/`cuMemcpyHtoD_v2`, `cuModuleLoadData`,
//! `cuModuleGetFunction`, `cuLaunchKernel` (→ compute pipeline + dispatch), `cuMemcpyDtoH_v2` (real
//! device→host readback), and `cuStream*`/`cuEvent*` synchronization — all through the shared
//! [`dd_gpu::cuda::CudaContext`] mapping. The accumulated IR is EXECUTED in-process on an embedded
//! [`dd_gpu::software::SoftwareBackend`] (the CPU PTX interpreter — the same executor `dd-gpu`'s oracle
//! and `dd-gpu/cuda/cuda_shim.c` use), so a real vector-add PTX kernel runs end-to-end and reads back
//! numerically correct results with NO GPU. On a real Apple-silicon host the same IR is shipped over
//! `$DD_GPU_EXEC` to the host Metal executor instead. See `docs/rendering/SHIM_RUST_ARCHITECTURE.md`.

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

    /// The `cu*` entry points share one process-global [`state`] (the CUDA single-device model), so the
    /// tests that drive them must not run concurrently — cargo runs tests in parallel by default. Each
    /// such test holds this guard for its whole body (and `state::reset()`s under it) so their frame /
    /// handle-table mutations never interleave.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn surface_is_complete_and_large() {
        // The manifest-driven surface must be the full clean-room `cu*` set, not a hand-picked few.
        assert!(
            CUDA_DRIVER_ENTRYPOINTS >= 120,
            "CUDA driver surface too small: {CUDA_DRIVER_ENTRYPOINTS}"
        );
        // Every entry point is either hand-implemented or a generated stub.
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_COUNT, TOTAL_ENTRYPOINTS);
        // The whole 132-entry surface now has real hand-written bodies at parity with the C oracle —
        // the long tail is fully ported, so there are no generated stubs left.
        assert_eq!(IMPLEMENTED_COUNT, 132, "hand-written entry-point count drifted from build.rs");
        assert_eq!(GENERATED_STUBS, 0, "the generated long tail should be fully ported to real bodies");
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

        let _serial = serial();
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

    /// THE FUNCTIONAL MILESTONE: drive the REAL exported `cu*` entry points through a full vector-add —
    /// alloc → H2D → module load → get function → launch → **DtoH** — and assert the read-back output is
    /// arithmetically correct (`c[i] == a[i] + b[i]`). This proves the whole guest path executes
    /// end-to-end: the shim lowers CUDA to the shared dd-gpu IR, the IR runs the PTX kernel on the
    /// embedded software backend (CPU interpreter), and `cuMemcpyDtoH_v2` returns the real results — the
    /// same numbers `dd-gpu/cuda/cuda_shim.c` (the parity oracle) produces. No GPU, no host process.
    #[test]
    fn vecadd_executes_end_to_end_through_the_shim() {
        use core::ffi::c_void;

        let _serial = serial();
        state::reset();
        let n = 1024usize;

        assert_eq!(driver::cuInit(0), result::CUDA_SUCCESS);
        let mut dev = -1i32;
        assert_eq!(driver::cuDeviceGet(&mut dev, 0), result::CUDA_SUCCESS);
        let mut ctx: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuCtxCreate_v2(&mut ctx, 0, dev), result::CUDA_SUCCESS);

        // cuMemAlloc a, b, c
        let (mut a, mut b, mut c) = (0u64, 0u64, 0u64);
        let sz = (n * 4) as usize;
        assert_eq!(driver::cuMemAlloc_v2(&mut a, sz), result::CUDA_SUCCESS);
        assert_eq!(driver::cuMemAlloc_v2(&mut b, sz), result::CUDA_SUCCESS);
        assert_eq!(driver::cuMemAlloc_v2(&mut c, sz), result::CUDA_SUCCESS);

        // host inputs → cuMemcpyHtoD into a, b
        let ha: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let hb: Vec<f32> = (0..n).map(|i| (n - i) as f32 * 0.25).collect();
        let to_bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let (ba, bb) = (to_bytes(&ha), to_bytes(&hb));
        assert_eq!(
            driver::cuMemcpyHtoD_v2(a, ba.as_ptr() as *const c_void, ba.len()),
            result::CUDA_SUCCESS
        );
        assert_eq!(
            driver::cuMemcpyHtoD_v2(b, bb.as_ptr() as *const c_void, bb.len()),
            result::CUDA_SUCCESS
        );

        // cuModuleLoadData(vecadd PTX) + cuModuleGetFunction("vecadd")
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

        // cuLaunchKernel<<<ceil(n/256), 256>>>(a, b, c, n) — each param slot points at the arg value.
        let mut nn: u32 = n as u32;
        let mut params: [*mut c_void; 4] = [
            &mut a as *mut u64 as *mut c_void,
            &mut b as *mut u64 as *mut c_void,
            &mut c as *mut u64 as *mut c_void,
            &mut nn as *mut u32 as *mut c_void,
        ];
        let grid = (n as u32).div_ceil(256);
        assert_eq!(
            driver::cuLaunchKernel(
                func, grid, 1, 1, 256, 1, 1, 0, core::ptr::null_mut(),
                params.as_mut_ptr(), core::ptr::null_mut(),
            ),
            result::CUDA_SUCCESS
        );

        // cuCtxSynchronize then cuMemcpyDtoH(c) → assert c[i] == a[i] + b[i].
        assert_eq!(driver::cuCtxSynchronize(), result::CUDA_SUCCESS);
        let mut out = vec![0u8; sz];
        assert_eq!(
            driver::cuMemcpyDtoH_v2(out.as_mut_ptr() as *mut c_void, c, sz),
            result::CUDA_SUCCESS
        );
        for i in 0..n {
            let got = f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(got, ha[i] + hb[i], "vecadd result mismatch at c[{i}] (shim end-to-end)");
        }

        // A readback from a dangling device pointer is a clean CUDA_ERROR_INVALID_VALUE, not UB.
        let mut junk = [0u8; 4];
        assert_eq!(
            driver::cuMemcpyDtoH_v2(junk.as_mut_ptr() as *mut c_void, 0xdead_0000, 4),
            result::CUDA_ERROR_INVALID_VALUE
        );
    }

    /// FUNCTIONAL: `cuMemsetD32` fills a device buffer, `cuMemcpyDtoD` copies it to another, and both
    /// read back correctly through `cuMemcpyDtoH` — the fill/copy families execute end-to-end on the
    /// embedded backend, and `cuMemGetInfo` reports sane VRAM. Drives only the exported `cu*` API.
    #[test]
    fn memset_dtod_and_meminfo_through_the_shim() {
        use core::ffi::c_void;
        let _serial = serial();
        state::reset();
        let n = 256usize;
        let sz = n * 4;

        assert_eq!(driver::cuInit(0), result::CUDA_SUCCESS);
        let mut ctx: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuCtxCreate_v2(&mut ctx, 0, 0), result::CUDA_SUCCESS);

        let (mut a, mut b) = (0u64, 0u64);
        assert_eq!(driver::cuMemAlloc_v2(&mut a, sz), result::CUDA_SUCCESS);
        assert_eq!(driver::cuMemAlloc_v2(&mut b, sz), result::CUDA_SUCCESS);

        // cuMemsetD32(a, 0xDEADBEEF, n) then DtoH -> every word is the fill value.
        assert_eq!(driver::cuMemsetD32_v2(a, 0xDEAD_BEEF, n), result::CUDA_SUCCESS);
        let mut out = vec![0u8; sz];
        assert_eq!(
            driver::cuMemcpyDtoH_v2(out.as_mut_ptr() as *mut c_void, a, sz),
            result::CUDA_SUCCESS
        );
        for i in 0..n {
            let w = u32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(w, 0xDEAD_BEEF, "cuMemsetD32 word {i}");
        }

        // cuMemcpyDtoD(b <- a) then DtoH b -> same fill.
        assert_eq!(driver::cuMemcpyDtoD_v2(b, a, sz), result::CUDA_SUCCESS);
        let mut out2 = vec![0u8; sz];
        assert_eq!(
            driver::cuMemcpyDtoH_v2(out2.as_mut_ptr() as *mut c_void, b, sz),
            result::CUDA_SUCCESS
        );
        assert_eq!(out, out2, "cuMemcpyDtoD must reproduce the source bytes");

        // cuMemsetD8(a, 0x00, n*4) clears it.
        assert_eq!(driver::cuMemsetD8_v2(a, 0, sz), result::CUDA_SUCCESS);
        let mut cleared = vec![0xFFu8; sz];
        assert_eq!(
            driver::cuMemcpyDtoH_v2(cleared.as_mut_ptr() as *mut c_void, a, sz),
            result::CUDA_SUCCESS
        );
        assert!(cleared.iter().all(|&x| x == 0), "cuMemsetD8(0) must zero the buffer");

        // cuMemGetInfo: total is the advertised VRAM (8 GiB default), free < total after allocations.
        let (mut free, mut total) = (0usize, 0usize);
        assert_eq!(driver::cuMemGetInfo_v2(&mut free, &mut total), result::CUDA_SUCCESS);
        assert_eq!(total, 8usize << 30);
        assert!(free < total && free == total - 2 * sz, "free should reflect outstanding bytes");

        // cuMemGetAddressRange resolves the base+size of an interior pointer.
        let (mut base, mut range) = (0u64, 0usize);
        assert_eq!(
            driver::cuMemGetAddressRange_v2(&mut base, &mut range, a + 16),
            result::CUDA_SUCCESS
        );
        assert_eq!(base, a);
        assert_eq!(range, sz);

        // Dangling memset / DtoD pointers are clean CUDA_ERROR_INVALID_VALUE.
        assert_eq!(driver::cuMemsetD32_v2(0xdead_0000, 0, 1), result::CUDA_ERROR_INVALID_VALUE);
        assert_eq!(
            driver::cuMemcpyDtoD_v2(0xdead_0000, a, 4),
            result::CUDA_ERROR_INVALID_VALUE
        );
    }

    /// Context management parity: push/pop stack, api version, limits (get/set + out-of-range),
    /// and primary-context ref-counting — mirrors `dd-gpu/cuda/cuda_shim.c`.
    #[test]
    fn context_management_matches_the_oracle() {
        use core::ffi::c_void;
        let _serial = serial();
        state::reset();
        assert_eq!(driver::cuInit(0), result::CUDA_SUCCESS);

        let (mut c1, mut c2): (*mut c_void, *mut c_void) =
            (core::ptr::null_mut(), core::ptr::null_mut());
        assert_eq!(driver::cuCtxCreate_v2(&mut c1, 0, 0), result::CUDA_SUCCESS);
        assert_eq!(driver::cuCtxCreate_v2(&mut c2, 0, 0), result::CUDA_SUCCESS);
        // c2 is current after the second create; push c1 makes it current, pop restores c2.
        assert_eq!(driver::cuCtxPushCurrent_v2(c1), result::CUDA_SUCCESS);
        let mut cur: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuCtxGetCurrent(&mut cur), result::CUDA_SUCCESS);
        assert_eq!(cur, c1);
        let mut popped: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuCtxPopCurrent_v2(&mut popped), result::CUDA_SUCCESS);
        assert_eq!(popped, c1);
        assert_eq!(driver::cuCtxGetCurrent(&mut cur), result::CUDA_SUCCESS);
        assert_eq!(cur, c2);

        // api version == 3020.
        let mut ver = 0u32;
        assert_eq!(driver::cuCtxGetApiVersion(c2, &mut ver), result::CUDA_SUCCESS);
        assert_eq!(ver, 3020);

        // limits: default stack size 1024, set/get roundtrip, out-of-range -> UNSUPPORTED_LIMIT.
        let mut lim = 0usize;
        assert_eq!(driver::cuCtxGetLimit(&mut lim, 0), result::CUDA_SUCCESS); // CU_LIMIT_STACK_SIZE
        assert_eq!(lim, 1024);
        assert_eq!(driver::cuCtxSetLimit(0, 4096), result::CUDA_SUCCESS);
        assert_eq!(driver::cuCtxGetLimit(&mut lim, 0), result::CUDA_SUCCESS);
        assert_eq!(lim, 4096);
        assert_eq!(driver::cuCtxGetLimit(&mut lim, 99), result::CUDA_ERROR_UNSUPPORTED_LIMIT);

        // flags roundtrip on the current context.
        assert_eq!(driver::cuCtxSetFlags(0x5), result::CUDA_SUCCESS);
        let mut fl = 0u32;
        assert_eq!(driver::cuCtxGetFlags(&mut fl), result::CUDA_SUCCESS);
        assert_eq!(fl, 0x5);

        // peer access on a single device is the spec-correct error, not a fake success.
        assert_eq!(driver::cuCtxEnablePeerAccess(c1, 0), result::CUDA_ERROR_PEER_ACCESS_UNSUPPORTED);
        assert_eq!(driver::cuCtxDisablePeerAccess(c1), result::CUDA_ERROR_PEER_ACCESS_NOT_ENABLED);

        // primary context ref-counting.
        let (mut p1, mut p2): (*mut c_void, *mut c_void) =
            (core::ptr::null_mut(), core::ptr::null_mut());
        assert_eq!(driver::cuDevicePrimaryCtxRetain(&mut p1, 0), result::CUDA_SUCCESS);
        assert_eq!(driver::cuDevicePrimaryCtxRetain(&mut p2, 0), result::CUDA_SUCCESS);
        assert_eq!(p1, p2, "primary context is a singleton");
        let (mut pflags, mut active) = (0u32, 0i32);
        assert_eq!(
            driver::cuDevicePrimaryCtxGetState(0, &mut pflags, &mut active),
            result::CUDA_SUCCESS
        );
        assert_eq!(active, 1);
        assert_eq!(driver::cuDevicePrimaryCtxRelease_v2(0), result::CUDA_SUCCESS);
        assert_eq!(driver::cuDevicePrimaryCtxRelease_v2(0), result::CUDA_SUCCESS);
        assert_eq!(
            driver::cuDevicePrimaryCtxGetState(0, &mut pflags, &mut active),
            result::CUDA_SUCCESS
        );
        assert_eq!(active, 0, "refcount back to zero deactivates the primary context");
    }

    /// Function + occupancy + event queries: after resolving `vecadd`, `cuFuncGetName`/`GetModule`/
    /// `GetAttribute` answer, occupancy is computed from the SM limits, and `cuEventElapsedTime`
    /// returns a non-negative duration between two recorded events.
    #[test]
    fn function_occupancy_and_event_queries() {
        use core::ffi::c_void;
        let _serial = serial();
        state::reset();
        assert_eq!(driver::cuInit(0), result::CUDA_SUCCESS);
        let mut ctx: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuCtxCreate_v2(&mut ctx, 0, 0), result::CUDA_SUCCESS);

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

        // cuFuncGetName returns the interned entry name.
        let mut namep: *const core::ffi::c_char = core::ptr::null();
        assert_eq!(driver::cuFuncGetName(&mut namep, func), result::CUDA_SUCCESS);
        let got = unsafe { std::ffi::CStr::from_ptr(namep) }.to_str().unwrap();
        assert_eq!(got, "vecadd");

        // cuFuncGetModule returns the owning module handle.
        let mut m2: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuFuncGetModule(&mut m2, func), result::CUDA_SUCCESS);
        assert_eq!(m2, module);

        // cuFuncGetAttribute: NUM_REGS == 32; SetAttribute(MAX_DYNAMIC_SHARED) then GetAttribute reads back.
        let mut regs = 0i32;
        assert_eq!(
            driver::cuFuncGetAttribute(&mut regs, result::CU_FUNC_ATTRIBUTE_NUM_REGS, func),
            result::CUDA_SUCCESS
        );
        assert_eq!(regs, 32);
        assert_eq!(
            driver::cuFuncSetAttribute(
                func,
                result::CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES,
                8192
            ),
            result::CUDA_SUCCESS
        );
        let mut dynsh = 0i32;
        assert_eq!(
            driver::cuFuncGetAttribute(
                &mut dynsh,
                result::CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES,
                func
            ),
            result::CUDA_SUCCESS
        );
        assert_eq!(dynsh, 8192);

        // occupancy: 2048 / 256 = 8 resident blocks.
        let mut blocks = 0i32;
        assert_eq!(
            driver::cuOccupancyMaxActiveBlocksPerMultiprocessor(&mut blocks, func, 256, 0),
            result::CUDA_SUCCESS
        );
        assert_eq!(blocks, 8);

        // events: record two, elapsed time is finite and non-negative.
        let (mut e1, mut e2): (*mut c_void, *mut c_void) =
            (core::ptr::null_mut(), core::ptr::null_mut());
        assert_eq!(driver::cuEventCreate(&mut e1, 0), result::CUDA_SUCCESS);
        assert_eq!(driver::cuEventCreate(&mut e2, 0), result::CUDA_SUCCESS);
        assert_eq!(driver::cuEventQuery(e1), result::CUDA_ERROR_NOT_READY); // unrecorded
        assert_eq!(driver::cuEventRecord(e1, core::ptr::null_mut()), result::CUDA_SUCCESS);
        assert_eq!(driver::cuEventRecord(e2, core::ptr::null_mut()), result::CUDA_SUCCESS);
        assert_eq!(driver::cuEventQuery(e1), result::CUDA_SUCCESS); // recorded -> ready
        let mut ms = -1.0f32;
        assert_eq!(driver::cuEventElapsedTime(&mut ms, e1, e2), result::CUDA_SUCCESS);
        assert!(ms >= 0.0 && ms.is_finite(), "elapsed time must be a finite non-negative ms: {ms}");
    }

    /// Anti-drift: the new IR-emitting fill/copy entry points (`cuMemsetD32`, `cuMemcpyDtoD`) encode
    /// with the SAME shared contract the host decodes — decode the accumulated bytes with the host's
    /// own `dd_gpu::ir` decoder and confirm the expected `WriteBuffer`s appear.
    #[test]
    fn memset_dtod_encode_the_shared_ir_contract() {
        use core::ffi::c_void;
        use dd_gpu::ir::{decode_stream, Cmd};
        let _serial = serial();
        state::reset();
        assert_eq!(driver::cuInit(0), result::CUDA_SUCCESS);
        let mut ctx: *mut c_void = core::ptr::null_mut();
        assert_eq!(driver::cuCtxCreate_v2(&mut ctx, 0, 0), result::CUDA_SUCCESS);
        let (mut a, mut b) = (0u64, 0u64);
        assert_eq!(driver::cuMemAlloc_v2(&mut a, 64), result::CUDA_SUCCESS);
        assert_eq!(driver::cuMemAlloc_v2(&mut b, 64), result::CUDA_SUCCESS);
        assert_eq!(driver::cuMemsetD32_v2(a, 7, 16), result::CUDA_SUCCESS);

        let bytes = state::with(|s| s.frame.finish());
        let cmds = decode_stream(&bytes).expect("host decoder must accept shim-encoded memset IR");
        // Two CreateBuffer (a,b) and a WriteBuffer (the memset fill).
        assert!(cmds.iter().filter(|c| matches!(c, Cmd::CreateBuffer(..))).count() >= 2);
        let write_fills = cmds.iter().any(|c| matches!(c, Cmd::WriteBuffer { data, .. } if data.len() == 64));
        assert!(write_fills, "cuMemsetD32 must emit a WriteBuffer of the expanded fill");
    }
}
