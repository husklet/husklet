//! CUDA result-code contract: the `CUresult` (driver API) + `cudaError_t` (runtime API) values the
//! guest libs return, and the map from a lowering [`hl_gpu::GpuError`] onto them.
//!
//! Numeric values match NVIDIA's published `cuda.h` / `driver_types.h` (the stable ABI a CUDA app
//! compiles against); they are re-declared clean-room here, ported from `hl-shim-cuda/src/result.rs`.
//! Only the subset the hand-written entry points reference is declared — generated stubs (later pass)
//! never inspect a value, they return `CUDA_SUCCESS`. The `GpuError` → code map is what the shim
//! cdylibs (later) will use to turn a lowering error into the `CUresult` the guest expects.

use hl_gpu::GpuError;

// ---- CUresult (returned as i32 across the C ABI) -------------------------------------------------
pub const CUDA_SUCCESS: i32 = 0;
pub const CUDA_ERROR_INVALID_VALUE: i32 = 1;
pub const CUDA_ERROR_OUT_OF_MEMORY: i32 = 2;
pub const CUDA_ERROR_NOT_INITIALIZED: i32 = 3;
pub const CUDA_ERROR_INVALID_DEVICE: i32 = 101;
pub const CUDA_ERROR_INVALID_IMAGE: i32 = 200;
pub const CUDA_ERROR_INVALID_CONTEXT: i32 = 201;
pub const CUDA_ERROR_UNSUPPORTED_LIMIT: i32 = 215;
pub const CUDA_ERROR_INVALID_PTX: i32 = 218;
pub const CUDA_ERROR_FILE_NOT_FOUND: i32 = 301;
pub const CUDA_ERROR_INVALID_HANDLE: i32 = 400;
pub const CUDA_ERROR_NOT_FOUND: i32 = 500;
pub const CUDA_ERROR_NOT_READY: i32 = 600;
pub const CUDA_ERROR_NOT_SUPPORTED: i32 = 801;
pub const CUDA_ERROR_UNKNOWN: i32 = 999;

/// The driver version `cuDriverGetVersion` reports: `major*1000 + minor*10`. 12020 == CUDA 12.2.
pub const DRIVER_VERSION: i32 = 12020;

// ---- cudaError_t (runtime API; the subset the runtime entry points return) -----------------------
pub const CUDART_SUCCESS: i32 = 0; // cudaSuccess
pub const CUDART_ERROR_INVALID_VALUE: i32 = 1; // cudaErrorInvalidValue
pub const CUDART_ERROR_MEMORY_ALLOCATION: i32 = 2; // cudaErrorMemoryAllocation
pub const CUDART_ERROR_INITIALIZATION: i32 = 3; // cudaErrorInitializationError
pub const CUDART_ERROR_INVALID_DEVICE: i32 = 101; // cudaErrorInvalidDevice
pub const CUDART_ERROR_INVALID_KERNEL_IMAGE: i32 = 200; // cudaErrorInvalidKernelImage
pub const CUDART_ERROR_INVALID_PTX: i32 = 218; // cudaErrorInvalidPtx
pub const CUDART_ERROR_INVALID_RESOURCE_HANDLE: i32 = 400; // cudaErrorInvalidResourceHandle
pub const CUDART_ERROR_SYMBOL_NOT_FOUND: i32 = 500; // cudaErrorSymbolNotFound
pub const CUDART_ERROR_NOT_SUPPORTED: i32 = 801; // cudaErrorNotSupported
pub const CUDART_ERROR_UNKNOWN: i32 = 999; // cudaErrorUnknown

/// Map a lowering [`GpuError`] onto the `CUresult` a driver-API entry point returns. A kernel that uses
/// an instruction/space/type outside hl's modeled subset is `CUDA_ERROR_NOT_SUPPORTED` (the executor
/// genuinely cannot run it) — matching a real driver — while an invalid-argument/handle error maps to
/// the closest `CUDA_ERROR_*`.
pub fn cu_result_from_gpu_error(e: &GpuError) -> i32 {
    match e {
        GpuError::Unsupported(_) => CUDA_ERROR_NOT_SUPPORTED,
        GpuError::Kernel(_) => CUDA_ERROR_INVALID_PTX,
        GpuError::UnknownId { .. } | GpuError::DuplicateId { .. } => CUDA_ERROR_INVALID_HANDLE,
        GpuError::OutOfBounds => CUDA_ERROR_INVALID_VALUE,
        GpuError::ResourceLimit(_) => CUDA_ERROR_OUT_OF_MEMORY,
        GpuError::Decode(_) => CUDA_ERROR_UNKNOWN,
        GpuError::Invalid(_)
        | GpuError::BadEnum { .. }
        | GpuError::BadTag(_)
        | GpuError::NonFinite(_)
        | GpuError::NonCanonicalBool(_)
        | GpuError::Utf8
        | GpuError::ShortBuffer
        | GpuError::TrailingBytes => CUDA_ERROR_INVALID_VALUE,
    }
}

/// Map a lowering [`GpuError`] onto the `cudaError_t` a runtime-API entry point returns.
pub fn cudart_from_gpu_error(e: &GpuError) -> i32 {
    match e {
        GpuError::Unsupported(_) => CUDART_ERROR_NOT_SUPPORTED,
        GpuError::Kernel(_) => CUDART_ERROR_INVALID_PTX,
        GpuError::UnknownId { .. } | GpuError::DuplicateId { .. } => {
            CUDART_ERROR_INVALID_RESOURCE_HANDLE
        }
        GpuError::ResourceLimit(_) => CUDART_ERROR_MEMORY_ALLOCATION,
        GpuError::Decode(_) => CUDART_ERROR_UNKNOWN,
        _ => CUDART_ERROR_INVALID_VALUE,
    }
}
