//! Device, managed, and pinned-host memory entry points.

use core::ffi::c_void;

use hl_cuda::model::device::DevicePtr;
use hl_cuda::result::{
    RuntimeStatus, CUDART_ERROR_INVALID_VALUE, CUDART_ERROR_MEMORY_ALLOCATION,
    CUDART_ERROR_NOT_SUPPORTED, CUDART_SUCCESS,
};
use hl_cuda::service::{allocate, transfer};

use crate::state::ShimState;

// cudaMemcpyKind values (stable ABI).
const MEMCPY_HOST_TO_HOST: i32 = 0;
const MEMCPY_HOST_TO_DEVICE: i32 = 1;
const MEMCPY_DEVICE_TO_HOST: i32 = 2;
const MEMCPY_DEVICE_TO_DEVICE: i32 = 3;

struct CInput;

impl CInput {
    unsafe fn bytes<'a>(pointer: *const c_void, length: usize) -> &'a [u8] {
        if pointer.is_null() || length == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(pointer as *const u8, length)
        }
    }
}
#[no_mangle]
pub extern "C" fn cudaMalloc(dev_ptr: *mut *mut c_void, size: usize) -> i32 {
    if dev_ptr.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    ShimState::with(
        |s| match allocate::mem_alloc(&mut s.ctx, &mut s.sink, size as u64) {
            Ok(p) => {
                unsafe { *dev_ptr = p.0 as *mut c_void };
                CUDART_SUCCESS
            }
            Err(e) => s.fail(RuntimeStatus::from(&e).code()),
        },
    )
}

#[no_mangle]
pub extern "C" fn cudaFree(dev_ptr: *mut c_void) -> i32 {
    if dev_ptr.is_null() {
        return CUDART_SUCCESS; // cudaFree(NULL) is a valid no-op.
    }
    ShimState::with(|s| {
        match allocate::mem_free(&mut s.ctx, &mut s.sink, DevicePtr(dev_ptr as u64)) {
            Ok(()) => CUDART_SUCCESS,
            Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
        }
    })
}

#[no_mangle]
pub extern "C" fn cudaMemcpy(dst: *mut c_void, src: *const c_void, count: usize, kind: i32) -> i32 {
    ShimState::with(|s| memcpy_impl(s, dst, src, count, kind))
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
    ShimState::with(|s| memcpy_impl(s, dst, src, count, kind))
}

fn memcpy_impl(
    s: &mut crate::state::State,
    dst: *mut c_void,
    src: *const c_void,
    count: usize,
    kind: i32,
) -> i32 {
    match kind {
        MEMCPY_HOST_TO_DEVICE => {
            let host = unsafe { CInput::bytes(src, count) };
            match transfer::memcpy_htod(&mut s.ctx, &mut s.sink, DevicePtr(dst as u64), host) {
                Ok(()) => CUDART_SUCCESS,
                Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
            }
        }
        MEMCPY_DEVICE_TO_DEVICE => {
            match transfer::memcpy_dtod(
                &mut s.ctx,
                &mut s.sink,
                DevicePtr(dst as u64),
                DevicePtr(src as u64),
                count as u64,
            ) {
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
                            std::ptr::copy_nonoverlapping(
                                bytes.as_ptr(),
                                dst as *mut u8,
                                bytes.len(),
                            )
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
    ShimState::with(|s| memset_impl(s, dev_ptr, value, count))
}

#[no_mangle]
pub extern "C" fn cudaMemsetAsync(
    dev_ptr: *mut c_void,
    value: i32,
    count: usize,
    _stream: *mut c_void,
) -> i32 {
    ShimState::with(|s| memset_impl(s, dev_ptr, value, count))
}

fn memset_impl(s: &mut crate::state::State, dev_ptr: *mut c_void, value: i32, count: usize) -> i32 {
    if count == 0 {
        return CUDART_SUCCESS;
    }
    // Lower through the bounded [`transfer::memset_elements`] (width = 1, the byte fill `cudaMemset`
    // specifies) rather than building the fill `vec![value; count]` here: that expansion bounds `count`
    // (checked, against the destination allocation) BEFORE allocating a single byte, so a hostile
    // `count` (e.g. near `usize::MAX`) returns a truthful `cudaErrorInvalidValue` instead of driving an
    // unbounded multi-GiB host allocation → OOM-abort.
    match transfer::memset_elements(
        &mut s.ctx,
        &mut s.sink,
        DevicePtr(dev_ptr as u64),
        value as u8 as u64,
        1,
        count,
    ) {
        Ok(()) => CUDART_SUCCESS,
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    }
}

#[no_mangle]
pub extern "C" fn cudaMemGetInfo(free_b: *mut usize, total_b: *mut usize) -> i32 {
    if free_b.is_null() || total_b.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    ShimState::with(|s| {
        // free = advertised VRAM minus the sum of every live device allocation (never underflows).
        let (free, total) = s.mem_info();
        unsafe {
            *total_b = total;
            *free_b = free;
        }
    });
    CUDART_SUCCESS
}

// ==================================================================================================
// host (pinned / mapped) memory + managed (unified) memory
// ==================================================================================================

/// `cudaMallocManaged(devPtr, size, flags)` — a managed (unified) device allocation: the same
/// `CreateBuffer` IR as `cudaMalloc`, flagged managed in the model. The (attach-global/host) flags do not
/// change the modeled semantics.
#[no_mangle]
pub extern "C" fn cudaMallocManaged(dev_ptr: *mut *mut c_void, size: usize, _flags: u32) -> i32 {
    if dev_ptr.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    ShimState::with(
        |s| match allocate::mem_alloc_managed(&mut s.ctx, &mut s.sink, size as u64) {
            Ok(p) => {
                unsafe { *dev_ptr = p.0 as *mut c_void };
                CUDART_SUCCESS
            }
            Err(e) => s.fail(RuntimeStatus::from(&e).code()),
        },
    )
}

/// `cudaMallocHost(ptr, size)` — a page-locked host allocation. Hands back the base of a real host buffer
/// the model owns (usable directly as a `cudaMemcpy` host source/destination).
#[no_mangle]
pub extern "C" fn cudaMallocHost(ptr: *mut *mut c_void, size: usize) -> i32 {
    if ptr.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    match ShimState::with(|s| s.ctx.host_alloc(size)) {
        Some(base) => {
            unsafe { *ptr = base as *mut c_void };
            CUDART_SUCCESS
        }
        None => ShimState::with(|s| s.fail(CUDART_ERROR_MEMORY_ALLOCATION)),
    }
}

/// `cudaHostAlloc(ptr, size, flags)` — the flagged pinned-allocation form; the modeled semantics do not
/// depend on the (portable / mapped / write-combined) flags, so it shares `cudaMallocHost`'s body.
#[no_mangle]
pub extern "C" fn cudaHostAlloc(ptr: *mut *mut c_void, size: usize, _flags: u32) -> i32 {
    cudaMallocHost(ptr, size)
}

/// `cudaFreeHost(ptr)` — free a pinned allocation. `cudaFreeHost(NULL)` is a valid no-op; a bogus /
/// already-freed pointer is `cudaErrorInvalidValue`.
#[no_mangle]
pub extern "C" fn cudaFreeHost(ptr: *mut c_void) -> i32 {
    if ptr.is_null() {
        return CUDART_SUCCESS;
    }
    ShimState::with(|s| match s.ctx.host_free(ptr as u64) {
        Ok(()) => CUDART_SUCCESS,
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    })
}

/// `cudaHostGetDevicePointer(pDevice, pHost, flags)` — the device pointer that maps a host allocation
/// `pHost` (lazily creating its backing device buffer). A pointer that is not a live host allocation is
/// `cudaErrorInvalidValue`.
#[no_mangle]
pub extern "C" fn cudaHostGetDevicePointer(
    p_device: *mut *mut c_void,
    p_host: *mut c_void,
    _flags: u32,
) -> i32 {
    if p_device.is_null() || p_host.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    ShimState::with(
        |s| match s.ctx.host_get_device_pointer(&mut s.sink, p_host as u64) {
            Ok(ptr) => {
                unsafe { *p_device = ptr.0 as *mut c_void };
                CUDART_SUCCESS
            }
            Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
        },
    )
}
