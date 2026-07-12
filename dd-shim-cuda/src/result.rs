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
pub const CUDA_ERROR_UNSUPPORTED_LIMIT: i32 = 215;
pub const CUDA_ERROR_PEER_ACCESS_UNSUPPORTED: i32 = 217;
pub const CUDA_ERROR_INVALID_DEVICE: i32 = 101;
pub const CUDA_ERROR_INVALID_IMAGE: i32 = 200;
pub const CUDA_ERROR_INVALID_CONTEXT: i32 = 201;
pub const CUDA_ERROR_INVALID_PTX: i32 = 218;
pub const CUDA_ERROR_FILE_NOT_FOUND: i32 = 301;
pub const CUDA_ERROR_INVALID_HANDLE: i32 = 400;
pub const CUDA_ERROR_NOT_FOUND: i32 = 500;
pub const CUDA_ERROR_NOT_READY: i32 = 600;
pub const CUDA_ERROR_PEER_ACCESS_NOT_ENABLED: i32 = 705;
pub const CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED: i32 = 712;
pub const CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED: i32 = 713;
pub const CUDA_ERROR_NOT_SUPPORTED: i32 = 801;

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

// ---- CUdevice_attribute (the full set cuDeviceGetAttribute answers, values from cuda.h) ----------
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X: i32 = 2;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y: i32 = 3;
pub const CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z: i32 = 4;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X: i32 = 5;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y: i32 = 6;
pub const CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z: i32 = 7;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK: i32 = 8;
pub const CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY: i32 = 9;
pub const CU_DEVICE_ATTRIBUTE_MAX_PITCH: i32 = 11;
pub const CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK: i32 = 12;
pub const CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT: i32 = 14;
pub const CU_DEVICE_ATTRIBUTE_GPU_OVERLAP: i32 = 15;
pub const CU_DEVICE_ATTRIBUTE_KERNEL_EXEC_TIMEOUT: i32 = 17;
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
pub const CU_DEVICE_ATTRIBUTE_PCI_DOMAIN_ID: i32 = 50;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR: i32 = 81;
pub const CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY: i32 = 83;
pub const CU_DEVICE_ATTRIBUTE_MULTI_GPU_BOARD: i32 = 84;
pub const CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS: i32 = 89;
pub const CU_DEVICE_ATTRIBUTE_COMPUTE_PREEMPTION_SUPPORTED: i32 = 90;
pub const CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN: i32 = 97;
pub const CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS: i32 = 101;
pub const CU_DEVICE_ATTRIBUTE_DIRECT_MANAGED_MEM_ACCESS_FROM_HOST: i32 = 108;
pub const CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED: i32 = 115;

// ---- CUfunction_attribute (values from cuda.h) ---------------------------------------------------
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
pub const CU_MEMORYTYPE_HOST: u32 = 1;
pub const CU_MEMORYTYPE_DEVICE: u32 = 2;

// ---- misc enums the ports reference --------------------------------------------------------------
/// `CU_LIMIT_MAX` — one past the last valid `CUlimit`; the limit table has this many slots.
pub const CU_LIMIT_MAX: i32 = 7;
/// `cuCtxGetApiVersion` reports the classic 3.2 driver-API version (matches the C oracle).
pub const CTX_API_VERSION: u32 = 3020;
/// `CU_MODULE_EAGER_LOADING` — the loading mode `cuModuleGetLoadingMode` reports.
pub const CU_MODULE_EAGER_LOADING: i32 = 1;
/// `cuGetProcAddress_v2` status codes.
pub const CU_GET_PROC_ADDRESS_SUCCESS: i32 = 0;
pub const CU_GET_PROC_ADDRESS_SYMBOL_NOT_FOUND: i32 = 1;
