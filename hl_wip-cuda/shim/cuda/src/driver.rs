//! The hand-written `cu*` entry points: marshal the CUDA Driver API C ABI into the `hl_cuda` lowering
//! services and submit through the process-global [`crate::state`] sink.
//!
//! Two groups: **bring-up** (init / driver-version / error strings / single-device presence / context
//! basics) that returns real, sane values so a dlopen + probe accepts the device, and the **IR-wired**
//! compute set (memory alloc/copy, PTX module load, kernel launch, stream/event sync) that calls the
//! shared `hl_cuda::service` functions — the SAME lowering the in-process end-to-end test exercises.
//!
//! Every body is panic-free across the C-ABI seam: raw pointers are null-checked, and a lowering
//! [`hl_gpu::GpuError`] is mapped to the accurate `CUresult` via [`hl_cuda::result`] (never a false
//! `CUDA_SUCCESS`). The crate builds with `panic = "abort"` as a belt-and-braces second guarantee.

use core::ffi::{c_char, c_void};

use hl_cuda::adapter::ptx;
use hl_cuda::model::device::DevicePtr;
use hl_cuda::result::{
    cu_result_from_gpu_error, CUDA_ERROR_INVALID_HANDLE, CUDA_ERROR_INVALID_VALUE,
    CUDA_ERROR_NOT_SUPPORTED, CUDA_SUCCESS, DRIVER_VERSION,
};
use hl_cuda::service::{allocate, launch, load_module, synchronize, transfer};
use hl_cuda::KernelArg;

use crate::state::with;

// ---- small C-ABI marshalling helpers -------------------------------------------------------------

/// Borrow a `const void*` + length as a byte slice (empty if null / zero-length).
unsafe fn bytes<'a>(p: *const c_void, n: usize) -> &'a [u8] {
    if p.is_null() || n == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p as *const u8, n)
    }
}

/// Read a nul-terminated C string into an owned `Vec<u8>` (without the nul). `None` if `p` is null.
unsafe fn cstr_bytes(p: *const c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(p).to_bytes().to_vec())
}

/// Write `s` (with a trailing nul) into the caller's `dst[..len]` buffer, truncating to fit.
unsafe fn write_cstr(dst: *mut c_char, len: i32, s: &str) {
    if dst.is_null() || len <= 0 {
        return;
    }
    let cap = (len as usize).saturating_sub(1);
    let n = s.len().min(cap);
    std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, n);
    *dst.add(n) = 0;
}

// ==================================================================================================
// bring-up
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuInit(_flags: u32) -> i32 {
    with(|s| s.inited = true);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDriverGetVersion(version: *mut i32) -> i32 {
    if version.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *version = DRIVER_VERSION };
    CUDA_SUCCESS
}

/// Static, nul-terminated error strings for `cuGetErrorString`/`cuGetErrorName`.
fn error_text(code: i32, name: bool) -> &'static [u8] {
    match (code, name) {
        (0, false) => b"no error\0",
        (0, true) => b"CUDA_SUCCESS\0",
        (1, _) => b"CUDA_ERROR_INVALID_VALUE\0",
        (2, _) => b"CUDA_ERROR_OUT_OF_MEMORY\0",
        (3, _) => b"CUDA_ERROR_NOT_INITIALIZED\0",
        (400, _) => b"CUDA_ERROR_INVALID_HANDLE\0",
        (500, _) => b"CUDA_ERROR_NOT_FOUND\0",
        (801, _) => b"CUDA_ERROR_NOT_SUPPORTED\0",
        (_, true) => b"CUDA_ERROR_UNKNOWN\0",
        (_, false) => b"unknown error\0",
    }
}

#[no_mangle]
pub extern "C" fn cuGetErrorString(error: i32, str_: *mut *const c_char) -> i32 {
    if str_.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *str_ = error_text(error, false).as_ptr() as *const c_char };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuGetErrorName(error: i32, str_: *mut *const c_char) -> i32 {
    if str_.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *str_ = error_text(error, true).as_ptr() as *const c_char };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetCount(count: *mut i32) -> i32 {
    if count.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *count = 1 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGet(device: *mut i32, ordinal: i32) -> i32 {
    if device.is_null() || ordinal != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *device = 0 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetName(name: *mut c_char, len: i32, _dev: i32) -> i32 {
    with(|s| unsafe { write_cstr(name, len, &s.ctx.device.name) });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceTotalMem_v2(bytes_out: *mut usize, _dev: i32) -> i32 {
    if bytes_out.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    with(|s| unsafe { *bytes_out = s.ctx.device.total_mem as usize });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetAttribute(pi: *mut i32, attrib: i32, _dev: i32) -> i32 {
    if pi.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // A benign, truthful subset of the common CU_DEVICE_ATTRIBUTE_* values; unknowns report 0.
    let v = with(|s| {
        let d = &s.ctx.device;
        match attrib {
            1 => d.max_threads_per_block as i32,        // MAX_THREADS_PER_BLOCK
            10 => d.warp_size as i32,                    // WARP_SIZE
            16 => d.multiprocessor_count as i32,         // MULTIPROCESSOR_COUNT
            13 => d.clock_khz as i32,                    // CLOCK_RATE
            75 => d.compute_capability.0 as i32,         // COMPUTE_CAPABILITY_MAJOR
            76 => d.compute_capability.1 as i32,         // COMPUTE_CAPABILITY_MINOR
            _ => 0,
        }
    });
    unsafe { *pi = v };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceComputeCapability(major: *mut i32, minor: *mut i32, _dev: i32) -> i32 {
    if major.is_null() || minor.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    with(|s| unsafe {
        *major = s.ctx.device.compute_capability.0 as i32;
        *minor = s.ctx.device.compute_capability.1 as i32;
    });
    CUDA_SUCCESS
}

unsafe fn write_uuid(uuid: *mut c_void) -> i32 {
    if uuid.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    with(|s| std::ptr::copy_nonoverlapping(s.ctx.device.uuid.as_ptr(), uuid as *mut u8, 16));
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetUuid(uuid: *mut c_void, _dev: i32) -> i32 {
    unsafe { write_uuid(uuid) }
}

#[no_mangle]
pub extern "C" fn cuDeviceGetUuid_v2(uuid: *mut c_void, _dev: i32) -> i32 {
    unsafe { write_uuid(uuid) }
}

// ==================================================================================================
// context basics
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuCtxCreate_v2(pctx: *mut *mut c_void, _flags: u32, _dev: i32) -> i32 {
    if pctx.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = with(|s| s.create_ctx());
    unsafe { *pctx = token };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxDestroy_v2(ctx: *mut c_void) -> i32 {
    with(|s| s.destroy_ctx(ctx));
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetCurrent(ctx: *mut c_void) -> i32 {
    with(|s| s.set_current_ctx(ctx));
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetCurrent(pctx: *mut *mut c_void) -> i32 {
    if pctx.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let cur = with(|s| s.current_ctx());
    unsafe { *pctx = cur };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetDevice(device: *mut i32) -> i32 {
    if device.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *device = 0 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSynchronize() -> i32 {
    with(|s| match synchronize::ctx_synchronize(&mut s.ctx, &mut s.sink) {
        Ok(()) => CUDA_SUCCESS,
        Err(e) => cu_result_from_gpu_error(&e),
    })
}

// ==================================================================================================
// IR-wired: memory
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuMemAlloc_v2(dptr: *mut u64, bytesize: usize) -> i32 {
    if dptr.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    with(|s| match allocate::mem_alloc(&mut s.ctx, &mut s.sink, bytesize as u64) {
        Ok(p) => {
            unsafe { *dptr = p.0 };
            CUDA_SUCCESS
        }
        Err(e) => cu_result_from_gpu_error(&e),
    })
}

#[no_mangle]
pub extern "C" fn cuMemFree_v2(dptr: u64) -> i32 {
    with(|s| match allocate::mem_free(&mut s.ctx, &mut s.sink, DevicePtr(dptr)) {
        Ok(()) => CUDA_SUCCESS,
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

#[no_mangle]
pub extern "C" fn cuMemcpyHtoD_v2(dst: u64, src: *const c_void, n: usize) -> i32 {
    let host = unsafe { bytes(src, n) };
    with(|s| match transfer::memcpy_htod(&mut s.ctx, &mut s.sink, DevicePtr(dst), host) {
        Ok(()) => CUDA_SUCCESS,
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

#[no_mangle]
pub extern "C" fn cuMemcpyDtoD_v2(dst: u64, src: u64, n: usize) -> i32 {
    with(|s| match transfer::memcpy_dtod(&mut s.ctx, &mut s.sink, DevicePtr(dst), DevicePtr(src), n as u64) {
        Ok(()) => CUDA_SUCCESS,
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

/// `cuMemcpyDtoH_v2` resolves the device source and reads `n` bytes back through the sink's device→host
/// readback path (`CommandSink::read_buffer`), copying them into the caller's host `dst`. A dangling source
/// or a failed readback → `CUDA_ERROR_INVALID_VALUE`.
#[no_mangle]
pub extern "C" fn cuMemcpyDtoH_v2(dst: *mut c_void, src: u64, n: usize) -> i32 {
    if dst.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    with(|s| match transfer::read_dtoh(&s.ctx, &mut s.sink, DevicePtr(src), n) {
        Ok(bytes) => {
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len()) };
            CUDA_SUCCESS
        }
        Err(_) => CUDA_ERROR_INVALID_VALUE,
    })
}

// ==================================================================================================
// IR-wired: module (PTX)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuModuleLoadData(module: *mut *mut c_void, image: *const c_void) -> i32 {
    if module.is_null() || image.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // The driver API hands `image` without a length; a PTX image is nul-terminated text, so read to nul.
    let Some(img) = (unsafe { cstr_bytes(image as *const c_char) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    with(|s| match load_module::module_load_data(&mut s.ctx, &img) {
        Ok(id) => {
            let h = s.intern_module(id);
            unsafe { *module = h };
            CUDA_SUCCESS
        }
        Err(e) => cu_result_from_gpu_error(&e),
    })
}

#[no_mangle]
pub extern "C" fn cuModuleGetFunction(hfunc: *mut *mut c_void, hmod: *mut c_void, name: *const c_char) -> i32 {
    if hfunc.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let Some(nm) = (unsafe { cstr_bytes(name) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let Ok(nm) = std::str::from_utf8(&nm) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    with(|s| {
        let Some(module_id) = s.module_id(hmod) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match load_module::module_get_function(&s.ctx, module_id, nm) {
            Ok(f) => {
                let h = s.intern_function(f, nm);
                unsafe { *hfunc = h };
                CUDA_SUCCESS
            }
            Err(e) => cu_result_from_gpu_error(&e),
        }
    })
}

// ==================================================================================================
// IR-wired: kernel launch
// ==================================================================================================

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn cuLaunchKernel(
    f: *mut c_void,
    gx: u32,
    gy: u32,
    gz: u32,
    bx: u32,
    by: u32,
    bz: u32,
    _shared_mem_bytes: u32,
    _stream: *mut c_void,
    kernel_params: *mut *mut c_void,
    _extra: *mut *mut c_void,
) -> i32 {
    with(|s| {
        let Some(func) = s.function(f) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let block = [bx, by, bz];
        // Recover the kernel's parameter layout (which args are pointers vs scalars, and each width) by
        // compiling the module's PTX with the launch block dims — the same front-end the executor uses.
        let Some((src, entry)) = s.ctx.entry_source(func) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let prog = match ptx::compile(&src, &entry, block) {
            Ok(p) => p,
            Err(e) => {
                crate::stub::unsupported("cuLaunchKernel", &format!("entry `{entry}`: {e:?}"));
                return cu_result_from_gpu_error(&e);
            }
        };
        if kernel_params.is_null() && !prog.params.is_empty() {
            // The `extra`-packed parameter form is not modeled; a kernel with params needs kernelParams.
            return CUDA_ERROR_NOT_SUPPORTED;
        }
        // Marshal each argument from its `kernelParams[i]` slot per the recovered layout.
        let mut args: Vec<KernelArg> = Vec::with_capacity(prog.params.len());
        for (i, p) in prog.params.iter().enumerate() {
            let slot = unsafe { *kernel_params.add(i) };
            if slot.is_null() {
                return CUDA_ERROR_INVALID_VALUE;
            }
            if p.is_ptr {
                let v = unsafe { std::ptr::read_unaligned(slot as *const u64) };
                args.push(KernelArg::Ptr(DevicePtr(v)));
            } else {
                let w = p.width as usize;
                let raw = unsafe { std::slice::from_raw_parts(slot as *const u8, w) };
                args.push(KernelArg::Scalar(raw.to_vec()));
            }
        }
        match launch::launch(&mut s.ctx, &mut s.sink, func, (gx, gy, gz), (bx, by, bz), &args) {
            Ok(_) => CUDA_SUCCESS,
            Err(e) => cu_result_from_gpu_error(&e),
        }
    })
}

// ==================================================================================================
// IR-wired: stream + event synchronization
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuStreamCreate(phstream: *mut *mut c_void, _flags: u32) -> i32 {
    if phstream.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let h = with(|s| {
        let stream = s.ctx.streams.create();
        s.intern_stream(stream)
    });
    unsafe { *phstream = h };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamDestroy_v2(hstream: *mut c_void) -> i32 {
    with(|s| match s.stream(hstream) {
        Some(st) => {
            s.ctx.streams.destroy(st);
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_HANDLE,
    })
}

#[no_mangle]
pub extern "C" fn cuStreamSynchronize(hstream: *mut c_void) -> i32 {
    with(|s| {
        let Some(st) = s.stream(hstream) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        match synchronize::stream_synchronize(&mut s.ctx, &mut s.sink, st) {
            Ok(()) => CUDA_SUCCESS,
            Err(e) => cu_result_from_gpu_error(&e),
        }
    })
}

#[no_mangle]
pub extern "C" fn cuEventCreate(phevent: *mut *mut c_void, _flags: u32) -> i32 {
    if phevent.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let h = with(|s| s.create_event());
    unsafe { *phevent = h };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuEventRecord(hevent: *mut c_void, _hstream: *mut c_void) -> i32 {
    if with(|s| s.record_event(hevent)) {
        CUDA_SUCCESS
    } else {
        CUDA_ERROR_INVALID_HANDLE
    }
}

#[no_mangle]
pub extern "C" fn cuEventSynchronize(hevent: *mut c_void) -> i32 {
    with(|s| {
        if !s.event_is_valid(hevent) {
            return CUDA_ERROR_INVALID_HANDLE;
        }
        // A recorded event completes when the context's prior work does; barrier the context.
        match synchronize::ctx_synchronize(&mut s.ctx, &mut s.sink) {
            Ok(()) => CUDA_SUCCESS,
            Err(e) => cu_result_from_gpu_error(&e),
        }
    })
}

#[no_mangle]
pub extern "C" fn cuEventDestroy_v2(hevent: *mut c_void) -> i32 {
    with(|s| if s.event_is_valid(hevent) { CUDA_SUCCESS } else { CUDA_ERROR_INVALID_HANDLE })
}
