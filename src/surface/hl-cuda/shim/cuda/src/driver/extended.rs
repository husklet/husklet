use super::*;
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn cuLaunchCooperativeKernel(
    f: *mut c_void,
    gx: u32,
    gy: u32,
    gz: u32,
    bx: u32,
    by: u32,
    bz: u32,
    shared_mem_bytes: u32,
    stream: *mut c_void,
    kernel_params: *mut *mut c_void,
) -> i32 {
    cuLaunchKernel(
        f,
        gx,
        gy,
        gz,
        bx,
        by,
        bz,
        shared_mem_bytes,
        stream,
        kernel_params,
        core::ptr::null_mut(),
    )
}

/// `cuLaunchHostFunc(stream, fn, userData)` — enqueue a host callback in stream order. With the
/// synchronous executor all previously-submitted stream work has already completed, so the callback runs
/// inline. It is invoked OUTSIDE the state lock (a callback may re-enter the driver API). A bogus stream
/// handle is `CUDA_ERROR_INVALID_HANDLE`; a null callback is `CUDA_ERROR_INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuLaunchHostFunc(
    stream: *mut c_void,
    fn_: *mut c_void,
    user_data: *mut c_void,
) -> i32 {
    if fn_.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if ShimState::with(|s| s.stream(stream).is_none()) {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    // SAFETY: `fn_` is a `CUhostFn = void(*)(void*)` supplied by the caller.
    let hostfn: extern "C" fn(*mut c_void) = unsafe { core::mem::transmute(fn_) };
    hostfn(user_data);
    CUDA_SUCCESS
}

/// `cuStreamAddCallback(stream, callback, userData, flags)` — the legacy stream-callback API. As with
/// `cuLaunchHostFunc`, the synchronous executor has already completed preceding work, so the callback
/// fires inline with `CUDA_SUCCESS`, OUTSIDE the state lock. A bogus stream is `INVALID_HANDLE`; a null
/// callback is `INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuStreamAddCallback(
    s: *mut c_void,
    cb: *mut c_void,
    user_data: *mut c_void,
    flags: u32,
) -> i32 {
    let _ = flags;
    if cb.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if ShimState::with(|st| st.stream(s).is_none()) {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    // SAFETY: `cb` is a `CUstreamCallback = void(*)(CUstream, CUresult, void*)`.
    let callback: extern "C" fn(*mut c_void, i32, *mut c_void) =
        unsafe { core::mem::transmute(cb) };
    callback(s, CUDA_SUCCESS, user_data);
    CUDA_SUCCESS
}

// ==================================================================================================
// memory: stream-ordered alloc/free, unified-memory hints, host-alloc flags, pointer set-attr
// ==================================================================================================

/// `cuMemAllocAsync(dptr, bytesize, stream)` — stream-ordered device allocation. The synchronous model
/// completes it immediately, sharing `cuMemAlloc_v2`'s body once the stream validates.
#[no_mangle]
pub extern "C" fn cuMemAllocAsync(dptr: *mut u64, bytesize: usize, s: *mut c_void) -> i32 {
    if ShimState::with(|st| st.stream(s).is_none()) {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    cuMemAlloc_v2(dptr, bytesize)
}

/// `cuMemFreeAsync(dptr, stream)` — stream-ordered free; shares `cuMemFree_v2` once the stream validates.
#[no_mangle]
pub extern "C" fn cuMemFreeAsync(dptr: u64, s: *mut c_void) -> i32 {
    if ShimState::with(|st| st.stream(s).is_none()) {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    cuMemFree_v2(dptr)
}

/// `cuMemAdvise(devPtr, count, advice, device)` — a memory-usage hint for managed memory. The model's
/// unified memory needs no migration, so advice is a valid no-op (the same observable result a real
/// driver gives: the hint is accepted and changes no data).
#[no_mangle]
pub extern "C" fn cuMemAdvise(p: u64, n: usize, advice: i32, dev: i32) -> i32 {
    let _ = (p, n, advice, dev);
    CUDA_SUCCESS
}

/// `cuMemPrefetchAsync(devPtr, count, dstDevice, stream)` — prefetch managed memory. Unified memory is
/// always resident in the model, so prefetch is a valid no-op once the stream validates.
#[no_mangle]
pub extern "C" fn cuMemPrefetchAsync(p: u64, n: usize, dst: i32, s: *mut c_void) -> i32 {
    let _ = (p, n, dst);
    if ShimState::with(|st| st.stream(s).is_none()) {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    CUDA_SUCCESS
}

/// `cuStreamAttachMemAsync(stream, devPtr, length, flags)` — scope a managed allocation to a stream. The
/// single-device unified model has no per-stream residency to change, so this is a valid no-op once the
/// stream validates.
#[no_mangle]
pub extern "C" fn cuStreamAttachMemAsync(
    s: *mut c_void,
    dptr: u64,
    length: usize,
    flags: u32,
) -> i32 {
    let _ = (dptr, length, flags);
    if ShimState::with(|st| st.stream(s).is_none()) {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    CUDA_SUCCESS
}

/// `cuMemHostGetFlags(pFlags, p)` — the flags a host allocation was created with. The modeled pinned
/// allocator ignores the (portable/mapped/write-combined) flags, so a *live* host allocation reports 0.
/// A pointer that is not a host allocation we own is `CUDA_ERROR_INVALID_VALUE` — never a fake success
/// reporting flags for memory that was never page-locked.
#[no_mangle]
pub extern "C" fn cuMemHostGetFlags(pflags: *mut u32, p: *mut c_void) -> i32 {
    if pflags.is_null() || p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if !ShimState::with(|s| s.ctx.host.is_host_base(p as u64)) {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *pflags = 0 };
    CUDA_SUCCESS
}

/// `cuPointerSetAttribute(value, attribute, ptr)` — set a writable pointer attribute. The only writable
/// attribute is `SYNC_MEMOPS`, which the synchronous model already reports as enabled, so setting it is a
/// valid no-op. A null value is rejected.
#[no_mangle]
pub extern "C" fn cuPointerSetAttribute(value: *const c_void, attr: i32, ptr: u64) -> i32 {
    let _ = (attr, ptr);
    if value.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    CUDA_SUCCESS
}

/// `cuOccupancyAvailableDynamicSMemPerBlock(dynSmem, f, numBlocks, blockSize)` — the dynamic shared bytes
/// still available per block if `numBlocks` blocks of the function co-reside on an SM: the SM's shared
/// budget split across the blocks, minus the function's static shared use.
#[no_mangle]
pub extern "C" fn cuOccupancyAvailableDynamicSMemPerBlock(
    dyn_smem: *mut usize,
    f: *mut c_void,
    num_blocks: i32,
    block_size: i32,
) -> i32 {
    let _ = block_size;
    if dyn_smem.is_null() || num_blocks <= 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|s| {
        let Some((_num_regs, static_shared)) = FunctionResources::get(s, f) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let per_block = MAX_SHARED_PER_SM / num_blocks; // the SM shared budget divided among the blocks
        let avail = per_block.saturating_sub(static_shared as i32).max(0);
        unsafe { *dyn_smem = avail as usize };
        CUDA_SUCCESS
    })
}

// ==================================================================================================
// stream: priority create, capture status (capture unsupported → honest NONE)
// ==================================================================================================

/// `cuStreamCreateWithPriority(phStream, flags, priority)` — create a stream recording its `(flags,
/// priority)`. The synchronous model has one priority band, but the requested priority round-trips through
/// `cuStreamGetPriority` (as a real driver clamps then reports the honored priority).
#[no_mangle]
pub extern "C" fn cuStreamCreateWithPriority(
    phstream: *mut *mut c_void,
    flags: u32,
    priority: i32,
) -> i32 {
    if phstream.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let h = ShimState::with(|s| {
        let stream = s.ctx.streams.create();
        s.intern_stream(stream, flags, priority)
    });
    unsafe { *phstream = h };
    CUDA_SUCCESS
}

/// `cuStreamIsCapturing(stream, status)` — graph capture is not modeled, so a valid stream is honestly
/// never capturing (`CU_STREAM_CAPTURE_STATUS_NONE`). A bogus handle is `CUDA_ERROR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuStreamIsCapturing(s: *mut c_void, status: *mut i32) -> i32 {
    ShimState::with(|st| {
        if st.stream(s).is_none() {
            return CUDA_ERROR_INVALID_HANDLE;
        }
        if !status.is_null() {
            unsafe { *status = 0 }; // CU_STREAM_CAPTURE_STATUS_NONE
        }
        CUDA_SUCCESS
    })
}

thread_local! {
    /// The calling thread's `CUstreamCaptureMode` (0=Global default, 1=ThreadLocal, 2=Relaxed). No capture
    /// is ever in progress, but the mode is real per-thread state an app sets and restores around a scope,
    /// so `cuThreadExchangeStreamCaptureMode` must swap it and hand back the previous value.
    static CAPTURE_MODE: core::cell::Cell<i32> = const { core::cell::Cell::new(0) };
}

/// `cuThreadExchangeStreamCaptureMode(mode)` — swap the calling thread's stream-capture mode with `*mode`,
/// writing the PREVIOUS mode back into `*mode`. This is a genuine per-thread state exchange (not a no-op):
/// an app that sets a scoped mode and restores it via a second exchange observes the correct prior value.
/// An out-of-range mode (not 0..=2) is `CUDA_ERROR_INVALID_VALUE`, leaving the stored mode untouched.
#[no_mangle]
pub extern "C" fn cuThreadExchangeStreamCaptureMode(mode: *mut i32) -> i32 {
    if mode.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let requested = unsafe { *mode };
    if !(0..=2).contains(&requested) {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let previous = CAPTURE_MODE.with(|m| m.replace(requested));
    unsafe { *mode = previous };
    CUDA_SUCCESS
}

// ==================================================================================================
// profiler: deprecated collection controls (no host profiler → benign no-ops)
// ==================================================================================================

/// `cuProfilerInitialize(configFile, outputFile, outputMode)` — the deprecated profiler-config entry.
/// There is no host profiler to configure, so it is a benign no-op.
#[no_mangle]
pub extern "C" fn cuProfilerInitialize(cfg: *const c_char, out: *const c_char, fmt: i32) -> i32 {
    let _ = (cfg, out, fmt);
    CUDA_SUCCESS
}

/// `cuProfilerStart()` — begin profile collection. No host profiler is attached, so this is a no-op that
/// succeeds (the documented behavior when no profiling session is active).
#[no_mangle]
pub extern "C" fn cuProfilerStart() -> i32 {
    CUDA_SUCCESS
}

/// `cuProfilerStop()` — end profile collection; the no-op counterpart to `cuProfilerStart`.
#[no_mangle]
pub extern "C" fn cuProfilerStop() -> i32 {
    CUDA_SUCCESS
}

// ==================================================================================================
// unit tests for the query/context/pointer entry points
// ==================================================================================================
//
// These call the `extern "C"` entry points directly and assert the real returned values. They touch
// only the sink-free surface (device attributes, context/primary-context tokens, pointer metadata,
// event/stream query) — no GPU-exec socket is needed. The process-global shim state is shared, so a
// single serializing lock + `state::reset()` makes each test deterministic.
