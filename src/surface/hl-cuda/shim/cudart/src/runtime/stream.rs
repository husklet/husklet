//! Device and stream synchronization entry points.

use core::ffi::c_void;

use hl_cuda::result::{
    RuntimeStatus, CUDART_ERROR_INVALID_RESOURCE_HANDLE, CUDART_ERROR_INVALID_VALUE, CUDART_SUCCESS,
};

use crate::state::ShimState;
#[no_mangle]
pub extern "C" fn cudaDeviceSynchronize() -> i32 {
    ShimState::with(|s| match s.ctx.synchronize(&mut s.sink) {
        Ok(()) => CUDART_SUCCESS,
        Err(e) => s.fail(RuntimeStatus::from(&e).code()),
    })
}

#[no_mangle]
pub extern "C" fn cudaThreadSynchronize() -> i32 {
    cudaDeviceSynchronize()
}

#[no_mangle]
pub extern "C" fn cudaStreamCreate(p_stream: *mut *mut c_void) -> i32 {
    if p_stream.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let h = ShimState::with(|s| {
        let st = s.ctx.streams.create();
        s.intern_stream(st)
    });
    unsafe { *p_stream = h };
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaStreamCreateWithFlags(p_stream: *mut *mut c_void, _flags: u32) -> i32 {
    cudaStreamCreate(p_stream)
}

/// `cudaStreamDestroy(stream)` — retire a created stream. A second destroy, an unknown token, and the
/// reserved default-stream tokens (`NULL`/`cudaStreamLegacy`/`cudaStreamPerThread`, which an application
/// may not destroy) are all `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaStreamDestroy(stream: *mut c_void) -> i32 {
    ShimState::with(|s| {
        if s.destroy_stream(stream) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE)
        }
    })
}

#[no_mangle]
pub extern "C" fn cudaStreamSynchronize(stream: *mut c_void) -> i32 {
    ShimState::with(|s| {
        let Some(st) = s.stream(stream) else {
            return s.fail(CUDART_ERROR_INVALID_VALUE);
        };
        match s.ctx.synchronize_stream(&mut s.sink, st) {
            Ok(()) => CUDART_SUCCESS,
            Err(e) => s.fail(RuntimeStatus::from(&e).code()),
        }
    })
}

/// `cudaStreamQuery(stream)` — is the stream idle? The synchronous executor completes every submit before
/// the call returns, so a valid stream is always ready (`cudaSuccess`). An unknown handle is
/// `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaStreamQuery(stream: *mut c_void) -> i32 {
    ShimState::with(|s| match s.stream(stream) {
        Some(_) => CUDART_SUCCESS,
        None => s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE),
    })
}

/// `cudaStreamWaitEvent(stream, event, flags)` — make `stream` wait on `event`. With the synchronous
/// executor the awaited work has already completed, so this validates both handles and records nothing.
/// An unknown stream or event handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaStreamWaitEvent(stream: *mut c_void, event: *mut c_void, _flags: u32) -> i32 {
    ShimState::with(|s| {
        if s.stream(stream).is_none() || !s.event_is_valid(event) {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        CUDART_SUCCESS
    })
}
