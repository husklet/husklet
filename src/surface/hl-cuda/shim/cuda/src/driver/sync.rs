use super::*;
#[no_mangle]
pub extern "C" fn cuStreamCreate(phstream: *mut *mut c_void, _flags: u32) -> i32 {
    if phstream.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        let stream = s.ctx.streams.create();
        let h = s.intern_stream(stream, _flags, 0);
        unsafe { *phstream = h };
        CUDA_SUCCESS
    })
}

/// `cuStreamDestroy(hStream)` — retire a created stream. A second destroy, an unknown token, and the
/// reserved default-stream tokens (`NULL`/`CU_STREAM_LEGACY`/`CU_STREAM_PER_THREAD`, which an application
/// may not destroy) are all `CUDA_ERROR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuStreamDestroy_v2(hstream: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        if s.destroy_stream(hstream) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

#[no_mangle]
pub extern "C" fn cuStreamSynchronize(hstream: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        let Some(st) = s.stream(hstream) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match s.ctx.synchronize_stream(&mut s.sink, st) {
            Ok(()) => CUDA_SUCCESS,
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

#[no_mangle]
pub extern "C" fn cuEventCreate(phevent: *mut *mut c_void, _flags: u32) -> i32 {
    if phevent.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        let h = s.create_event();
        unsafe { *phevent = h };
        CUDA_SUCCESS
    })
}

#[no_mangle]
pub extern "C" fn cuEventRecord(hevent: *mut c_void, _hstream: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        if s.record_event(hevent) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

#[no_mangle]
pub extern "C" fn cuEventSynchronize(hevent: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        if !s.event_is_valid(hevent) {
            return CUDA_ERROR_INVALID_HANDLE;
        }
        // A recorded event completes when the context's prior work does; barrier the context.
        match s.ctx.synchronize(&mut s.sink) {
            Ok(()) => CUDA_SUCCESS,
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

/// `cuEventDestroy(hEvent)` — retire an event. It really retires the model object, so a second destroy
/// or a use afterwards is `CUDA_ERROR_INVALID_HANDLE` rather than continuing to work.
#[no_mangle]
pub extern "C" fn cuEventDestroy_v2(hevent: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        if s.destroy_event(hevent) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

#[no_mangle]
pub extern "C" fn cuEventRecordWithFlags(
    hevent: *mut c_void,
    hstream: *mut c_void,
    _flags: u32,
) -> i32 {
    cuEventRecord(hevent, hstream)
}

/// `cuEventQuery` — with the synchronous executor a recorded event is already complete
/// (`CUDA_SUCCESS`); a valid-but-unrecorded event is `CUDA_ERROR_NOT_READY`; an unknown handle is
/// `CUDA_ERROR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuEventQuery(hevent: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        if !s.event_is_valid(hevent) {
            CUDA_ERROR_INVALID_HANDLE
        } else if s.event_recorded(hevent) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_NOT_READY
        }
    })
}

#[no_mangle]
pub extern "C" fn cuEventElapsedTime(ms: *mut f32, start: *mut c_void, end: *mut c_void) -> i32 {
    if ms.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        if !s.event_is_valid(start) || !s.event_is_valid(end) {
            return CUDA_ERROR_INVALID_HANDLE;
        }
        match s.event_elapsed_ms(start, end) {
            Some(v) => {
                unsafe { *ms = v };
                CUDA_SUCCESS
            }
            None => CUDA_ERROR_NOT_READY, // one or both events unrecorded
        }
    })
}

/// `cuStreamQuery` — the synchronous executor completes every submit before returning, so a valid
/// stream is always ready. An unknown handle is `CUDA_ERROR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuStreamQuery(hstream: *mut c_void) -> i32 {
    ShimState::with_context(|s| {
        if s.stream(hstream).is_some() {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

/// `cuStreamWaitEvent` — make `hstream` wait on `hevent`. With a synchronous executor the awaited work
/// has already completed, so this validates both handles and returns success.
#[no_mangle]
pub extern "C" fn cuStreamWaitEvent(hstream: *mut c_void, hevent: *mut c_void, _flags: u32) -> i32 {
    ShimState::with_context(|s| {
        if s.stream(hstream).is_none() || !s.event_is_valid(hevent) {
            CUDA_ERROR_INVALID_HANDLE
        } else {
            CUDA_SUCCESS
        }
    })
}

// ==================================================================================================
// stream getters: creation flags / priority / owning context / unique id
// ==================================================================================================

/// `cuStreamGetFlags` — the flags the stream was created with (the default stream reports `0`).
#[no_mangle]
pub extern "C" fn cuStreamGetFlags(hstream: *mut c_void, flags: *mut u32) -> i32 {
    if flags.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.stream_meta(hstream) {
        Some((f, _)) => {
            unsafe { *flags = f };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_HANDLE,
    })
}

/// `cuStreamGetPriority` — the priority the stream was created with (the synchronous model uses a single
/// priority band, so every stream created via `cuStreamCreate` reports `0`).
#[no_mangle]
pub extern "C" fn cuStreamGetPriority(hstream: *mut c_void, priority: *mut i32) -> i32 {
    if priority.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.stream_meta(hstream) {
        Some((_, p)) => {
            unsafe { *priority = p };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_HANDLE,
    })
}

/// `cuStreamGetCtx` — the context a stream belongs to. The single simulated device has one active
/// context, so a valid stream reports the current context.
#[no_mangle]
pub extern "C" fn cuStreamGetCtx(hstream: *mut c_void, pctx: *mut *mut c_void) -> i32 {
    if pctx.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        if s.stream(hstream).is_none() {
            return CUDA_ERROR_INVALID_HANDLE;
        }
        let cur = s.current_ctx();
        unsafe { *pctx = cur };
        CUDA_SUCCESS
    })
}

/// `cuStreamGetId` — a unique id for the stream. The opaque handle value is already a stable per-process
/// id (the default stream is `0`), so it is reported directly.
#[no_mangle]
pub extern "C" fn cuStreamGetId(hstream: *mut c_void, id: *mut u64) -> i32 {
    if id.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        if s.stream(hstream).is_none() {
            return CUDA_ERROR_INVALID_HANDLE;
        }
        unsafe { *id = hstream as u64 };
        CUDA_SUCCESS
    })
}

// ==================================================================================================
// context limits + cache config + stream-priority range
// ==================================================================================================

/// `cuCtxGetLimit` — read a modeled `CUlimit` slot. An out-of-range limit is `UNSUPPORTED_LIMIT`.
#[no_mangle]
pub extern "C" fn cuCtxGetLimit(pvalue: *mut usize, limit: i32) -> i32 {
    if pvalue.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if limit < 0 || limit >= CU_LIMIT_MAX {
        return CUDA_ERROR_UNSUPPORTED_LIMIT;
    }
    ShimState::with_context(|s| {
        let v = s.ctx_limit(limit as usize);
        unsafe { *pvalue = v };
        CUDA_SUCCESS
    })
}

/// `cuCtxSetLimit` — record a modeled `CUlimit` slot (round-trips through `cuCtxGetLimit`). An
/// out-of-range limit is `UNSUPPORTED_LIMIT`.
#[no_mangle]
pub extern "C" fn cuCtxSetLimit(limit: i32, value: usize) -> i32 {
    if limit < 0 || limit >= CU_LIMIT_MAX {
        return CUDA_ERROR_UNSUPPORTED_LIMIT;
    }
    ShimState::with_context(|s| {
        s.set_ctx_limit(limit as usize, value);
        CUDA_SUCCESS
    })
}

/// `cuCtxGetCacheConfig` — the current context's preferred L1/shared cache split.
#[no_mangle]
pub extern "C" fn cuCtxGetCacheConfig(pconfig: *mut i32) -> i32 {
    if pconfig.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        let v = s.ctx_cache_config();
        unsafe { *pconfig = v };
        CUDA_SUCCESS
    })
}

/// `cuCtxSetCacheConfig` — record the context's preferred cache split (round-trips through the getter).
#[no_mangle]
pub extern "C" fn cuCtxSetCacheConfig(config: i32) -> i32 {
    ShimState::with_context(|s| {
        s.set_ctx_cache_config(config);
        CUDA_SUCCESS
    })
}

/// `cuCtxGetStreamPriorityRange` — the `[greatest, least]` numeric priority range. The synchronous model
/// has a single priority band, so both ends are `0` (as a real driver reports when priorities collapse).
#[no_mangle]
pub extern "C" fn cuCtxGetStreamPriorityRange(least: *mut i32, greatest: *mut i32) -> i32 {
    ShimState::with_context(|_| {
        if !least.is_null() {
            unsafe { *least = 0 };
        }
        if !greatest.is_null() {
            unsafe { *greatest = 0 };
        }
        CUDA_SUCCESS
    })
}

// ==================================================================================================
// device: peer access, PCI/LUID identity, original properties struct
// ==================================================================================================
