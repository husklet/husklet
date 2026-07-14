//! The hand-written `cuda*` runtime entry points: marshal the CUDA Runtime API C ABI into the `hl_cuda`
//! lowering services through the process-global [`crate::state`] sink.
//!
//! Covers the memory + device + stream basics that map cleanly onto the services (alloc/free/memcpy/
//! memset/synchronize) plus the version/error/device queries a probe expects, AND the runtime's launch
//! path: `__cudaRegisterFatBinary`/`__cudaRegisterFunction`/`__cudaRegisterFatBinaryEnd` populate the
//! [`hl_cuda::service::register::Registry`] (fatbin handle → module, host-fn pointer → kernel) and
//! `cudaLaunchKernel` resolves a host-fn pointer to its device entry and lowers exactly like the
//! driver-API `cuLaunchKernel` (same `CreateShader{kernel}` + `ComputePipeline` + `BindGroup` + `Dispatch`).

use core::ffi::{c_char, c_void};

use hl_cuda::model::device::DevicePtr;
use hl_cuda::result::{
    cudart_from_gpu_error, CUDART_ERROR_INVALID_DEVICE, CUDART_ERROR_INVALID_VALUE,
    CUDART_ERROR_NOT_SUPPORTED, CUDART_SUCCESS,
};
use hl_cuda::service::register::{self, FatbinHandle};
use hl_cuda::service::{allocate, synchronize, transfer};

use crate::state::with;
use crate::Dim3;

// nvcc's `__fatBinC_Wrapper_t`: `{ int magic; int version; const void* data; void* filename_or_fatbins; }`.
// `__cudaRegisterFatBinary` receives a pointer to this; `data` points at the fatbin CONTAINER (which
// begins with the 0xba55ed50 fatbin magic). Clean-room from the documented layout.
const FATBIN_WRAPPER_MAGIC: u32 = 0x4662_43b1;
const FATBIN_MAGIC: u32 = 0xba55_ed50;

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

// ==================================================================================================
// runtime-API launch path: `__cudaRegister*` fatbin/function registry + `cudaLaunchKernel`
// ==================================================================================================

/// nvcc's `__fatBinC_Wrapper_t` — the argument `__cudaRegisterFatBinary` actually receives. `data` points
/// at the fatbin container (which itself begins with [`FATBIN_MAGIC`]).
#[repr(C)]
struct FatBinWrapper {
    magic: i32,
    version: i32,
    data: *const c_void,
    filename_or_fatbins: *const c_void,
}

/// Follow `fat_cubin` (a `__fatBinC_Wrapper_t*`, or defensively a bare container) to the fatbin CONTAINER
/// bytes, sized by the container's own `header_size + fat_size`. `None` for a null/foreign/short image.
unsafe fn container_bytes<'a>(fat_cubin: *const c_void) -> Option<&'a [u8]> {
    if fat_cubin.is_null() {
        return None;
    }
    let head = std::ptr::read_unaligned(fat_cubin as *const u32);
    let container: *const u8 = if head == FATBIN_WRAPPER_MAGIC {
        let w = &*(fat_cubin as *const FatBinWrapper);
        if w.data.is_null() {
            return None;
        }
        w.data as *const u8
    } else if head == FATBIN_MAGIC {
        fat_cubin as *const u8
    } else {
        return None;
    };
    if std::ptr::read_unaligned(container as *const u32) != FATBIN_MAGIC {
        return None;
    }
    let header_size = std::ptr::read_unaligned(container.add(6) as *const u16) as usize;
    let fat_size = std::ptr::read_unaligned(container.add(8) as *const u64) as usize;
    let total = header_size.checked_add(fat_size)?;
    Some(std::slice::from_raw_parts(container, total))
}

/// Read a nul-terminated C string into an owned `String` (`None` if null or not UTF-8).
unsafe fn cstr_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    std::ffi::CStr::from_ptr(p).to_str().ok().map(str::to_string)
}

/// Encode a [`FatbinHandle`] as the opaque `void**` nvcc round-trips back to us: a heap cell whose stored
/// `void*` value is the handle. [`decode_handle`] reads it back.
fn encode_handle(h: FatbinHandle) -> *mut *mut c_void {
    Box::into_raw(Box::new(h.0 as *mut c_void))
}
unsafe fn decode_handle(h: *mut *mut c_void) -> Option<FatbinHandle> {
    if h.is_null() {
        return None;
    }
    Some(FatbinHandle(*h as u64))
}

/// `__cudaRegisterFatBinary(fatCubin)` — walk the wrapped fatbin to its PTX, load it as a module, and hand
/// nvcc an opaque handle bound to that module. Returns null on a bad image (nvcc tolerates a null handle).
#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinary(fatCubin: *mut c_void) -> *mut *mut c_void {
    let Some(container) = (unsafe { container_bytes(fatCubin) }) else {
        return core::ptr::null_mut();
    };
    with(|s| match s.registry.register_fatbinary(&mut s.ctx, container) {
        Ok(handle) => encode_handle(handle),
        Err(e) => {
            s.fail(cudart_from_gpu_error(&e));
            core::ptr::null_mut()
        }
    })
}

/// `__cudaRegisterFunction(handle, hostFun, deviceFun, deviceName, …)` — bind the host function pointer
/// `hostFun` to the device entry `deviceName` in the handle's module. `deviceFun` + the launch-bound
/// descriptors are nvcc bookkeeping the lowering does not need.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn __cudaRegisterFunction(
    fatCubinHandle: *mut *mut c_void,
    hostFun: *const c_char,
    deviceFun: *mut c_char,
    deviceName: *const c_char,
    thread_limit: i32,
    tid: *mut c_void,
    bid: *mut c_void,
    bDim: *mut c_void,
    gDim: *mut c_void,
    wSize: *mut i32,
) {
    let _ = (deviceFun, thread_limit, tid, bid, bDim, gDim, wSize);
    let Some(handle) = (unsafe { decode_handle(fatCubinHandle) }) else {
        return;
    };
    let Some(name) = (unsafe { cstr_string(deviceName) }) else {
        return;
    };
    let host_fn = hostFun as usize;
    with(|s| {
        if let Err(e) = s.registry.register_function(&s.ctx, handle, host_fn, &name) {
            s.fail(cudart_from_gpu_error(&e));
        }
    });
}

/// `__cudaRegisterFatBinaryEnd(handle)` — the finalization marker after the last `__cudaRegisterFunction`.
#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinaryEnd(fatCubinHandle: *mut *mut c_void) {
    if let Some(handle) = unsafe { decode_handle(fatCubinHandle) } {
        with(|s| {
            s.registry.register_fatbinary_end(handle);
        });
    }
}

/// `cudaLaunchKernel(func, gridDim, blockDim, args, sharedMem, stream)` — resolve the host-fn pointer to
/// its registered device entry and lower exactly like the driver-API `cuLaunchKernel`, via the shared
/// [`register::launch_kernel`].
#[no_mangle]
pub extern "C" fn cudaLaunchKernel(
    func: *const c_void,
    gridDim: Dim3,
    blockDim: Dim3,
    args: *mut *mut c_void,
    _sharedMem: usize,
    _stream: *mut c_void,
) -> i32 {
    let host_fn = func as usize;
    let grid = (gridDim.x, gridDim.y, gridDim.z);
    let block = (blockDim.x, blockDim.y, blockDim.z);
    with(|s| {
        match unsafe {
            register::launch_kernel(
                &mut s.ctx,
                &mut s.sink,
                &s.registry,
                host_fn,
                grid,
                block,
                args as *const *const c_void,
            )
        } {
            Ok(()) => CUDART_SUCCESS,
            Err(e) => s.fail(cudart_from_gpu_error(&e)),
        }
    })
}
