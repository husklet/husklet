//! CUDA Runtime API result codes (`cudaError_t`) + the `CUresult -> cudaError_t` map.
//!
//! The runtime returns a DIFFERENT enum from the driver: an app that links libcudart checks
//! `cudaError_t`, not the driver's `CUresult`. Values match NVIDIA's published `driver_types.h` (the
//! stable ABI a CUDA app compiles against); re-declared clean-room here, mirroring hl's
//! `hl-gpu/cuda/cudart_min.h`. The map mirrors `hl-gpu/cuda/cudart_shim.c`'s `map_err` exactly so the
//! runtime library returns the SAME `cudaError_t` for the SAME driver `CUresult`.

// ---- cudaError_t (returned as i32 across the C ABI) ----------------------------------------------
pub const CUDA_SUCCESS_RT: i32 = 0; // cudaSuccess
pub const CUDA_ERROR_INVALID_VALUE_RT: i32 = 1; // cudaErrorInvalidValue
pub const CUDA_ERROR_MEMORY_ALLOCATION: i32 = 2; // cudaErrorMemoryAllocation
pub const CUDA_ERROR_INITIALIZATION: i32 = 3; // cudaErrorInitializationError
pub const CUDA_ERROR_CUDART_UNLOADING: i32 = 4;
pub const CUDA_ERROR_PROFILER_DISABLED_RT: i32 = 5;
pub const CUDA_ERROR_INVALID_CONFIGURATION: i32 = 9;
pub const CUDA_ERROR_INVALID_MEMCPY_DIRECTION: i32 = 21;
pub const CUDA_ERROR_INVALID_DEVICE_FUNCTION: i32 = 98;
pub const CUDA_ERROR_NO_DEVICE_RT: i32 = 100;
pub const CUDA_ERROR_INVALID_DEVICE_RT: i32 = 101;
pub const CUDA_ERROR_DEVICE_NOT_LICENSED_RT: i32 = 102;
pub const CUDA_ERROR_INVALID_KERNEL_IMAGE: i32 = 200;
pub const CUDA_ERROR_DEVICE_UNINITIALIZED: i32 = 201;
pub const CUDA_ERROR_NO_KERNEL_IMAGE_FOR_DEVICE: i32 = 209;
pub const CUDA_ERROR_INVALID_PTX_RT: i32 = 218;
pub const CUDA_ERROR_UNSUPPORTED_PTX_VERSION_RT: i32 = 222;
pub const CUDA_ERROR_INVALID_SOURCE_RT: i32 = 300;
pub const CUDA_ERROR_FILE_NOT_FOUND_RT: i32 = 301;
pub const CUDA_ERROR_OPERATING_SYSTEM_RT: i32 = 304;
pub const CUDA_ERROR_INVALID_RESOURCE_HANDLE: i32 = 400;
pub const CUDA_ERROR_ILLEGAL_STATE_RT: i32 = 401;
pub const CUDA_ERROR_SYMBOL_NOT_FOUND: i32 = 500;
pub const CUDA_ERROR_NOT_READY_RT: i32 = 600;
pub const CUDA_ERROR_ILLEGAL_ADDRESS_RT: i32 = 700;
pub const CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES_RT: i32 = 701;
pub const CUDA_ERROR_LAUNCH_TIMEOUT_RT: i32 = 702;
pub const CUDA_ERROR_LAUNCH_FAILURE: i32 = 719;
pub const CUDA_ERROR_NOT_PERMITTED_RT: i32 = 800;
pub const CUDA_ERROR_NOT_SUPPORTED_RT: i32 = 801;
pub const CUDA_ERROR_UNKNOWN_RT: i32 = 999;

// ---- cudaMemcpyKind ------------------------------------------------------------------------------
pub const CUDA_MEMCPY_HOST_TO_HOST: i32 = 0;
pub const CUDA_MEMCPY_HOST_TO_DEVICE: i32 = 1;
pub const CUDA_MEMCPY_DEVICE_TO_HOST: i32 = 2;
pub const CUDA_MEMCPY_DEVICE_TO_DEVICE: i32 = 3;
pub const CUDA_MEMCPY_DEFAULT: i32 = 4;

/// The runtime version `cudaRuntimeGetVersion` reports: 12020 == CUDA 12.2 (matches the C oracle).
pub const RUNTIME_VERSION: i32 = 12020;
/// The driver version `cudaDriverGetVersion` reports (`major*1000 + minor*10`): 12020 == CUDA 12.2,
/// matching the driver shim's `cuDriverGetVersion` (`hl-gpu/cuda/cuda_shim.c` `g_driver_version`).
pub const DRIVER_VERSION: i32 = 12020;

// ---- CUresult numeric values (driver enum, from cuda.h) the map inspects -------------------------
mod cu {
    pub const SUCCESS: i32 = 0;
    pub const INVALID_VALUE: i32 = 1;
    pub const OUT_OF_MEMORY: i32 = 2;
    pub const NOT_INITIALIZED: i32 = 3;
    pub const DEINITIALIZED: i32 = 4;
    pub const PROFILER_DISABLED: i32 = 5;
    pub const NO_DEVICE: i32 = 100;
    pub const INVALID_DEVICE: i32 = 101;
    pub const DEVICE_NOT_LICENSED: i32 = 102;
    pub const INVALID_IMAGE: i32 = 200;
    pub const INVALID_CONTEXT: i32 = 201;
    pub const NO_BINARY_FOR_GPU: i32 = 209;
    pub const INVALID_PTX: i32 = 218;
    pub const UNSUPPORTED_PTX_VERSION: i32 = 222;
    pub const INVALID_SOURCE: i32 = 300;
    pub const FILE_NOT_FOUND: i32 = 301;
    pub const OPERATING_SYSTEM: i32 = 304;
    pub const INVALID_HANDLE: i32 = 400;
    pub const ILLEGAL_STATE: i32 = 401;
    pub const NOT_FOUND: i32 = 500;
    pub const NOT_READY: i32 = 600;
    pub const ILLEGAL_ADDRESS: i32 = 700;
    pub const LAUNCH_OUT_OF_RESOURCES: i32 = 701;
    pub const LAUNCH_TIMEOUT: i32 = 702;
    pub const LAUNCH_FAILED: i32 = 719;
    pub const NOT_PERMITTED: i32 = 800;
    pub const NOT_SUPPORTED: i32 = 801;
}

/// Map a driver `CUresult` to the runtime `cudaError_t`. Byte-for-byte the switch in
/// `hl-gpu/cuda/cudart_shim.c` `map_err`.
pub fn map_err(r: i32) -> i32 {
    match r {
        cu::SUCCESS => CUDA_SUCCESS_RT,
        cu::INVALID_VALUE => CUDA_ERROR_INVALID_VALUE_RT,
        cu::OUT_OF_MEMORY => CUDA_ERROR_MEMORY_ALLOCATION,
        cu::NOT_INITIALIZED => CUDA_ERROR_INITIALIZATION,
        cu::DEINITIALIZED => CUDA_ERROR_CUDART_UNLOADING,
        cu::PROFILER_DISABLED => CUDA_ERROR_PROFILER_DISABLED_RT,
        cu::NO_DEVICE => CUDA_ERROR_NO_DEVICE_RT,
        cu::INVALID_DEVICE => CUDA_ERROR_INVALID_DEVICE_RT,
        cu::DEVICE_NOT_LICENSED => CUDA_ERROR_DEVICE_NOT_LICENSED_RT,
        cu::INVALID_IMAGE => CUDA_ERROR_INVALID_KERNEL_IMAGE,
        cu::INVALID_CONTEXT => CUDA_ERROR_DEVICE_UNINITIALIZED,
        cu::NO_BINARY_FOR_GPU => CUDA_ERROR_NO_KERNEL_IMAGE_FOR_DEVICE,
        cu::INVALID_PTX => CUDA_ERROR_INVALID_PTX_RT,
        cu::UNSUPPORTED_PTX_VERSION => CUDA_ERROR_UNSUPPORTED_PTX_VERSION_RT,
        cu::INVALID_SOURCE => CUDA_ERROR_INVALID_SOURCE_RT,
        cu::FILE_NOT_FOUND => CUDA_ERROR_FILE_NOT_FOUND_RT,
        cu::OPERATING_SYSTEM => CUDA_ERROR_OPERATING_SYSTEM_RT,
        cu::INVALID_HANDLE => CUDA_ERROR_INVALID_RESOURCE_HANDLE,
        cu::ILLEGAL_STATE => CUDA_ERROR_ILLEGAL_STATE_RT,
        cu::NOT_FOUND => CUDA_ERROR_SYMBOL_NOT_FOUND,
        cu::NOT_READY => CUDA_ERROR_NOT_READY_RT,
        cu::ILLEGAL_ADDRESS => CUDA_ERROR_ILLEGAL_ADDRESS_RT,
        cu::LAUNCH_OUT_OF_RESOURCES => CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES_RT,
        cu::LAUNCH_TIMEOUT => CUDA_ERROR_LAUNCH_TIMEOUT_RT,
        cu::LAUNCH_FAILED => CUDA_ERROR_LAUNCH_FAILURE,
        cu::NOT_PERMITTED => CUDA_ERROR_NOT_PERMITTED_RT,
        cu::NOT_SUPPORTED => CUDA_ERROR_NOT_SUPPORTED_RT,
        _ => CUDA_ERROR_UNKNOWN_RT,
    }
}
