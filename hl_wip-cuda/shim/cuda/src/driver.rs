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
// The whole CUDA result/enum contract (result codes, the `CUdevice_attribute` + `CUpointer_attribute`
// value sets, `CTX_API_VERSION`, `CU_MEMORYTYPE_DEVICE`) — the query/context entry points map across
// most of it, so a glob keeps the (already exhaustive) list from being restated here.
use hl_cuda::result::*;
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
pub extern "C" fn cuDeviceGetAttribute(pi: *mut i32, attrib: i32, dev: i32) -> i32 {
    if pi.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // The full modeled CU_DEVICE_ATTRIBUTE_* set: values that vary with the device descriptor read it
    // (compute capability, warp size, SM count, clock); the rest are the fixed, truthful properties of
    // the simulated Ampere-class unified-memory device. The unmodeled attribute tail reports 0, which is
    // the spec-faithful "feature absent" answer a real driver gives for an attribute it doesn't set.
    let v = with(|s| {
        let d = &s.ctx.device;
        match attrib {
            CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK => d.max_threads_per_block as i32,
            CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X => 1024,
            CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y => 1024,
            CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z => 64,
            CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X => 2147483647,
            CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y => 65535,
            CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z => 65535,
            CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK => 49152,
            CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY => 65536,
            CU_DEVICE_ATTRIBUTE_WARP_SIZE => d.warp_size as i32,
            CU_DEVICE_ATTRIBUTE_MAX_PITCH => 2147483647,
            CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK => 65536,
            CU_DEVICE_ATTRIBUTE_CLOCK_RATE => d.clock_khz as i32,
            CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT => 512,
            CU_DEVICE_ATTRIBUTE_GPU_OVERLAP => 1,
            CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT => d.multiprocessor_count as i32,
            CU_DEVICE_ATTRIBUTE_KERNEL_EXEC_TIMEOUT => 0,
            CU_DEVICE_ATTRIBUTE_INTEGRATED => 1, // unified memory on the host
            CU_DEVICE_ATTRIBUTE_CAN_MAP_HOST_MEMORY => 1,
            CU_DEVICE_ATTRIBUTE_COMPUTE_MODE => 0, // DEFAULT
            CU_DEVICE_ATTRIBUTE_MAXIMUM_TEXTURE1D_WIDTH => 131072,
            CU_DEVICE_ATTRIBUTE_CONCURRENT_KERNELS => 1,
            CU_DEVICE_ATTRIBUTE_ECC_ENABLED => 0,
            CU_DEVICE_ATTRIBUTE_PCI_BUS_ID => 0,
            CU_DEVICE_ATTRIBUTE_PCI_DEVICE_ID => 0,
            CU_DEVICE_ATTRIBUTE_TCC_DRIVER => 0,
            CU_DEVICE_ATTRIBUTE_MEMORY_CLOCK_RATE => 6251000,
            CU_DEVICE_ATTRIBUTE_GLOBAL_MEMORY_BUS_WIDTH => 256,
            CU_DEVICE_ATTRIBUTE_L2_CACHE_SIZE => 4194304,
            CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_MULTIPROCESSOR => 2048,
            CU_DEVICE_ATTRIBUTE_ASYNC_ENGINE_COUNT => 2,
            CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING => 1,
            CU_DEVICE_ATTRIBUTE_PCI_DOMAIN_ID => 0,
            CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR => d.compute_capability.0 as i32,
            CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR => d.compute_capability.1 as i32,
            CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR => 102400,
            CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY => 1,
            CU_DEVICE_ATTRIBUTE_MULTI_GPU_BOARD => 0,
            CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS => 1,
            CU_DEVICE_ATTRIBUTE_COMPUTE_PREEMPTION_SUPPORTED => 1,
            CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN => 101376,
            CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS => 1,
            CU_DEVICE_ATTRIBUTE_DIRECT_MANAGED_MEM_ACCESS_FROM_HOST => 1,
            CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED => 0, // pools are unsupported
            _ => 0, // spec-faithful default for the unmodeled attribute tail
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
pub extern "C" fn cuCtxCreate_v2(pctx: *mut *mut c_void, flags: u32, dev: i32) -> i32 {
    if pctx.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = with(|s| s.create_ctx_with_flags(flags));
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
// context management: push/pop stack, api version, flags
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuCtxPushCurrent_v2(ctx: *mut c_void) -> i32 {
    if ctx.is_null() {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    with(|s| s.push_current_ctx(ctx));
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxPopCurrent_v2(pctx: *mut *mut c_void) -> i32 {
    let popped = with(|s| s.pop_current_ctx());
    if !pctx.is_null() {
        unsafe { *pctx = popped };
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetApiVersion(_ctx: *mut c_void, version: *mut u32) -> i32 {
    if version.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *version = CTX_API_VERSION };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetFlags(flags: *mut u32) -> i32 {
    if flags.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let f = with(|s| s.current_ctx_flags());
    unsafe { *flags = f };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetFlags(flags: u32) -> i32 {
    with(|s| s.set_current_ctx_flags(flags));
    CUDA_SUCCESS
}

// ==================================================================================================
// primary context (device 0): retain/release/reset ref-counting + state
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxRetain(pctx: *mut *mut c_void, dev: i32) -> i32 {
    if pctx.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = with(|s| s.primary_ctx_retain());
    unsafe { *pctx = token };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxRelease_v2(dev: i32) -> i32 {
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    with(|s| s.primary_ctx_release());
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxReset_v2(dev: i32) -> i32 {
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    with(|s| s.primary_ctx_reset());
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxGetState(dev: i32, flags: *mut u32, active: *mut i32) -> i32 {
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    let (f, a) = with(|s| s.primary_ctx_state());
    if !flags.is_null() {
        unsafe { *flags = f };
    }
    if !active.is_null() {
        unsafe { *active = a };
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxSetFlags_v2(dev: i32, flags: u32) -> i32 {
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    with(|s| s.set_primary_ctx_flags(flags));
    CUDA_SUCCESS
}

// ==================================================================================================
// memory info + pointer attributes (report what the allocation table actually knows)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuMemGetInfo_v2(free_out: *mut usize, total_out: *mut usize) -> i32 {
    let (free, total) = with(|s| s.mem_info());
    if !free_out.is_null() {
        unsafe { *free_out = free };
    }
    if !total_out.is_null() {
        unsafe { *total_out = total };
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemGetAddressRange_v2(pbase: *mut u64, psize: *mut usize, dptr: u64) -> i32 {
    match with(|s| s.ctx.mem.containing(DevicePtr(dptr))) {
        Some((base, size)) => {
            if !pbase.is_null() {
                unsafe { *pbase = base };
            }
            if !psize.is_null() {
                unsafe { *psize = size as usize };
            }
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_VALUE, // not a live device allocation
    }
}

/// Fill one pointer attribute into `data`. Every allocation the modeled driver hands out is device
/// memory in the unified VA (there is no managed/host alloc path yet), so the memory-type / managed /
/// ordinal answers are truthful for what the model knows. An attribute we cannot honestly answer for a
/// pointer that is not a live allocation returns `CUDA_ERROR_INVALID_VALUE`.
///
/// # Safety
/// `data` must point at a writable buffer large enough for `attr`'s value type.
unsafe fn pointer_attr(attr: i32, data: *mut c_void, ptr: u64) -> i32 {
    if data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let (found, base, size, cur_ctx) = with(|s| {
        let m = s.ctx.mem.containing(DevicePtr(ptr));
        (m.is_some(), m.map(|x| x.0).unwrap_or(0), m.map(|x| x.1).unwrap_or(0), s.current_ctx() as usize)
    });
    match attr {
        CU_POINTER_ATTRIBUTE_CONTEXT => *(data as *mut usize) = cur_ctx,
        CU_POINTER_ATTRIBUTE_MEMORY_TYPE => {
            if !found {
                return CUDA_ERROR_INVALID_VALUE;
            }
            *(data as *mut u32) = CU_MEMORYTYPE_DEVICE;
        }
        CU_POINTER_ATTRIBUTE_DEVICE_POINTER => {
            if !found {
                return CUDA_ERROR_INVALID_VALUE;
            }
            *(data as *mut u64) = ptr;
        }
        CU_POINTER_ATTRIBUTE_HOST_POINTER => *(data as *mut *mut c_void) = core::ptr::null_mut(),
        CU_POINTER_ATTRIBUTE_IS_MANAGED => *(data as *mut u32) = 0, // no managed-alloc path modeled
        CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL => *(data as *mut i32) = 0,
        CU_POINTER_ATTRIBUTE_BUFFER_ID => *(data as *mut u64) = base,
        CU_POINTER_ATTRIBUTE_SYNC_MEMOPS => *(data as *mut i32) = 1,
        CU_POINTER_ATTRIBUTE_MAPPED => *(data as *mut i32) = found as i32,
        CU_POINTER_ATTRIBUTE_RANGE_START_ADDR => *(data as *mut u64) = base,
        CU_POINTER_ATTRIBUTE_RANGE_SIZE => *(data as *mut usize) = size as usize,
        _ => return CUDA_ERROR_NOT_SUPPORTED,
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuPointerGetAttribute(data: *mut c_void, attr: i32, ptr: u64) -> i32 {
    if data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { pointer_attr(attr, data, ptr) }
}

#[no_mangle]
pub extern "C" fn cuPointerGetAttributes(
    n: u32,
    attrs: *mut i32,
    data: *mut *mut c_void,
    ptr: u64,
) -> i32 {
    if attrs.is_null() || data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    for i in 0..n as usize {
        let attr = unsafe { *attrs.add(i) };
        let slot = unsafe { *data.add(i) };
        let r = unsafe { pointer_attr(attr, slot, ptr) };
        // An unsupported attribute is skipped (its slot is left untouched), matching the driver's
        // batch semantics; any hard error aborts the batch.
        if r != CUDA_SUCCESS && r != CUDA_ERROR_NOT_SUPPORTED {
            return r;
        }
    }
    CUDA_SUCCESS
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

#[no_mangle]
pub extern "C" fn cuEventRecordWithFlags(hevent: *mut c_void, hstream: *mut c_void, _flags: u32) -> i32 {
    cuEventRecord(hevent, hstream)
}

/// `cuEventQuery` — with the synchronous executor a recorded event is already complete
/// (`CUDA_SUCCESS`); a valid-but-unrecorded event is `CUDA_ERROR_NOT_READY`; an unknown handle is
/// `CUDA_ERROR_INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuEventQuery(hevent: *mut c_void) -> i32 {
    with(|s| {
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
    with(|s| {
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
    with(|s| if s.stream(hstream).is_some() { CUDA_SUCCESS } else { CUDA_ERROR_INVALID_HANDLE })
}

/// `cuStreamWaitEvent` — make `hstream` wait on `hevent`. With a synchronous executor the awaited work
/// has already completed, so this validates both handles and returns success.
#[no_mangle]
pub extern "C" fn cuStreamWaitEvent(hstream: *mut c_void, hevent: *mut c_void, _flags: u32) -> i32 {
    with(|s| {
        if s.stream(hstream).is_none() || !s.event_is_valid(hevent) {
            CUDA_ERROR_INVALID_HANDLE
        } else {
            CUDA_SUCCESS
        }
    })
}

// ==================================================================================================
// unit tests for the query/context/pointer entry points
// ==================================================================================================
//
// These call the `extern "C"` entry points directly and assert the real returned values. They touch
// only the sink-free surface (device attributes, context/primary-context tokens, pointer metadata,
// event/stream query) — no GPU-exec socket is needed. The process-global shim state is shared, so a
// single serializing lock + `state::reset()` makes each test deterministic.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{reset, with};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Serialize the tests: they share one process-global `State`, so they must not run concurrently.
    fn guard() -> MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        let g = L.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner());
        reset();
        g
    }

    /// Record a live device allocation of `size` bytes directly in the model (no sink), returning its
    /// device pointer — the stand-in for a completed `cuMemAlloc`.
    fn record_alloc(size: u64) -> u64 {
        with(|s| {
            let b = s.ctx.alloc_buffer();
            s.ctx.mem.record(b, size).0
        })
    }

    #[test]
    fn device_attribute_reports_configured_and_fixed_values() {
        let _g = guard();
        let want = with(|s| s.ctx.device.clone());
        let mut v = -1i32;
        let get = |attr: i32, out: &mut i32| cuDeviceGetAttribute(out as *mut i32, attr, 0);

        assert_eq!(get(CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR, &mut v), CUDA_SUCCESS);
        assert_eq!(v, want.compute_capability.0 as i32);
        assert_eq!(get(CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR, &mut v), CUDA_SUCCESS);
        assert_eq!(v, want.compute_capability.1 as i32);
        assert_eq!(get(CU_DEVICE_ATTRIBUTE_WARP_SIZE, &mut v), CUDA_SUCCESS);
        assert_eq!(v, want.warp_size as i32);
        assert_eq!(get(CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT, &mut v), CUDA_SUCCESS);
        assert_eq!(v, want.multiprocessor_count as i32);
        assert_eq!(get(CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK, &mut v), CUDA_SUCCESS);
        assert_eq!(v, want.max_threads_per_block as i32);
        // A fixed, truthful property of the modeled unified-memory device.
        assert_eq!(get(CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING, &mut v), CUDA_SUCCESS);
        assert_eq!(v, 1);
        // A bad device ordinal is rejected.
        assert_eq!(cuDeviceGetAttribute(&mut v as *mut i32, CU_DEVICE_ATTRIBUTE_WARP_SIZE, 1),
                   CUDA_ERROR_INVALID_VALUE);
        // A null out-pointer is rejected.
        assert_eq!(cuDeviceGetAttribute(core::ptr::null_mut(), CU_DEVICE_ATTRIBUTE_WARP_SIZE, 0),
                   CUDA_ERROR_INVALID_VALUE);
    }

    #[test]
    fn ctx_push_pop_round_trips() {
        let _g = guard();
        let mut a: *mut c_void = core::ptr::null_mut();
        let mut b: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuCtxCreate_v2(&mut a, 0, 0), CUDA_SUCCESS);
        assert_eq!(cuCtxCreate_v2(&mut b, 0, 0), CUDA_SUCCESS);
        assert_ne!(a, b);

        // After creating `b`, it is current.
        let mut cur: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
        assert_eq!(cur, b);

        // Push `a`; it becomes current. Pop restores `b` and hands back `a`.
        assert_eq!(cuCtxPushCurrent_v2(a), CUDA_SUCCESS);
        assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
        assert_eq!(cur, a);
        let mut popped: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuCtxPopCurrent_v2(&mut popped), CUDA_SUCCESS);
        assert_eq!(popped, a);
        assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
        assert_eq!(cur, b);

        // A null context handle can't be pushed.
        assert_eq!(cuCtxPushCurrent_v2(core::ptr::null_mut()), CUDA_ERROR_INVALID_HANDLE);
    }

    #[test]
    fn ctx_api_version_and_flags() {
        let _g = guard();
        let mut ctx: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuCtxCreate_v2(&mut ctx, 7, 0), CUDA_SUCCESS);
        let mut ver = 0u32;
        assert_eq!(cuCtxGetApiVersion(ctx, &mut ver), CUDA_SUCCESS);
        assert_eq!(ver, CTX_API_VERSION);
        // Flags recorded at create are readable; SetFlags updates them.
        let mut flags = 0u32;
        assert_eq!(cuCtxGetFlags(&mut flags), CUDA_SUCCESS);
        assert_eq!(flags, 7);
        assert_eq!(cuCtxSetFlags(3), CUDA_SUCCESS);
        assert_eq!(cuCtxGetFlags(&mut flags), CUDA_SUCCESS);
        assert_eq!(flags, 3);
    }

    #[test]
    fn primary_ctx_retain_release_refcounts() {
        let _g = guard();
        let mut p1: *mut c_void = core::ptr::null_mut();
        let mut p2: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuDevicePrimaryCtxRetain(&mut p1, 0), CUDA_SUCCESS);
        assert_eq!(cuDevicePrimaryCtxRetain(&mut p2, 0), CUDA_SUCCESS);
        assert_eq!(p1, p2, "the single device has one primary context");
        assert!(!p1.is_null());

        let mut active = -1i32;
        let mut flags = 0u32;
        assert_eq!(cuDevicePrimaryCtxGetState(0, &mut flags, &mut active), CUDA_SUCCESS);
        assert_eq!(active, 1, "active while a reference is held");

        // Two retains → two releases before it goes inactive.
        assert_eq!(cuDevicePrimaryCtxRelease_v2(0), CUDA_SUCCESS);
        assert_eq!(cuDevicePrimaryCtxGetState(0, &mut flags, &mut active), CUDA_SUCCESS);
        assert_eq!(active, 1);
        assert_eq!(cuDevicePrimaryCtxRelease_v2(0), CUDA_SUCCESS);
        assert_eq!(cuDevicePrimaryCtxGetState(0, &mut flags, &mut active), CUDA_SUCCESS);
        assert_eq!(active, 0, "last release deactivates the primary context");

        // A bad device ordinal is rejected.
        assert_eq!(cuDevicePrimaryCtxRelease_v2(1), CUDA_ERROR_INVALID_DEVICE);
    }

    #[test]
    fn mem_get_info_reflects_allocations() {
        let _g = guard();
        let (mut free0, mut total0) = (0usize, 0usize);
        assert_eq!(cuMemGetInfo_v2(&mut free0, &mut total0), CUDA_SUCCESS);
        assert_eq!(free0, total0, "no allocations → all memory free");

        record_alloc(1 << 20); // 1 MiB
        let (mut free1, mut total1) = (0usize, 0usize);
        assert_eq!(cuMemGetInfo_v2(&mut free1, &mut total1), CUDA_SUCCESS);
        assert_eq!(total1, total0, "total VRAM is fixed");
        assert_eq!(free0 - free1, 1 << 20, "free dropped by exactly the allocation size");
    }

    #[test]
    fn pointer_get_attribute_reports_device_memory() {
        let _g = guard();
        let ptr = record_alloc(4096);

        // MEMORY_TYPE of a live allocation is DEVICE.
        let mut mtype = 0u32;
        assert_eq!(
            cuPointerGetAttribute(&mut mtype as *mut u32 as *mut c_void, CU_POINTER_ATTRIBUTE_MEMORY_TYPE, ptr),
            CUDA_SUCCESS
        );
        assert_eq!(mtype, CU_MEMORYTYPE_DEVICE);

        // RANGE_START_ADDR / RANGE_SIZE report the allocation base + size (query mid-range).
        let mut start = 0u64;
        let mut size = 0usize;
        assert_eq!(
            cuPointerGetAttribute(&mut start as *mut u64 as *mut c_void, CU_POINTER_ATTRIBUTE_RANGE_START_ADDR, ptr + 8),
            CUDA_SUCCESS
        );
        assert_eq!(start, ptr);
        assert_eq!(
            cuPointerGetAttribute(&mut size as *mut usize as *mut c_void, CU_POINTER_ATTRIBUTE_RANGE_SIZE, ptr + 8),
            CUDA_SUCCESS
        );
        assert_eq!(size, 4096);

        // IS_MANAGED is false (no managed-alloc path modeled).
        let mut managed = 9u32;
        assert_eq!(
            cuPointerGetAttribute(&mut managed as *mut u32 as *mut c_void, CU_POINTER_ATTRIBUTE_IS_MANAGED, ptr),
            CUDA_SUCCESS
        );
        assert_eq!(managed, 0);

        // An unknown pointer can't honestly report a memory type.
        let mut junk = 0u32;
        assert_eq!(
            cuPointerGetAttribute(&mut junk as *mut u32 as *mut c_void, CU_POINTER_ATTRIBUTE_MEMORY_TYPE, 0xdead_beef),
            CUDA_ERROR_INVALID_VALUE
        );

        // cuMemGetAddressRange resolves the same base/size.
        let mut base = 0u64;
        let mut rsize = 0usize;
        assert_eq!(cuMemGetAddressRange_v2(&mut base, &mut rsize, ptr + 16), CUDA_SUCCESS);
        assert_eq!((base, rsize), (ptr, 4096));
        assert_eq!(cuMemGetAddressRange_v2(&mut base, &mut rsize, 0xdead_beef), CUDA_ERROR_INVALID_VALUE);
    }

    #[test]
    fn event_query_and_elapsed_time() {
        let _g = guard();
        let mut start: *mut c_void = core::ptr::null_mut();
        let mut end: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuEventCreate(&mut start, 0), CUDA_SUCCESS);
        assert_eq!(cuEventCreate(&mut end, 0), CUDA_SUCCESS);

        // Unrecorded → NOT_READY.
        assert_eq!(cuEventQuery(start), CUDA_ERROR_NOT_READY);
        // Unknown handle → INVALID_HANDLE.
        assert_eq!(cuEventQuery(0x1234 as *mut c_void), CUDA_ERROR_INVALID_HANDLE);

        // Record both; a recorded event queries ready and elapsed time is finite and non-negative.
        assert_eq!(cuEventRecord(start, core::ptr::null_mut()), CUDA_SUCCESS);
        assert_eq!(cuEventRecordWithFlags(end, core::ptr::null_mut(), 0), CUDA_SUCCESS);
        assert_eq!(cuEventQuery(start), CUDA_SUCCESS);
        assert_eq!(cuEventQuery(end), CUDA_SUCCESS);
        let mut ms = -1.0f32;
        assert_eq!(cuEventElapsedTime(&mut ms, start, end), CUDA_SUCCESS);
        assert!(ms >= 0.0 && ms.is_finite(), "elapsed ms was {ms}");
    }

    #[test]
    fn stream_query_and_wait_event() {
        let _g = guard();
        // The default (null) stream is always valid and ready.
        assert_eq!(cuStreamQuery(core::ptr::null_mut()), CUDA_SUCCESS);

        let mut stream: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuStreamCreate(&mut stream, 0), CUDA_SUCCESS);
        assert_eq!(cuStreamQuery(stream), CUDA_SUCCESS);
        // An unknown stream handle is rejected.
        assert_eq!(cuStreamQuery(0x9999 as *mut c_void), CUDA_ERROR_INVALID_HANDLE);

        let mut ev: *mut c_void = core::ptr::null_mut();
        assert_eq!(cuEventCreate(&mut ev, 0), CUDA_SUCCESS);
        assert_eq!(cuStreamWaitEvent(stream, ev, 0), CUDA_SUCCESS);
        assert_eq!(cuStreamWaitEvent(stream, 0x9999 as *mut c_void, 0), CUDA_ERROR_INVALID_HANDLE);
    }
}
