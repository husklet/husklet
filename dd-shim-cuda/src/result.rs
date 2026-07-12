//! CUDA Driver API result codes + the device-attribute enum values the bring-up entry points answer.
//!
//! Numeric values match NVIDIA's published `cuda.h` (they are the stable ABI a CUDA app compiles
//! against); they are re-declared clean-room here, mirroring dd's `dd-gpu/cuda/cuda_min.h`. Only the
//! subset the hand-written entry points reference is declared — the generated stubs never inspect a
//! `CUresult` value, they just return `CUDA_SUCCESS`.

// ---- CUresult (returned as i32 across the C ABI) -------------------------------------------------
pub const CUDA_SUCCESS: i32 = 0;
pub const CUDA_ERROR_INVALID_VALUE: i32 = 1;
pub const CUDA_ERROR_OUT_OF_MEMORY: i32 = 2;
pub const CUDA_ERROR_NOT_INITIALIZED: i32 = 3;
pub const CUDA_ERROR_INVALID_DEVICE: i32 = 101;
pub const CUDA_ERROR_INVALID_IMAGE: i32 = 200;
pub const CUDA_ERROR_INVALID_CONTEXT: i32 = 201;
pub const CUDA_ERROR_INVALID_PTX: i32 = 218;
pub const CUDA_ERROR_INVALID_HANDLE: i32 = 400;
pub const CUDA_ERROR_NOT_FOUND: i32 = 500;

/// The driver version cuDriverGetVersion reports: `major*1000 + minor*10`. 12020 == CUDA 12.2,
/// matching dd's C shim (`dd-gpu/cuda/cuda_shim.c` `g_driver_version`).
pub const DRIVER_VERSION: i32 = 12020;

// ---- CUdevice_attribute (subset the bring-up path answers; values from cuda.h) -------------------
pub const CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK: i32 = 1;
pub const CU_DEVICE_ATTRIBUTE_WARP_SIZE: i32 = 10;
pub const CU_DEVICE_ATTRIBUTE_CLOCK_RATE: i32 = 13;
pub const CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT: i32 = 16;
pub const CU_DEVICE_ATTRIBUTE_INTEGRATED: i32 = 18;
pub const CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING: i32 = 41;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR: i32 = 75;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR: i32 = 76;
