use super::*;
extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Map a base (unversioned) `cu*` name to the newest versioned symbol the app should bind. A CUDA app
/// that calls `cuGetProcAddress("cuMemAlloc", ...)` expects the `_v2` entry point back — the same alias
/// table the real driver's dispatch applies.
pub(super) struct CudaSymbol;

impl CudaSymbol {
    pub(super) fn newest(name: &str) -> &str {
        match name {
            "cuDeviceTotalMem" => "cuDeviceTotalMem_v2",
            "cuCtxCreate" => "cuCtxCreate_v2",
            "cuCtxDestroy" => "cuCtxDestroy_v2",
            "cuCtxPushCurrent" => "cuCtxPushCurrent_v2",
            "cuCtxPopCurrent" => "cuCtxPopCurrent_v2",
            "cuDevicePrimaryCtxRelease" => "cuDevicePrimaryCtxRelease_v2",
            "cuDevicePrimaryCtxReset" => "cuDevicePrimaryCtxReset_v2",
            "cuDevicePrimaryCtxSetFlags" => "cuDevicePrimaryCtxSetFlags_v2",
            "cuModuleGetGlobal" => "cuModuleGetGlobal_v2",
            "cuMemGetInfo" => "cuMemGetInfo_v2",
            "cuMemAlloc" => "cuMemAlloc_v2",
            "cuMemAllocPitch" => "cuMemAllocPitch_v2",
            "cuMemFree" => "cuMemFree_v2",
            "cuMemGetAddressRange" => "cuMemGetAddressRange_v2",
            "cuMemAllocHost" => "cuMemAllocHost_v2",
            "cuMemHostGetDevicePointer" => "cuMemHostGetDevicePointer_v2",
            "cuMemHostRegister" => "cuMemHostRegister_v2",
            "cuMemcpyHtoD" => "cuMemcpyHtoD_v2",
            "cuMemcpyDtoH" => "cuMemcpyDtoH_v2",
            "cuMemcpyDtoD" => "cuMemcpyDtoD_v2",
            "cuMemcpyHtoDAsync" => "cuMemcpyHtoDAsync_v2",
            "cuMemcpyDtoHAsync" => "cuMemcpyDtoHAsync_v2",
            "cuMemcpyDtoDAsync" => "cuMemcpyDtoDAsync_v2",
            "cuMemsetD8" => "cuMemsetD8_v2",
            "cuMemsetD16" => "cuMemsetD16_v2",
            "cuMemsetD32" => "cuMemsetD32_v2",
            "cuStreamDestroy" => "cuStreamDestroy_v2",
            "cuEventDestroy" => "cuEventDestroy_v2",
            other => other, // already a real exported symbol (versioned or unversioned)
        }
    }
}

/// `cuGetProcAddress(symbol, pfn, cudaVersion, flags)` — resolve a driver-API entry point by name to its
/// function pointer. Resolves against this object's own exported `cu*` surface via `dlsym(RTLD_DEFAULT)`
/// (when deployed as `libcuda.so.1` every entry point is a dynamic symbol). A symbol this driver does not
/// export is honestly `CUDA_ERROR_NOT_FOUND`.
#[no_mangle]
pub extern "C" fn cuGetProcAddress(
    symbol: *const c_char,
    pfn: *mut *mut c_void,
    cuda_version: i32,
    flags: u64,
) -> i32 {
    let _ = (cuda_version, flags);
    if symbol.is_null() || pfn.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let Some(raw) = (unsafe { CInput::string(symbol) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let Ok(name) = String::from_utf8(raw) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let resolved = CudaSymbol::newest(&name);
    let Ok(cname) = std::ffi::CString::new(resolved) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let p = unsafe { dlsym(core::ptr::null_mut(), cname.as_ptr()) }; // RTLD_DEFAULT
    if p.is_null() {
        unsafe { *pfn = core::ptr::null_mut() };
        return CUDA_ERROR_NOT_FOUND;
    }
    unsafe { *pfn = p };
    CUDA_SUCCESS
}

/// `cuGetProcAddress_v2(symbol, pfn, cudaVersion, flags, status)` — the same lookup as `cuGetProcAddress`,
/// plus the driver's `CUdriverProcAddressQueryResult` status out-param.
#[no_mangle]
pub extern "C" fn cuGetProcAddress_v2(
    symbol: *const c_char,
    pfn: *mut *mut c_void,
    cuda_version: i32,
    flags: u64,
    status: *mut i32,
) -> i32 {
    let r = cuGetProcAddress(symbol, pfn, cuda_version, flags);
    if !status.is_null() {
        unsafe {
            *status = if r == CUDA_SUCCESS {
                CU_GET_PROC_ADDRESS_SUCCESS
            } else {
                CU_GET_PROC_ADDRESS_SYMBOL_NOT_FOUND
            };
        }
    }
    r
}

// ==================================================================================================
// launch: cooperative kernel, host callbacks
// ==================================================================================================
