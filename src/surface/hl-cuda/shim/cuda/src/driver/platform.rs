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

use super::*;

// ---- small C-ABI marshalling helpers -------------------------------------------------------------

/// Borrow a `const void*` + length as a byte slice (empty if null / zero-length).
pub(super) struct CInput;

impl CInput {
    pub(super) unsafe fn bytes<'a>(pointer: *const c_void, length: usize) -> &'a [u8] {
        if pointer.is_null() || length == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(pointer as *const u8, length)
        }
    }

    /// Read a nul-terminated C string into an owned `Vec<u8>` (without the nul).
    pub(super) unsafe fn string(pointer: *const c_char) -> Option<Vec<u8>> {
        if pointer.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(pointer).to_bytes().to_vec())
    }
}

/// Write `s` (with a trailing nul) into the caller's `dst[..len]` buffer, truncating to fit.
pub(super) unsafe fn write_cstr(dst: *mut c_char, len: i32, s: &str) {
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

/// `cuInit(flags)` — initialize the driver for the calling process. Every entry point that lowers IR is
/// gated on this having run (`CUDA_ERROR_NOT_INITIALIZED` otherwise), which is also how a `fork(2)` child
/// is refused: its disowned state has never been initialized, so it must call `cuInit` itself — and then
/// gets a fresh context and its own `$HL_GPU_EXEC` connection, never the parent's.
#[no_mangle]
pub extern "C" fn cuInit(flags: u32) -> i32 {
    // `cuInit` defines no flags; a non-zero value is `CUDA_ERROR_INVALID_VALUE`.
    if flags != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|s| s.inited = true);
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
struct CudaError;

impl CudaError {
    fn text(code: i32, name: bool) -> &'static [u8] {
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
}

#[no_mangle]
pub extern "C" fn cuGetErrorString(error: i32, str_: *mut *const c_char) -> i32 {
    if str_.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *str_ = CudaError::text(error, false).as_ptr() as *const c_char };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuGetErrorName(error: i32, str_: *mut *const c_char) -> i32 {
    if str_.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *str_ = CudaError::text(error, true).as_ptr() as *const c_char };
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
    ShimState::with(|s| unsafe { write_cstr(name, len, &s.ctx.device.name) });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceTotalMem_v2(bytes_out: *mut usize, _dev: i32) -> i32 {
    if bytes_out.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|s| unsafe { *bytes_out = s.ctx.device.total_mem as usize });
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
    let v = ShimState::with(|s| {
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
            // No grid-wide barrier in the kernel IR → cooperative launch is genuinely absent, and
            // `cuLaunchCooperativeKernel` / `cudaDeviceProp::cooperativeLaunch` must agree.
            CU_DEVICE_ATTRIBUTE_COOPERATIVE_LAUNCH => 0,
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
    ShimState::with(|s| unsafe {
        *major = s.ctx.device.compute_capability.0 as i32;
        *minor = s.ctx.device.compute_capability.1 as i32;
    });
    CUDA_SUCCESS
}

struct DeviceUuid;

impl DeviceUuid {
    unsafe fn write(uuid: *mut c_void) -> i32 {
        if uuid.is_null() {
            return CUDA_ERROR_INVALID_VALUE;
        }
        ShimState::with(|s| {
            std::ptr::copy_nonoverlapping(s.ctx.device.uuid.as_ptr(), uuid as *mut u8, 16)
        });
        CUDA_SUCCESS
    }
}

#[no_mangle]
pub extern "C" fn cuDeviceGetUuid(uuid: *mut c_void, _dev: i32) -> i32 {
    unsafe { DeviceUuid::write(uuid) }
}

#[no_mangle]
pub extern "C" fn cuDeviceGetUuid_v2(uuid: *mut c_void, _dev: i32) -> i32 {
    unsafe { DeviceUuid::write(uuid) }
}

// ==================================================================================================
// context basics
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuCtxCreate_v2(pctx: *mut *mut c_void, flags: u32, dev: i32) -> i32 {
    if pctx.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = ShimState::with(|s| s.create_ctx_with_flags(flags));
    unsafe { *pctx = token };
    CUDA_SUCCESS
}

/// `cuCtxDestroy(ctx)` — retire a context. A null token is `CUDA_ERROR_INVALID_VALUE`; a token that was
/// never created or was already destroyed is `CUDA_ERROR_INVALID_CONTEXT`. Accepting any token let a
/// double-destroy — a lifetime bug — report success.
#[no_mangle]
pub extern "C" fn cuCtxDestroy_v2(ctx: *mut c_void) -> i32 {
    if ctx.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|s| {
        if s.destroy_ctx(ctx) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_CONTEXT
        }
    })
}

/// `cuCtxSetCurrent(ctx)` — bind `ctx` to the calling thread. A null token detaches the current context
/// (permitted by CUDA); a token that is not live is `CUDA_ERROR_INVALID_CONTEXT`.
#[no_mangle]
pub extern "C" fn cuCtxSetCurrent(ctx: *mut c_void) -> i32 {
    ShimState::with(|s| {
        if s.set_current_ctx(ctx) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_CONTEXT
        }
    })
}

#[no_mangle]
pub extern "C" fn cuCtxGetCurrent(pctx: *mut *mut c_void) -> i32 {
    if pctx.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let cur = ShimState::with(|s| s.current_ctx());
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
    ShimState::with(|s| {
        if let Err(code) = s.require_init() {
            return code;
        }
        match s.ctx.synchronize(&mut s.sink) {
            Ok(()) => CUDA_SUCCESS,
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

// ==================================================================================================
// context management: push/pop stack, api version, flags
// ==================================================================================================

/// `cuCtxPushCurrent(ctx)` — push `ctx` onto the calling thread's context stack. A null token is
/// `CUDA_ERROR_INVALID_HANDLE`; a destroyed / never-created token is `CUDA_ERROR_INVALID_CONTEXT`.
#[no_mangle]
pub extern "C" fn cuCtxPushCurrent_v2(ctx: *mut c_void) -> i32 {
    if ctx.is_null() {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    ShimState::with(|s| {
        if s.push_current_ctx(ctx) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_CONTEXT
        }
    })
}

/// `cuCtxPopCurrent(pctx)` — pop the calling thread's current context. With no context current the stack
/// is empty and real CUDA returns `CUDA_ERROR_INVALID_CONTEXT`; succeeding with a null token let a
/// program that popped more than it pushed carry on believing it had a context.
#[no_mangle]
pub extern "C" fn cuCtxPopCurrent_v2(pctx: *mut *mut c_void) -> i32 {
    let Some(popped) = ShimState::with(|s| s.pop_current_ctx()) else {
        return CUDA_ERROR_INVALID_CONTEXT;
    };
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
    let f = ShimState::with(|s| s.current_ctx_flags());
    unsafe { *flags = f };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetFlags(flags: u32) -> i32 {
    ShimState::with(|s| s.set_current_ctx_flags(flags));
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
    let token = ShimState::with(|s| s.primary_ctx_retain());
    unsafe { *pctx = token };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxRelease_v2(dev: i32) -> i32 {
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    ShimState::with(|s| s.primary_ctx_release());
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxReset_v2(dev: i32) -> i32 {
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    ShimState::with(|s| s.primary_ctx_reset());
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxGetState(dev: i32, flags: *mut u32, active: *mut i32) -> i32 {
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    let (f, a) = ShimState::with(|s| s.report_primary_context());
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
    ShimState::with(|s| s.set_primary_ctx_flags(flags));
    CUDA_SUCCESS
}

// ==================================================================================================
// memory info + pointer attributes (report what the allocation table actually knows)
// ==================================================================================================
