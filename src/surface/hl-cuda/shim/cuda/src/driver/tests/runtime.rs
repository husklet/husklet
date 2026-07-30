use super::support::*;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
#[test]
fn host_func_and_stream_callback_run_inline() {
    let _g = guard();
    let counter = AtomicUsize::new(0);
    let p = &counter as *const AtomicUsize as *mut c_void;
    let hf: extern "C" fn(*mut c_void) = host_cb;
    let sf: extern "C" fn(*mut c_void, i32, *mut c_void) = stream_cb;

    // The host func runs inline on the default stream (synchronous executor).
    assert_eq!(
        cuLaunchHostFunc(core::ptr::null_mut(), hf as *mut c_void, p),
        CUDA_SUCCESS
    );
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    // A null callback / bogus stream are rejected honestly.
    assert_eq!(
        cuLaunchHostFunc(core::ptr::null_mut(), core::ptr::null_mut(), p),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        cuLaunchHostFunc(0x9999 as *mut c_void, hf as *mut c_void, p),
        CUDA_ERROR_INVALID_HANDLE
    );

    // The stream callback fires with success.
    assert_eq!(
        cuStreamAddCallback(core::ptr::null_mut(), sf as *mut c_void, p, 0),
        CUDA_SUCCESS
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert_eq!(
        cuStreamAddCallback(core::ptr::null_mut(), core::ptr::null_mut(), p, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        cuStreamAddCallback(0x9999 as *mut c_void, sf as *mut c_void, p, 0),
        CUDA_ERROR_INVALID_HANDLE
    );
}

#[test]
fn get_proc_address_aliases_and_error_paths() {
    let _g = guard();
    // The alias table maps a base name to its newest versioned symbol (the app-facing contract).
    assert_eq!(CudaSymbol::newest("cuMemAlloc"), "cuMemAlloc_v2");
    assert_eq!(CudaSymbol::newest("cuCtxCreate"), "cuCtxCreate_v2");
    assert_eq!(CudaSymbol::newest("cuLaunchKernel"), "cuLaunchKernel"); // already the real symbol

    // Null args are rejected.
    let mut pfn: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuGetProcAddress(core::ptr::null(), &mut pfn, 12020, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    let sym = std::ffi::CString::new("cuInit").unwrap();
    assert_eq!(
        cuGetProcAddress(sym.as_ptr(), core::ptr::null_mut(), 12020, 0),
        CUDA_ERROR_INVALID_VALUE
    );

    // A symbol this driver does not export is honestly NOT_FOUND, and _v2 reports the status.
    let bogus = std::ffi::CString::new("cuNotARealEntryPoint").unwrap();
    assert_eq!(
        cuGetProcAddress(bogus.as_ptr(), &mut pfn, 12020, 0),
        CUDA_ERROR_NOT_FOUND
    );
    assert!(pfn.is_null());
    let mut status = -1i32;
    assert_eq!(
        cuGetProcAddress_v2(bogus.as_ptr(), &mut pfn, 12020, 0, &mut status),
        CUDA_ERROR_NOT_FOUND
    );
    assert_eq!(status, CU_GET_PROC_ADDRESS_SYMBOL_NOT_FOUND);
}

#[test]
fn ctx_id_and_shared_mem_config_round_trip() {
    let _g = guard();
    let mut ctx: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut ctx, 0, 0), CUDA_SUCCESS);

    // cuCtxGetId reports the current context's token; an explicit ctx reports its own.
    let mut id = 0u64;
    assert_eq!(cuCtxGetId(core::ptr::null_mut(), &mut id), CUDA_SUCCESS);
    assert_eq!(id, ctx as u64);
    assert_eq!(cuCtxGetId(ctx, &mut id), CUDA_SUCCESS);
    assert_eq!(id, ctx as u64);
    assert_eq!(
        cuCtxGetId(ctx, core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );

    // v3 create is equivalent to v2 (affinity params ignored).
    let mut ctx3: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuCtxCreate_v3(&mut ctx3, core::ptr::null_mut(), 0, 5, 0),
        CUDA_SUCCESS
    );
    assert!(!ctx3.is_null());

    // Shared-mem config round-trips; persisting-L2 reset is a valid no-op.
    let mut c = -1i32;
    assert_eq!(cuCtxGetSharedMemConfig(&mut c), CUDA_SUCCESS);
    assert_eq!(c, 0);
    assert_eq!(cuCtxSetSharedMemConfig(2), CUDA_SUCCESS);
    assert_eq!(cuCtxGetSharedMemConfig(&mut c), CUDA_SUCCESS);
    assert_eq!(c, 2);
    assert_eq!(cuCtxResetPersistingL2Cache(), CUDA_SUCCESS);
}

#[test]
fn peer_access_is_honestly_unsupported() {
    let _g = guard();
    // A single simulated device has no peers.
    let mut can = -1i32;
    assert_eq!(cuDeviceCanAccessPeer(&mut can, 0, 0), CUDA_SUCCESS);
    assert_eq!(can, 0);
    assert_eq!(
        cuDeviceCanAccessPeer(core::ptr::null_mut(), 0, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    // Enable/disable peer access are honest, distinct errors (never a fake success).
    assert_eq!(
        cuCtxEnablePeerAccess(0x1 as *mut c_void, 0),
        CUDA_ERROR_PEER_ACCESS_UNSUPPORTED
    );
    assert_eq!(
        cuCtxDisablePeerAccess(0x1 as *mut c_void),
        CUDA_ERROR_PEER_ACCESS_NOT_ENABLED
    );
}

#[test]
fn device_identity_pci_luid_and_properties() {
    let _g = guard();
    // PCI bus id is written into the caller's buffer.
    let mut buf = [0 as c_char; 32];
    assert_eq!(cuDeviceGetPCIBusId(buf.as_mut_ptr(), 32, 0), CUDA_SUCCESS);
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_str()
        .unwrap();
    assert_eq!(s, "0000:00:00.0");
    assert_eq!(
        cuDeviceGetPCIBusId(core::ptr::null_mut(), 32, 0),
        CUDA_ERROR_INVALID_VALUE
    );

    // By-PCI-id resolves the single device to ordinal 0.
    let mut dev = -1i32;
    let id = std::ffi::CString::new("0000:00:00.0").unwrap();
    assert_eq!(cuDeviceGetByPCIBusId(&mut dev, id.as_ptr()), CUDA_SUCCESS);
    assert_eq!(dev, 0);

    // LUID is Windows/TCC-only → honest NOT_SUPPORTED (bad ordinal is INVALID_DEVICE).
    let mut luid = [0 as c_char; 8];
    let mut mask = 0u32;
    assert_eq!(
        cuDeviceGetLuid(luid.as_mut_ptr(), &mut mask, 0),
        CUDA_ERROR_NOT_SUPPORTED
    );
    assert_eq!(
        cuDeviceGetLuid(luid.as_mut_ptr(), &mut mask, 1),
        CUDA_ERROR_INVALID_DEVICE
    );

    // The original properties struct mirrors the attribute values.
    let mut prop = CuDevprop {
        max_threads_per_block: 0,
        max_threads_dim: [0; 3],
        max_grid_size: [0; 3],
        shared_mem_per_block: 0,
        total_constant_memory: 0,
        simd_width: 0,
        mem_pitch: 0,
        regs_per_block: 0,
        clock_rate: 0,
        texture_align: 0,
    };
    assert_eq!(
        cuDeviceGetProperties(&mut prop as *mut CuDevprop as *mut c_void, 0),
        CUDA_SUCCESS
    );
    let (want_max, want_warp) = ShimState::with(|s| {
        (
            s.ctx.device.max_threads_per_block as i32,
            s.ctx.device.warp_size as i32,
        )
    });
    assert_eq!(prop.max_threads_per_block, want_max);
    assert_eq!(prop.simd_width, want_warp);
    assert_eq!(prop.total_constant_memory, 65536);
    assert_eq!(
        cuDeviceGetProperties(core::ptr::null_mut(), 0),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn func_module_name_and_shared_config() {
    let _g = guard();
    let f = load_vecadd();

    // cuFuncGetName reports the entry the function was resolved by.
    let mut np: *const c_char = core::ptr::null();
    assert_eq!(cuFuncGetName(&mut np, f), CUDA_SUCCESS);
    let name = unsafe { std::ffi::CStr::from_ptr(np) }.to_str().unwrap();
    assert_eq!(name, "vecadd");
    // cuFuncGetModule hands back a non-null module handle for a resolved function.
    let mut m: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuFuncGetModule(&mut m, f), CUDA_SUCCESS);
    assert!(!m.is_null());
    // Shared-mem config validates the handle.
    assert_eq!(cuFuncSetSharedMemConfig(f, 1), CUDA_SUCCESS);

    // Bad handles are rejected honestly.
    assert_eq!(
        cuFuncGetName(&mut np, 0x1234 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuFuncGetModule(&mut m, 0x1234 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuFuncSetSharedMemConfig(0x1234 as *mut c_void, 1),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuFuncGetName(core::ptr::null_mut(), f),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn stream_capture_priority_and_memory_hints() {
    let _g = guard();
    // A priority stream round-trips its priority through cuStreamGetPriority.
    let mut stream: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamCreateWithPriority(&mut stream, 1, -2), CUDA_SUCCESS);
    let mut prio = 0i32;
    assert_eq!(cuStreamGetPriority(stream, &mut prio), CUDA_SUCCESS);
    assert_eq!(prio, -2);

    // Capture is unsupported → a valid stream honestly reports NONE (0).
    let mut status = -1i32;
    assert_eq!(cuStreamIsCapturing(stream, &mut status), CUDA_SUCCESS);
    assert_eq!(status, 0);
    assert_eq!(
        cuStreamIsCapturing(0x9999 as *mut c_void, &mut status),
        CUDA_ERROR_INVALID_HANDLE
    );
    let mut mode = 1i32;
    assert_eq!(cuThreadExchangeStreamCaptureMode(&mut mode), CUDA_SUCCESS);
    assert_eq!(
        cuThreadExchangeStreamCaptureMode(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );

    // Unified-memory hints are valid no-ops; stream-scoped ones validate the stream.
    assert_eq!(cuMemAdvise(0xdead_beef, 4096, 0, 0), CUDA_SUCCESS);
    assert_eq!(
        cuMemPrefetchAsync(0xdead_beef, 4096, 0, stream),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuMemPrefetchAsync(0xdead_beef, 4096, 0, 0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuStreamAttachMemAsync(stream, 0xdead_beef, 4096, 4),
        CUDA_SUCCESS
    );
    // Stream-ordered alloc/free reject a bogus stream before touching the allocator.
    let mut dptr = 0u64;
    assert_eq!(
        cuMemAllocAsync(&mut dptr, 64, 0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuMemFreeAsync(0xdead_beef, 0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );

    // Host-alloc flags of a live pinned allocation are reported (0 for the modeled allocator).
    let mut hp: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuMemAllocHost_v2(&mut hp, 32), CUDA_SUCCESS);
    let mut fl = 9u32;
    assert_eq!(cuMemHostGetFlags(&mut fl, hp), CUDA_SUCCESS);
    assert_eq!(fl, 0);
    assert_eq!(cuMemFreeHost(hp), CUDA_SUCCESS);
    assert_eq!(
        cuMemHostGetFlags(&mut fl, core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );

    // Pointer set-attribute (SYNC_MEMOPS) is a valid no-op; a null value is rejected.
    let one = 1i32;
    assert_eq!(
        cuPointerSetAttribute(
            &one as *const i32 as *const c_void,
            CU_POINTER_ATTRIBUTE_SYNC_MEMOPS,
            0
        ),
        CUDA_SUCCESS
    );
    assert_eq!(
        cuPointerSetAttribute(core::ptr::null(), CU_POINTER_ATTRIBUTE_SYNC_MEMOPS, 0),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn available_dynamic_smem_is_sane() {
    let _g = guard();
    let f = load_vecadd();
    // Dynamic shared memory is not expressible in the kernel IR, so the available amount is truthfully 0
    // for any block count — not a share of an SM budget a kernel could never be given.
    let mut smem = 1usize;
    assert_eq!(
        cuOccupancyAvailableDynamicSMemPerBlock(&mut smem, f, 1, 256),
        CUDA_SUCCESS
    );
    assert_eq!(smem, 0);
    smem = 1;
    assert_eq!(
        cuOccupancyAvailableDynamicSMemPerBlock(&mut smem, f, 2, 256),
        CUDA_SUCCESS
    );
    assert_eq!(smem, 0);
    // Invalid args / bad handle rejected honestly.
    assert_eq!(
        cuOccupancyAvailableDynamicSMemPerBlock(core::ptr::null_mut(), f, 1, 256),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        cuOccupancyAvailableDynamicSMemPerBlock(&mut smem, f, 0, 256),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        cuOccupancyAvailableDynamicSMemPerBlock(&mut smem, 0x1234 as *mut c_void, 1, 256),
        CUDA_ERROR_INVALID_HANDLE
    );
}

#[test]
fn profiler_controls_are_benign_noops() {
    let _g = guard();
    assert_eq!(cuProfilerStart(), CUDA_SUCCESS);
    assert_eq!(cuProfilerStop(), CUDA_SUCCESS);
    let cfg = std::ffi::CString::new("cfg").unwrap();
    let out = std::ffi::CString::new("out").unwrap();
    assert_eq!(
        cuProfilerInitialize(cfg.as_ptr(), out.as_ptr(), 0),
        CUDA_SUCCESS
    );
}

#[test]
fn thread_exchange_stream_capture_mode_is_a_real_swap() {
    let _g = guard();
    // Store ThreadLocal(1); the exchange returns whatever the thread's prior mode was and keeps 1.
    let mut mode = 1i32;
    assert_eq!(cuThreadExchangeStreamCaptureMode(&mut mode), CUDA_SUCCESS);
    // A second exchange to Relaxed(2) MUST hand back exactly the 1 we stored — proving the exchange
    // actually mutates thread-local state rather than no-op'ing (the old bug returned success but
    // never wrote the previous mode back).
    mode = 2;
    assert_eq!(cuThreadExchangeStreamCaptureMode(&mut mode), CUDA_SUCCESS);
    assert_eq!(mode, 1, "exchange must return the previously-stored mode");
    // Restore to Global(0); it returns the 2 we just stored.
    mode = 0;
    assert_eq!(cuThreadExchangeStreamCaptureMode(&mut mode), CUDA_SUCCESS);
    assert_eq!(mode, 2);
    // An out-of-range mode is rejected and leaves both the caller's value and the stored mode untouched.
    let mut bad = 7i32;
    assert_eq!(
        cuThreadExchangeStreamCaptureMode(&mut bad),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        bad, 7,
        "a rejected exchange must not overwrite the caller's value"
    );
    let mut probe = 1i32;
    assert_eq!(cuThreadExchangeStreamCaptureMode(&mut probe), CUDA_SUCCESS);
    assert_eq!(probe, 0, "the rejected exchange left the stored mode at 0");
    // Null is rejected.
    assert_eq!(
        cuThreadExchangeStreamCaptureMode(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );
}
