//! The hand-written `nvml*` entry points: the init/shutdown/error/count/handle/name/version basics that
//! let a probe enumerate hl's single simulated device. NVML is a device-info surface (no compute path,
//! no command sink) — it just answers the numbers `nvidia-smi`-style enumeration expects. The rest of the
//! 62-entry surface are benign `NVML_SUCCESS` stubs (see `build.rs`).

use core::ffi::{c_char, c_void};

use hl_cuda::CudaDeviceDesc;

// nvmlReturn_t values (stable NVML ABI; from nvml_min.h).
const NVML_SUCCESS: i32 = 0;
const NVML_ERROR_INVALID_ARGUMENT: i32 = 2;

/// The single simulated device NVML reports; name overridable via `$HL_CUDA_NAME` for parity with the
/// C oracle. The opaque `nvmlDevice_t` handle for it is the non-null token `1`.
fn device() -> CudaDeviceDesc {
    let mut d = CudaDeviceDesc::apple_default(8u64 << 30);
    if let Ok(name) = std::env::var("HL_CUDA_NAME") {
        d.name = name;
    }
    d
}

const DEVICE_TOKEN: usize = 1;

unsafe fn write_cstr(dst: *mut c_char, len: u32, s: &str) -> i32 {
    if dst.is_null() || len == 0 {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    let cap = (len as usize).saturating_sub(1);
    let n = s.len().min(cap);
    std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, n);
    *dst.add(n) = 0;
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlInit_v2() -> i32 {
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlInit() -> i32 {
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlInitWithFlags(_flags: u32) -> i32 {
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlShutdown() -> i32 {
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlErrorString(result: i32) -> *const c_char {
    let s: &'static [u8] = match result {
        0 => b"The operation was successful\0",
        1 => b"NVML was not first initialized with nvmlInit()\0",
        2 => b"A supplied argument is invalid\0",
        3 => b"The requested operation is not available on target device\0",
        6 => b"A query to find an object was unsuccessful\0",
        7 => b"An input argument is not large enough\0",
        _ => b"An internal driver error occurred\0",
    };
    s.as_ptr() as *const c_char
}

fn count_impl(count: *mut u32) -> i32 {
    if count.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *count = 1 };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetCount(count: *mut u32) -> i32 {
    count_impl(count)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetCount_v2(count: *mut u32) -> i32 {
    count_impl(count)
}

fn handle_by_index_impl(index: u32, dev: *mut *mut c_void) -> i32 {
    if dev.is_null() || index != 0 {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *dev = DEVICE_TOKEN as *mut c_void };
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetHandleByIndex(index: u32, dev: *mut *mut c_void) -> i32 {
    handle_by_index_impl(index, dev)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetHandleByIndex_v2(index: u32, dev: *mut *mut c_void) -> i32 {
    handle_by_index_impl(index, dev)
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetName(dev: *mut c_void, name: *mut c_char, len: u32) -> i32 {
    if dev as usize != DEVICE_TOKEN {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { write_cstr(name, len, &device().name) }
}

fn cuda_driver_version(v: *mut i32) -> i32 {
    if v.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe { *v = 12020 }; // CUDA 12.2, matching the driver/runtime shims.
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlSystemGetCudaDriverVersion(v: *mut i32) -> i32 {
    cuda_driver_version(v)
}

#[no_mangle]
pub extern "C" fn nvmlSystemGetCudaDriverVersion_v2(v: *mut i32) -> i32 {
    cuda_driver_version(v)
}
