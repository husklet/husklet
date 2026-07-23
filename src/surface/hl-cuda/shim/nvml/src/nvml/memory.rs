use super::*;

/// `nvmlMemory_t` (v1) — `{ total, free, used }` in bytes.
#[repr(C)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

/// `nvmlMemory_v2_t` — version-tagged with an extra `reserved` field. The caller sets `version`; we keep
/// it and fill the byte counts.
#[repr(C)]
struct NvmlMemoryV2 {
    version: u32,
    total: u64,
    reserved: u64,
    free: u64,
    used: u64,
}

#[repr(C)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMemoryInfo(dev: *mut c_void, m: *mut c_void) -> i32 {
    if !Nvml::is_valid(dev) || m.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    let total = Nvml::with(|s| s.desc.total_mem);
    let out = unsafe { &mut *(m as *mut NvmlMemory) };
    out.total = total;
    out.used = 0;
    out.free = total;
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetMemoryInfo_v2(dev: *mut c_void, m: *mut c_void) -> i32 {
    if !Nvml::is_valid(dev) || m.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    let total = Nvml::with(|s| s.desc.total_mem);
    let out = unsafe { &mut *(m as *mut NvmlMemoryV2) };
    // Keep the caller-provided version tag; fill the v2 byte counts.
    out.total = total;
    out.reserved = 0;
    out.used = 0;
    out.free = total;
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetUtilizationRates(dev: *mut c_void, u: *mut c_void) -> i32 {
    if !Nvml::is_valid(dev) || u.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    // Idle simulated device: no outstanding compute → 0% GPU / 0% memory-controller utilization.
    let out = unsafe { &mut *(u as *mut NvmlUtilization) };
    out.gpu = 0;
    out.memory = 0;
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetEncoderUtilization(
    dev: *mut c_void,
    util: *mut u32,
    sampling: *mut u32,
) -> i32 {
    if !Nvml::is_valid(dev) || util.is_null() || sampling.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe {
        *util = 0;
        *sampling = 167000; // sampling period (µs), like the C oracle
    }
    NVML_SUCCESS
}

#[no_mangle]
pub extern "C" fn nvmlDeviceGetDecoderUtilization(
    dev: *mut c_void,
    util: *mut u32,
    sampling: *mut u32,
) -> i32 {
    if !Nvml::is_valid(dev) || util.is_null() || sampling.is_null() {
        return NVML_ERROR_INVALID_ARGUMENT;
    }
    unsafe {
        *util = 0;
        *sampling = 167000;
    }
    NVML_SUCCESS
}
