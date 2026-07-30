//! CUDA event lifecycle and timing entry points.

use core::ffi::c_void;

use hl_cuda::result::{
    CUDART_ERROR_INVALID_RESOURCE_HANDLE, CUDART_ERROR_INVALID_VALUE, CUDART_SUCCESS,
};

use crate::state::ShimState;

/// `cudaErrorNotReady` (600) for an event that has not been recorded.
const CUDART_ERROR_NOT_READY: i32 = 600;
/// `cudaEventCreate(event)` — mint an (unrecorded) event handle.
#[no_mangle]
pub extern "C" fn cudaEventCreate(event: *mut *mut c_void) -> i32 {
    if event.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let h = ShimState::with(|s| s.create_event());
    unsafe { *event = h };
    CUDART_SUCCESS
}

/// `cudaEventCreateWithFlags(event, flags)` — the flagged form. The (blocking-sync / disable-timing /
/// interprocess) flags do not change the modeled monotonic-clock timing, so it shares the body.
#[no_mangle]
pub extern "C" fn cudaEventCreateWithFlags(event: *mut *mut c_void, _flags: u32) -> i32 {
    cudaEventCreate(event)
}

/// `cudaEventRecord(event, stream)` — timestamp the event. Every prior submit has already completed under
/// the synchronous executor, so the record time is the correct completion instant for
/// `cudaEventElapsedTime`. A bad event handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventRecord(event: *mut c_void, _stream: *mut c_void) -> i32 {
    ShimState::with(|s| {
        if s.record_event(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE)
        }
    })
}

/// `cudaEventSynchronize(event)` — block until the event completes. Recorded work is already complete
/// (synchronous executor), so a valid, recorded event returns immediately; an unrecorded one is
/// `cudaErrorNotReady`; a bad handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventSynchronize(event: *mut c_void) -> i32 {
    ShimState::with(|s| {
        if !s.event_is_valid(event) {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        if s.event_recorded(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_NOT_READY)
        }
    })
}

/// `cudaEventQuery(event)` — has the event completed? A recorded event is complete (`cudaSuccess`); a
/// valid-but-unrecorded one is `cudaErrorNotReady`; a bad handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventQuery(event: *mut c_void) -> i32 {
    ShimState::with(|s| {
        if !s.event_is_valid(event) {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        if s.event_recorded(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_NOT_READY)
        }
    })
}

/// `cudaEventElapsedTime(ms, start, end)` — milliseconds between two recorded events, from the monotonic
/// clock. A null out-pointer is `cudaErrorInvalidValue`; an unknown or destroyed event handle is
/// `cudaErrorInvalidResourceHandle`; a live-but-unrecorded event is `cudaErrorNotReady`. The two are
/// different faults and must not report the same code.
#[no_mangle]
pub extern "C" fn cudaEventElapsedTime(ms: *mut f32, start: *mut c_void, end: *mut c_void) -> i32 {
    if ms.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    ShimState::with(|s| {
        if !s.event_is_valid(start) || !s.event_is_valid(end) {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        match s.event_elapsed_ms(start, end) {
            Some(v) => {
                unsafe { *ms = v };
                CUDART_SUCCESS
            }
            None => s.fail(CUDART_ERROR_NOT_READY),
        }
    })
}

/// `cudaEventDestroy(event)` — retire an event handle. A bad handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventDestroy(event: *mut c_void) -> i32 {
    ShimState::with(|s| {
        if s.destroy_event(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE)
        }
    })
}
