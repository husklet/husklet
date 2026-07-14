//! The hand-written `cuda*` runtime entry points: marshal the CUDA Runtime API C ABI into the `hl_cuda`
//! lowering services through the process-global [`crate::state`] sink.
//!
//! Covers the memory + device + stream basics that map cleanly onto the services (alloc/free/memcpy/
//! memset/synchronize) plus the version/error/device queries a probe expects. The runtime's launch path
//! (`cudaLaunchKernel` + the `__cudaRegister*` fatbin registration) is intentionally a benign stub — it
//! needs the host-function → fatbin registration machinery, which is deferred; the driver-API `cuLaunchKernel`
//! is the wired compute launch.

use core::ffi::{c_char, c_void};

use hl_cuda::model::device::DevicePtr;
use hl_cuda::result::{
    cudart_from_gpu_error, CUDART_ERROR_INVALID_DEVICE, CUDART_ERROR_INVALID_VALUE,
    CUDART_ERROR_NOT_SUPPORTED, CUDART_SUCCESS,
};
use hl_cuda::service::{allocate, synchronize, transfer};

use crate::state::with;

/// CUDA 12.2 — reported by both `cudaDriverGetVersion` and `cudaRuntimeGetVersion`.
const CUDART_VERSION: i32 = 12020;

// cudaMemcpyKind values (stable ABI).
const MEMCPY_HOST_TO_HOST: i32 = 0;
const MEMCPY_HOST_TO_DEVICE: i32 = 1;
const MEMCPY_DEVICE_TO_HOST: i32 = 2;
const MEMCPY_DEVICE_TO_DEVICE: i32 = 3;

unsafe fn bytes<'a>(p: *const c_void, n: usize) -> &'a [u8] {
    if p.is_null() || n == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p as *const u8, n)
    }
}

// ==================================================================================================
// memory
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaMalloc(dev_ptr: *mut *mut c_void, size: usize) -> i32 {
    if dev_ptr.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| match allocate::mem_alloc(&mut s.ctx, &mut s.sink, size as u64) {
        Ok(p) => {
            unsafe { *dev_ptr = p.0 as *mut c_void };
            CUDART_SUCCESS
        }
        Err(e) => s.fail(cudart_from_gpu_error(&e)),
    })
}

#[no_mangle]
pub extern "C" fn cudaFree(dev_ptr: *mut c_void) -> i32 {
    if dev_ptr.is_null() {
        return CUDART_SUCCESS; // cudaFree(NULL) is a valid no-op.
    }
    with(|s| match allocate::mem_free(&mut s.ctx, &mut s.sink, DevicePtr(dev_ptr as u64)) {
        Ok(()) => CUDART_SUCCESS,
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    })
}

#[no_mangle]
pub extern "C" fn cudaMemcpy(dst: *mut c_void, src: *const c_void, count: usize, kind: i32) -> i32 {
    with(|s| memcpy_impl(s, dst, src, count, kind))
}

#[no_mangle]
pub extern "C" fn cudaMemcpyAsync(
    dst: *mut c_void,
    src: *const c_void,
    count: usize,
    kind: i32,
    _stream: *mut c_void,
) -> i32 {
    // Synchronous executor: async copy is the same lowering (ordering is trivially satisfied).
    with(|s| memcpy_impl(s, dst, src, count, kind))
}

fn memcpy_impl(s: &mut crate::state::State, dst: *mut c_void, src: *const c_void, count: usize, kind: i32) -> i32 {
    match kind {
        MEMCPY_HOST_TO_DEVICE => {
            let host = unsafe { bytes(src, count) };
            match transfer::memcpy_htod(&mut s.ctx, &mut s.sink, DevicePtr(dst as u64), host) {
                Ok(()) => CUDART_SUCCESS,
                Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
            }
        }
        MEMCPY_DEVICE_TO_DEVICE => {
            match transfer::memcpy_dtod(&mut s.ctx, &mut s.sink, DevicePtr(dst as u64), DevicePtr(src as u64), count as u64) {
                Ok(()) => CUDART_SUCCESS,
                Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
            }
        }
        MEMCPY_DEVICE_TO_HOST => {
            // Read the device source back through the sink's device→host readback path and copy it into
            // the caller's host buffer.
            match transfer::read_dtoh(&s.ctx, &mut s.sink, DevicePtr(src as u64), count) {
                Ok(bytes) => {
                    if !dst.is_null() {
                        unsafe {
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len())
                        };
                    }
                    CUDART_SUCCESS
                }
                Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
            }
        }
        MEMCPY_HOST_TO_HOST => {
            if !dst.is_null() && !src.is_null() && count > 0 {
                unsafe { std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, count) };
            }
            CUDART_SUCCESS
        }
        // cudaMemcpyDefault (UVA-inferred) is not modeled; report it truthfully rather than mis-copying.
        _ => s.fail(CUDART_ERROR_NOT_SUPPORTED),
    }
}

#[no_mangle]
pub extern "C" fn cudaMemset(dev_ptr: *mut c_void, value: i32, count: usize) -> i32 {
    with(|s| memset_impl(s, dev_ptr, value, count))
}

#[no_mangle]
pub extern "C" fn cudaMemsetAsync(dev_ptr: *mut c_void, value: i32, count: usize, _stream: *mut c_void) -> i32 {
    with(|s| memset_impl(s, dev_ptr, value, count))
}

fn memset_impl(s: &mut crate::state::State, dev_ptr: *mut c_void, value: i32, count: usize) -> i32 {
    let fill = vec![value as u8; count];
    match transfer::memcpy_htod(&mut s.ctx, &mut s.sink, DevicePtr(dev_ptr as u64), &fill) {
        Ok(()) => CUDART_SUCCESS,
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    }
}

#[no_mangle]
pub extern "C" fn cudaMemGetInfo(free_b: *mut usize, total_b: *mut usize) -> i32 {
    if free_b.is_null() || total_b.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| {
        let total = s.ctx.device.total_mem as usize;
        unsafe {
            *total_b = total;
            *free_b = total; // outstanding-byte accounting lives in the driver model; report full here.
        }
    });
    CUDART_SUCCESS
}

// ==================================================================================================
// synchronization
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaDeviceSynchronize() -> i32 {
    with(|s| match synchronize::ctx_synchronize(&mut s.ctx, &mut s.sink) {
        Ok(()) => CUDART_SUCCESS,
        Err(e) => s.fail(cudart_from_gpu_error(&e)),
    })
}

#[no_mangle]
pub extern "C" fn cudaThreadSynchronize() -> i32 {
    cudaDeviceSynchronize()
}

#[no_mangle]
pub extern "C" fn cudaStreamCreate(p_stream: *mut *mut c_void) -> i32 {
    if p_stream.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let h = with(|s| {
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

#[no_mangle]
pub extern "C" fn cudaStreamDestroy(stream: *mut c_void) -> i32 {
    with(|s| match s.stream(stream) {
        Some(st) => {
            s.ctx.streams.destroy(st);
            CUDART_SUCCESS
        }
        None => s.fail(CUDART_ERROR_INVALID_VALUE),
    })
}

#[no_mangle]
pub extern "C" fn cudaStreamSynchronize(stream: *mut c_void) -> i32 {
    with(|s| {
        let Some(st) = s.stream(stream) else {
            return s.fail(CUDART_ERROR_INVALID_VALUE);
        };
        match synchronize::stream_synchronize(&mut s.ctx, &mut s.sink, st) {
            Ok(()) => CUDART_SUCCESS,
            Err(e) => s.fail(cudart_from_gpu_error(&e)),
        }
    })
}

// ==================================================================================================
// device + version + error queries
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaGetDeviceCount(count: *mut i32) -> i32 {
    if count.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    unsafe { *count = 1 };
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaGetDevice(device: *mut i32) -> i32 {
    if device.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| unsafe { *device = s.device });
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaSetDevice(device: i32) -> i32 {
    if device != 0 {
        return with(|s| s.fail(CUDART_ERROR_INVALID_DEVICE));
    }
    with(|s| s.device = 0);
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaDriverGetVersion(version: *mut i32) -> i32 {
    if version.is_null() {
        return CUDART_ERROR_INVALID_VALUE;
    }
    unsafe { *version = CUDART_VERSION };
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaRuntimeGetVersion(version: *mut i32) -> i32 {
    if version.is_null() {
        return CUDART_ERROR_INVALID_VALUE;
    }
    unsafe { *version = CUDART_VERSION };
    CUDART_SUCCESS
}

fn error_text(code: i32, name: bool) -> &'static [u8] {
    match (code, name) {
        (0, false) => b"no error\0",
        (0, true) => b"cudaSuccess\0",
        (1, _) => b"cudaErrorInvalidValue\0",
        (2, _) => b"cudaErrorMemoryAllocation\0",
        (101, _) => b"cudaErrorInvalidDevice\0",
        (801, _) => b"cudaErrorNotSupported\0",
        (_, true) => b"cudaErrorUnknown\0",
        (_, false) => b"unknown error\0",
    }
}

#[no_mangle]
pub extern "C" fn cudaGetErrorString(error: i32) -> *const c_char {
    error_text(error, false).as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaGetErrorName(error: i32) -> *const c_char {
    error_text(error, true).as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaGetLastError() -> i32 {
    with(|s| std::mem::replace(&mut s.last_error, CUDART_SUCCESS))
}

#[no_mangle]
pub extern "C" fn cudaPeekAtLastError() -> i32 {
    with(|s| s.last_error)
}

#[no_mangle]
pub extern "C" fn cudaDeviceReset() -> i32 {
    with(|s| {
        s.last_error = CUDART_SUCCESS;
        s.device = 0;
    });
    CUDART_SUCCESS
}
