//! dd-shim-cudart — the guest CUDA Runtime API shim, in Rust.
//!
//! Builds the single shared object deployed as `libcudart.so.1` (the CUDA Runtime API soname). A CUDA
//! app or framework that links `-lcudart` runs unmodified: every `cuda*`/`__cuda*` symbol below is
//! exported with the CUDA Runtime API C ABI. cudart is the UPPER edge; each stateful compute call
//! lowers the CUDA model into the shared `dd-gpu` IR through [`hl_gpu::cuda::CudaContext`] and EXECUTEs
//! it in-process on the embedded [`SoftwareBackend`](hl_gpu::software::SoftwareBackend) — the SAME IR +
//! executor dd-shim-cuda's driver bodies use (dd-shim-cudart is a PEER over dd-gpu, not a consumer of
//! dd-shim-cuda). A runtime vector-add thus runs end-to-end with NO GPU and reads back
//! numerically-correct results — identical to `hl-gpu/cuda/cudart_shim.c` (the parity oracle).
//!
//! ## Coverage
//! The exported entry-point *surface* is code-generated from a committed manifest (`build.rs` +
//! `registry/`), extracted from dd's clean-room `hl-gpu/cuda/cudart_shim.c` runtime surface plus the
//! standard driver-backed tail, so it is genuinely surface-complete. Every entry point in
//! [`build::IMPLEMENTED`](../build.rs) has a real hand-written body in [`runtime`]; there are no
//! generated stubs left.
//!
//! ## What cudart owns (vs the driver)
//! The `CUresult → cudaError_t` map ([`result::map_err`]), a thread-local last-error cell, the nvcc
//! registration glue (`__cudaRegister*` + the `<<<>>>` call-config stack), and the fatbin → PTX walk
//! ([`fatbin`]) — on top of the shared compute core in [`state`]. See
//! `docs/rendering/SHIM_RUST_ARCHITECTURE.md`.

// The generated + hand-written entry-point surface uses the CUDA C names verbatim (cudaMalloc, …).
#![allow(non_snake_case)]

// The shared IR + transport foundation (re-exported so readers see the IR type is dd-gpu's).
pub use hl_shim as common;

pub mod capability;
pub mod fatbin;
pub mod result;
pub mod runtime;
pub mod state;
pub mod stub;

// The generated C-ABI export surface (every entry point not in `IMPLEMENTED`; currently none).
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported CUDA Runtime API entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = CUDART_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_void;
    use runtime::Dim3;

    /// The runtime entry points share one process-global state (the driver shim's `CudaContext` +
    /// this crate's registries), so tests that drive them must not run concurrently — cargo runs tests
    /// in parallel by default. Each such test holds this guard for its whole body.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    const CUDA_SUCCESS: i32 = result::CUDA_SUCCESS_RT;

    /// Build a SYNTHETIC uncompressed fatbin container wrapping `ptx` (kind=1, PTX), exactly the layout
    /// `hl-gpu/cuda/fatbin.h` / `test_cudart.c` produce, so the runtime launch path walks a real fatbin.
    fn build_fatbin(ptx: &str) -> Vec<u8> {
        let payload = {
            let mut v = ptx.as_bytes().to_vec();
            v.push(0); // NUL-terminated PTX text
            v
        };
        let fat_size = 64u64 + payload.len() as u64; // entry header (64) + payload
        let mut buf = Vec::new();
        // ---- container header (16 bytes) ----
        buf.extend_from_slice(&0xba55ed50u32.to_le_bytes()); // magic
        buf.extend_from_slice(&1u16.to_le_bytes()); // version
        buf.extend_from_slice(&16u16.to_le_bytes()); // header_size
        buf.extend_from_slice(&fat_size.to_le_bytes()); // fat_size
        // ---- entry header (64 bytes) ----
        let entry_start = buf.len();
        buf.extend_from_slice(&1u16.to_le_bytes()); // kind = PTX
        buf.extend_from_slice(&1u16.to_le_bytes()); // version
        buf.extend_from_slice(&64u32.to_le_bytes()); // header_size
        buf.extend_from_slice(&(payload.len() as u64).to_le_bytes()); // payload_size
        buf.resize(entry_start + 64, 0); // zero-fill the rest of the entry header (flags @40 = 0)
        // ---- payload ----
        buf.extend_from_slice(&payload);
        buf
    }

    #[test]
    fn surface_is_complete_and_large() {
        // The manifest-driven surface must be the full runtime set, not a hand-picked few.
        assert!(
            CUDART_ENTRYPOINTS >= 45,
            "CUDA runtime surface too small: {CUDART_ENTRYPOINTS}"
        );
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_COUNT, TOTAL_ENTRYPOINTS);
        // The whole surface has real hand-written bodies at parity with the C oracle — no stubs left.
        assert_eq!(GENERATED_STUBS, 0, "the runtime surface should be fully implemented");
    }

    const IMPLEMENTED_COUNT: usize = TOTAL_ENTRYPOINTS - GENERATED_STUBS;

    /// TIER 0 device query + memory round-trip through the exported `cuda*` API, exactly as an
    /// unmodified CUDA app would: device count/props, malloc, H2D, memset, D2H — asserting the bytes
    /// round-trip and the last-error cell behaves.
    #[test]
    fn tier0_device_and_memory_roundtrip() {
        let _g = serial();
        let mut count = -1;
        assert_eq!(runtime::cudaGetDeviceCount(&mut count), CUDA_SUCCESS);
        assert_eq!(count, 1);
        assert_eq!(runtime::cudaSetDevice(0), CUDA_SUCCESS);
        let mut dev = -1;
        assert_eq!(runtime::cudaGetDevice(&mut dev), CUDA_SUCCESS);
        assert_eq!(dev, 0);
        // cudaSetDevice(1) is a clean InvalidDevice, recorded as the sticky last error.
        assert_eq!(runtime::cudaSetDevice(1), result::CUDA_ERROR_INVALID_DEVICE_RT);
        assert_eq!(runtime::cudaGetLastError(), result::CUDA_ERROR_INVALID_DEVICE_RT);
        assert_eq!(runtime::cudaGetLastError(), CUDA_SUCCESS, "last error drained");

        // A bad memcpy kind is InvalidMemcpyDirection; peek sees it sticky, get drains it.
        assert_eq!(
            runtime::cudaMemcpy(16usize as *mut c_void, 16usize as *const c_void, 8, 99),
            result::CUDA_ERROR_INVALID_MEMCPY_DIRECTION
        );
        assert_eq!(runtime::cudaPeekAtLastError(), result::CUDA_ERROR_INVALID_MEMCPY_DIRECTION);
        assert_eq!(runtime::cudaGetLastError(), result::CUDA_ERROR_INVALID_MEMCPY_DIRECTION);

        let n = 1000usize;
        let nb = n * 4;
        let ha: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let mut da: *mut c_void = core::ptr::null_mut();
        assert_eq!(runtime::cudaMalloc(&mut da, nb), CUDA_SUCCESS);
        assert!(!da.is_null());
        assert_eq!(
            runtime::cudaMemcpy(da, ha.as_ptr() as *const c_void, nb, result::CUDA_MEMCPY_HOST_TO_DEVICE),
            CUDA_SUCCESS
        );
        let mut hc = vec![0f32; n];
        assert_eq!(
            runtime::cudaMemcpy(hc.as_mut_ptr() as *mut c_void, da, nb, result::CUDA_MEMCPY_DEVICE_TO_HOST),
            CUDA_SUCCESS
        );
        assert_eq!(ha, hc, "H2D -> D2H must round-trip exactly");
        assert_eq!(runtime::cudaFree(da), CUDA_SUCCESS);
        assert_eq!(runtime::cudaFree(core::ptr::null_mut()), CUDA_SUCCESS, "cudaFree(NULL) ok");

        // cudaGetDeviceProperties populates the modeled device.
        let mut prop = vec![0u8; 1024];
        assert_eq!(runtime::cudaGetDeviceProperties(prop.as_mut_ptr() as *mut c_void, 0), CUDA_SUCCESS);
        assert!(prop[0] != 0, "device name populated");
    }

    /// Anti-drift round-trip: drive the REAL exported IR-emitting runtime entry points (`cudaMalloc` →
    /// `cudaMemcpy` H2D), then decode the accumulated IR bytes with the HOST's own `hl_gpu::ir` decoder.
    /// Same bytes, same code path — the guest producer (runtime → driver) and host executor can't drift.
    #[test]
    fn runtime_encodes_the_shared_ir_contract() {
        use hl_gpu::ir::{decode_stream, Cmd};
        let _g = serial();
        // Drain any frame accumulated by earlier tests so we decode only our own commands.
        assert_eq!(runtime::cudaDeviceSynchronize(), CUDA_SUCCESS);

        let mut da: *mut c_void = core::ptr::null_mut();
        let mut db: *mut c_void = core::ptr::null_mut();
        assert_eq!(runtime::cudaMalloc(&mut da, 1024), CUDA_SUCCESS);
        assert_eq!(runtime::cudaMalloc(&mut db, 1024), CUDA_SUCCESS);
        let host: Vec<u8> = (0..1024u32).map(|i| i as u8).collect();
        assert_eq!(
            runtime::cudaMemcpy(da, host.as_ptr() as *const c_void, host.len(), result::CUDA_MEMCPY_HOST_TO_DEVICE),
            CUDA_SUCCESS
        );

        let bytes = state::with(|s| s.frame.finish());
        let cmds = decode_stream(&bytes).expect("host decoder must accept shim-encoded IR");
        let buffers = cmds.iter().filter(|c| matches!(c, Cmd::CreateBuffer(..))).count();
        assert!(buffers >= 2, "cudaMalloc -> >=2 CreateBuffer, got {buffers}");
        assert!(
            cmds.iter().any(|c| matches!(c, Cmd::WriteBuffer { .. })),
            "cudaMemcpy H2D -> WriteBuffer"
        );
        // clean up frame
        assert_eq!(runtime::cudaDeviceSynchronize(), CUDA_SUCCESS);
        runtime::cudaFree(da);
        runtime::cudaFree(db);
    }

    /// THE FUNCTIONAL MILESTONE: a full runtime-API vecadd through the nvcc glue + synthetic fatbin,
    /// exactly as a compiled CUDA app runs: register a fatbin, `cudaMalloc` / `cudaMemcpy` H2D, push a
    /// `<<<grid,block>>>` config, pop it in a "host stub", `cudaLaunchKernel`, `cudaDeviceSynchronize`,
    /// `cudaMemcpy` D2H — asserting `c[i] == a[i] + b[i]`. Proves the runtime → driver → shared IR →
    /// embedded PTX interpreter path executes end-to-end with NO GPU (the numbers `cudart_shim.c` gives).
    #[test]
    fn vecadd_executes_end_to_end_through_cudart() {
        let _g = serial();
        let n = 1024usize;
        let nb = n * 4;

        // Register a synthetic uncompressed fatbin (PTX) via the nvcc glue, keyed by a fake host stub.
        let fatbin = build_fatbin(hl_gpu::ptx::VECADD_PTX);
        let handle = runtime::__cudaRegisterFatBinary(fatbin.as_ptr() as *mut c_void);
        assert!(!handle.is_null());
        let host_stub = 0xF00Dusize as *const c_void; // any unique non-null host-fun key
        let dev_name = std::ffi::CString::new("vecadd").unwrap();
        runtime::__cudaRegisterFunction(
            handle,
            host_stub as *const core::ffi::c_char,
            core::ptr::null_mut(),
            dev_name.as_ptr(),
            -1,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        runtime::__cudaRegisterFatBinaryEnd(handle);

        let ha: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let hb: Vec<f32> = (0..n).map(|i| (n - i) as f32 * 0.25).collect();
        let to_bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();

        let (mut da, mut db, mut dc): (*mut c_void, *mut c_void, *mut c_void) =
            (core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut());
        assert_eq!(runtime::cudaMalloc(&mut da, nb), CUDA_SUCCESS);
        assert_eq!(runtime::cudaMalloc(&mut db, nb), CUDA_SUCCESS);
        assert_eq!(runtime::cudaMalloc(&mut dc, nb), CUDA_SUCCESS);
        let (ba, bb) = (to_bytes(&ha), to_bytes(&hb));
        assert_eq!(
            runtime::cudaMemcpy(da, ba.as_ptr() as *const c_void, nb, result::CUDA_MEMCPY_HOST_TO_DEVICE),
            CUDA_SUCCESS
        );
        assert_eq!(
            runtime::cudaMemcpy(db, bb.as_ptr() as *const c_void, nb, result::CUDA_MEMCPY_HOST_TO_DEVICE),
            CUDA_SUCCESS
        );

        // The <<<grid,block>>> call site nvcc lowers: push config, then the host stub pops it + launches.
        let grid = Dim3 { x: (n as u32).div_ceil(256), y: 1, z: 1 };
        let block = Dim3 { x: 256, y: 1, z: 1 };
        assert_eq!(runtime::__cudaPushCallConfiguration(grid, block, 0, core::ptr::null_mut()), 0);
        let (mut g, mut b) = (Dim3 { x: 0, y: 0, z: 0 }, Dim3 { x: 0, y: 0, z: 0 });
        let mut sh = 0usize;
        let mut st: *mut c_void = core::ptr::null_mut();
        assert_eq!(
            runtime::__cudaPopCallConfiguration(&mut g, &mut b, &mut sh, &mut st as *mut _ as *mut c_void),
            CUDA_SUCCESS
        );
        let mut nn: u32 = n as u32;
        let mut args: [*mut c_void; 4] = [
            &mut da as *mut _ as *mut c_void,
            &mut db as *mut _ as *mut c_void,
            &mut dc as *mut _ as *mut c_void,
            &mut nn as *mut u32 as *mut c_void,
        ];
        assert_eq!(
            runtime::cudaLaunchKernel(host_stub, g, b, args.as_mut_ptr(), sh, st),
            CUDA_SUCCESS
        );
        assert_eq!(runtime::cudaDeviceSynchronize(), CUDA_SUCCESS);

        let mut out = vec![0u8; nb];
        assert_eq!(
            runtime::cudaMemcpy(out.as_mut_ptr() as *mut c_void, dc, nb, result::CUDA_MEMCPY_DEVICE_TO_HOST),
            CUDA_SUCCESS
        );
        for i in 0..n {
            let got = f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(got, ha[i] + hb[i], "cudart vecadd mismatch at c[{i}]");
        }

        runtime::cudaFree(da);
        runtime::cudaFree(db);
        runtime::cudaFree(dc);
        runtime::__cudaUnregisterFatBinary(handle);
    }

    /// A SASS-only (ELF) fatbin has no usable PTX and must surface a clean `cudaErrorInvalidKernelImage`
    /// (never a crash); an unregistered host function is `cudaErrorInvalidDeviceFunction`. Parity with
    /// `test_cudart.c`.
    #[test]
    fn bad_fatbin_and_unregistered_fn_reject_cleanly() {
        let _g = serial();
        // SASS-only fatbin: a kind=2 (ELF) entry, no PTX.
        let mut sass = Vec::new();
        let elf: &[u8] = &[0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        let payload_len = elf.len();
        let fat_size = 64u64 + payload_len as u64;
        sass.extend_from_slice(&0xba55ed50u32.to_le_bytes());
        sass.extend_from_slice(&1u16.to_le_bytes());
        sass.extend_from_slice(&16u16.to_le_bytes());
        sass.extend_from_slice(&fat_size.to_le_bytes());
        let es = sass.len();
        sass.extend_from_slice(&2u16.to_le_bytes()); // kind = ELF
        sass.extend_from_slice(&1u16.to_le_bytes());
        sass.extend_from_slice(&64u32.to_le_bytes());
        sass.extend_from_slice(&(payload_len as u64).to_le_bytes());
        sass.resize(es + 64, 0);
        sass.extend_from_slice(elf);

        let handle = runtime::__cudaRegisterFatBinary(sass.as_ptr() as *mut c_void);
        let ghost_key = 0xBEEFusize as *const c_void;
        let name = std::ffi::CString::new("ghost").unwrap();
        runtime::__cudaRegisterFunction(
            handle, ghost_key as *const core::ffi::c_char, core::ptr::null_mut(), name.as_ptr(),
            -1, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut(),
            core::ptr::null_mut(), core::ptr::null_mut(),
        );
        let one = Dim3 { x: 1, y: 1, z: 1 };
        let mut noargs: [*mut c_void; 1] = [core::ptr::null_mut()];
        assert_eq!(
            runtime::cudaLaunchKernel(ghost_key, one, one, noargs.as_mut_ptr(), 0, core::ptr::null_mut()),
            result::CUDA_ERROR_INVALID_KERNEL_IMAGE
        );
        let _ = runtime::cudaGetLastError();

        // An unregistered host function -> InvalidDeviceFunction.
        let bogus = 0x1234usize as *const c_void;
        assert_eq!(
            runtime::cudaLaunchKernel(bogus, one, one, noargs.as_mut_ptr(), 0, core::ptr::null_mut()),
            result::CUDA_ERROR_INVALID_DEVICE_FUNCTION
        );
        let _ = runtime::cudaGetLastError();
        runtime::__cudaUnregisterFatBinary(handle);
    }

    /// Streams + events forward to the driver: create/record/query/elapsed-time/destroy all succeed and
    /// elapsed time is a finite, non-negative millisecond span.
    #[test]
    fn streams_and_events_forward_to_driver() {
        let _g = serial();
        let mut s: *mut c_void = core::ptr::null_mut();
        assert_eq!(runtime::cudaStreamCreate(&mut s), CUDA_SUCCESS);
        let (mut e0, mut e1): (*mut c_void, *mut c_void) =
            (core::ptr::null_mut(), core::ptr::null_mut());
        assert_eq!(runtime::cudaEventCreate(&mut e0), CUDA_SUCCESS);
        assert_eq!(runtime::cudaEventCreate(&mut e1), CUDA_SUCCESS);
        assert_eq!(runtime::cudaEventRecord(e0, s), CUDA_SUCCESS);
        assert_eq!(runtime::cudaEventRecord(e1, s), CUDA_SUCCESS);
        assert_eq!(runtime::cudaEventSynchronize(e1), CUDA_SUCCESS);
        assert_eq!(runtime::cudaEventQuery(e1), CUDA_SUCCESS);
        let mut ms = -1.0f32;
        assert_eq!(runtime::cudaEventElapsedTime(&mut ms, e0, e1), CUDA_SUCCESS);
        assert!(ms >= 0.0 && ms.is_finite(), "elapsed must be finite non-negative: {ms}");
        assert_eq!(runtime::cudaStreamSynchronize(s), CUDA_SUCCESS);
        assert_eq!(runtime::cudaEventDestroy(e0), CUDA_SUCCESS);
        assert_eq!(runtime::cudaEventDestroy(e1), CUDA_SUCCESS);
        assert_eq!(runtime::cudaStreamDestroy(s), CUDA_SUCCESS);
    }

    /// A kernel using an instruction outside dd's modeled PTX subset (`shfl.sync`, a warp intrinsic).
    /// It parses as a valid entry (so the fatbin walk + module load succeed) but cannot be executed.
    const UNSUPPORTED_PTX: &str = r#"
        .version 7.5
        .target sm_86
        .address_size 64
        .visible .entry unsup(
            .param .u64 unsup_param_0,
            .param .u32 unsup_param_1
        )
        {
            .reg .b32 %r<6>;
            .reg .b64 %rd<3>;
            ld.param.u64 %rd1, [unsup_param_0];
            ld.param.u32 %r2, [unsup_param_1];
            mov.u32 %r3, %tid.x;
            shfl.sync.down.b32 %r4, %r3, 1, 31, -1;
            ret;
        }
    "#;

    /// Register an unsupported-kernel fatbin via the nvcc glue; returns (host_stub, handle). The
    /// host-stub key is unique per call (the process-global func registry accumulates across tests, so a
    /// reused key would collide with an earlier now-unregistered entry).
    fn register_unsupported() -> (*const c_void, *mut *mut c_void) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static KEY: AtomicUsize = AtomicUsize::new(0xBADC0DE0);
        let fatbin = build_fatbin(UNSUPPORTED_PTX);
        // Leak the fatbin bytes so the raw pointer registered stays valid for the process lifetime
        // (the registry stores it as a `usize`); a real nvcc fatbin has static storage duration too.
        let leaked = Box::leak(fatbin.into_boxed_slice());
        let handle = runtime::__cudaRegisterFatBinary(leaked.as_ptr() as *mut c_void);
        let host_stub = KEY.fetch_add(1, Ordering::Relaxed) as *const c_void;
        let dev_name = std::ffi::CString::new("unsup").unwrap();
        runtime::__cudaRegisterFunction(
            handle, host_stub as *const core::ffi::c_char, core::ptr::null_mut(), dev_name.as_ptr(),
            -1, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut(),
            core::ptr::null_mut(), core::ptr::null_mut(),
        );
        runtime::__cudaRegisterFatBinaryEnd(handle);
        (host_stub, handle)
    }

    /// TRUTHFUL FAILURE: a runtime launch of a kernel outside the modeled PTX subset returns the accurate
    /// `cudaErrorNotSupported` — not the old false `cudaSuccess` no-op. Phase-0 exit gate for cudart.
    #[test]
    fn unsupported_ptx_launch_returns_error_not_success() {
        let _g = serial();
        let (host_stub, handle) = register_unsupported();
        let one = Dim3 { x: 1, y: 1, z: 1 };
        let mut noargs: [*mut c_void; 1] = [core::ptr::null_mut()];
        let r = runtime::cudaLaunchKernel(host_stub, one, one, noargs.as_mut_ptr(), 0, core::ptr::null_mut());
        assert_eq!(
            r,
            result::CUDA_ERROR_NOT_SUPPORTED_RT,
            "an unsupported PTX kernel must fail truthfully, not report success"
        );
        // sticky last error reflects it too.
        assert_eq!(runtime::cudaGetLastError(), result::CUDA_ERROR_NOT_SUPPORTED_RT);
        runtime::__cudaUnregisterFatBinary(handle);
    }

    /// DD_SHIM_STRICT=1: cudart aborts at the first unsupported CUDA call. Under `cfg(test)` the strict
    /// path records that it would abort (a thread-local trip flag) rather than killing the test process.
    #[test]
    fn strict_mode_trips_abort_on_unsupported_launch() {
        let _g = serial();
        stub::STRICT_TRIPPED.with(|c| c.set(false));
        std::env::set_var("DD_SHIM_STRICT", "1");
        let (host_stub, handle) = register_unsupported();
        let one = Dim3 { x: 1, y: 1, z: 1 };
        let mut noargs: [*mut c_void; 1] = [core::ptr::null_mut()];
        let r = runtime::cudaLaunchKernel(host_stub, one, one, noargs.as_mut_ptr(), 0, core::ptr::null_mut());
        std::env::remove_var("DD_SHIM_STRICT");
        assert_eq!(r, result::CUDA_ERROR_NOT_SUPPORTED_RT);
        assert!(
            stub::STRICT_TRIPPED.with(|c| c.get()),
            "DD_SHIM_STRICT=1 must trip the abort at the first unsupported call"
        );
        let report = stub::strict_report("cudaLaunchKernel", "kernel `unsup`");
        assert!(report.contains("cudaLaunchKernel"));
        assert!(report.contains("history"));
        let _ = runtime::cudaGetLastError();
        runtime::__cudaUnregisterFatBinary(handle);
    }

    /// The generated capability inventory covers every runtime entry point, its counts reconcile, every
    /// `unsupported` entry carries a real error, and the known classifications are as documented.
    #[test]
    fn capability_inventory_is_complete_and_truthful() {
        assert_eq!(CAPABILITIES.len(), TOTAL_ENTRYPOINTS);
        assert_eq!(CAP_FULL + CAP_PARTIAL + CAP_UNSUPPORTED, TOTAL_ENTRYPOINTS);
        for e in CAPABILITIES {
            match e.cap {
                capability::Cap::Unsupported => {
                    assert_ne!(e.cuda_error, result::CUDA_SUCCESS_RT, "unsupported `{}` must carry an error", e.name);
                    assert!(!e.note.is_empty());
                }
                capability::Cap::Partial => assert!(!e.note.is_empty(), "partial `{}` needs a domain note", e.name),
                capability::Cap::Full => {}
            }
        }
        let find = |n: &str| CAPABILITIES.iter().find(|e| e.name == n).unwrap_or_else(|| panic!("missing {n}"));
        assert_eq!(find("cudaMalloc").cap, capability::Cap::Full);
        assert_eq!(find("cudaLaunchKernel").cap, capability::Cap::Partial);
        assert_eq!(find("cudaLaunchKernel").cuda_error, result::CUDA_ERROR_NOT_SUPPORTED_RT);
        assert_eq!(find("__cudaRegisterVar").cap, capability::Cap::Partial);
    }

    /// ACCURATE ADVERTISEMENT: `cudaRuntimeGetVersion` / `cudaDriverGetVersion` report exactly the
    /// inventory's single-source-of-truth version.
    #[test]
    fn advertised_version_matches_the_inventory() {
        assert_eq!(result::RUNTIME_VERSION, capability::SUPPORTED_RUNTIME_VERSION);
        assert_eq!(result::DRIVER_VERSION, capability::SUPPORTED_RUNTIME_VERSION);
        let mut v = 0i32;
        assert_eq!(runtime::cudaRuntimeGetVersion(&mut v), CUDA_SUCCESS);
        assert_eq!(v, capability::SUPPORTED_RUNTIME_VERSION);
        let mut dv = 0i32;
        assert_eq!(runtime::cudaDriverGetVersion(&mut dv), CUDA_SUCCESS);
        assert_eq!(dv, capability::SUPPORTED_RUNTIME_VERSION);
    }
}
