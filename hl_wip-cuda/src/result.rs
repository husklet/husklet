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

/// `cuCtxGetApiVersion` reports the classic 3.2 driver-API version (the value real drivers return for a
/// context created through the modern API).
pub const CTX_API_VERSION: u32 = 3020;

// ---- CUdevice_attribute (the set `cuDeviceGetAttribute` answers; values from cuda.h) --------------
pub const CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK: i32 = 1;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X: i32 = 2;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y: i32 = 3;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z: i32 = 4;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X: i32 = 5;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y: i32 = 6;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z: i32 = 7;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK: i32 = 8;
pub const CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY: i32 = 9;
pub const CU_DEVICE_ATTRIBUTE_WARP_SIZE: i32 = 10;
pub const CU_DEVICE_ATTRIBUTE_MAX_PITCH: i32 = 11;
pub const CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK: i32 = 12;
pub const CU_DEVICE_ATTRIBUTE_CLOCK_RATE: i32 = 13;
pub const CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT: i32 = 14;
pub const CU_DEVICE_ATTRIBUTE_GPU_OVERLAP: i32 = 15;
pub const CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT: i32 = 16;
pub const CU_DEVICE_ATTRIBUTE_KERNEL_EXEC_TIMEOUT: i32 = 17;
pub const CU_DEVICE_ATTRIBUTE_INTEGRATED: i32 = 18;
pub const CU_DEVICE_ATTRIBUTE_CAN_MAP_HOST_MEMORY: i32 = 19;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_MODE: i32 = 20;
pub const CU_DEVICE_ATTRIBUTE_MAXIMUM_TEXTURE1D_WIDTH: i32 = 21;
pub const CU_DEVICE_ATTRIBUTE_CONCURRENT_KERNELS: i32 = 31;
pub const CU_DEVICE_ATTRIBUTE_ECC_ENABLED: i32 = 32;
pub const CU_DEVICE_ATTRIBUTE_PCI_BUS_ID: i32 = 33;
pub const CU_DEVICE_ATTRIBUTE_PCI_DEVICE_ID: i32 = 34;
pub const CU_DEVICE_ATTRIBUTE_TCC_DRIVER: i32 = 35;
pub const CU_DEVICE_ATTRIBUTE_MEMORY_CLOCK_RATE: i32 = 36;
pub const CU_DEVICE_ATTRIBUTE_GLOBAL_MEMORY_BUS_WIDTH: i32 = 37;
pub const CU_DEVICE_ATTRIBUTE_L2_CACHE_SIZE: i32 = 38;
pub const CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_MULTIPROCESSOR: i32 = 39;
pub const CU_DEVICE_ATTRIBUTE_ASYNC_ENGINE_COUNT: i32 = 40;
pub const CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING: i32 = 41;
pub const CU_DEVICE_ATTRIBUTE_PCI_DOMAIN_ID: i32 = 50;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR: i32 = 75;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR: i32 = 76;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR: i32 = 81;
pub const CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY: i32 = 83;
pub const CU_DEVICE_ATTRIBUTE_MULTI_GPU_BOARD: i32 = 84;
pub const CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS: i32 = 89;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_PREEMPTION_SUPPORTED: i32 = 90;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN: i32 = 97;
pub const CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS: i32 = 101;
pub const CU_DEVICE_ATTRIBUTE_DIRECT_MANAGED_MEM_ACCESS_FROM_HOST: i32 = 108;
pub const CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED: i32 = 115;

// ---- CUpointer_attribute + CUmemorytype (values from cuda.h) -------------------------------------
pub const CU_POINTER_ATTRIBUTE_CONTEXT: i32 = 1;
pub const CU_POINTER_ATTRIBUTE_MEMORY_TYPE: i32 = 2;
pub const CU_POINTER_ATTRIBUTE_DEVICE_POINTER: i32 = 3;
pub const CU_POINTER_ATTRIBUTE_HOST_POINTER: i32 = 4;
pub const CU_POINTER_ATTRIBUTE_SYNC_MEMOPS: i32 = 6;
pub const CU_POINTER_ATTRIBUTE_BUFFER_ID: i32 = 7;
pub const CU_POINTER_ATTRIBUTE_IS_MANAGED: i32 = 8;
pub const CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL: i32 = 9;
pub const CU_POINTER_ATTRIBUTE_RANGE_START_ADDR: i32 = 11;
pub const CU_POINTER_ATTRIBUTE_RANGE_SIZE: i32 = 12;
pub const CU_POINTER_ATTRIBUTE_MAPPED: i32 = 13;
pub const CU_MEMORYTYPE_DEVICE: u32 = 2;

// ---- CUfunction_attribute (the set `cuFuncGetAttribute`/`cuFuncSetAttribute` answer; from cuda.h) --
pub const CU_FUNC_ATTRIBUTE_MAX_THREADS_PER_BLOCK: i32 = 0;
pub const CU_FUNC_ATTRIBUTE_SHARED_SIZE_BYTES: i32 = 1;
pub const CU_FUNC_ATTRIBUTE_CONST_SIZE_BYTES: i32 = 2;
pub const CU_FUNC_ATTRIBUTE_LOCAL_SIZE_BYTES: i32 = 3;
pub const CU_FUNC_ATTRIBUTE_NUM_REGS: i32 = 4;
pub const CU_FUNC_ATTRIBUTE_PTX_VERSION: i32 = 5;
pub const CU_FUNC_ATTRIBUTE_BINARY_VERSION: i32 = 6;
pub const CU_FUNC_ATTRIBUTE_CACHE_MODE_CA: i32 = 7;
pub const CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES: i32 = 8;
pub const CU_FUNC_ATTRIBUTE_PREFERRED_SHARED_MEMORY_CARVEOUT: i32 = 9;

// ---- CUlimit — `CU_LIMIT_MAX` is one past the last valid `CUlimit`; the limit table has this many slots.
pub const CU_LIMIT_MAX: i32 = 7;

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
