/* Clean-room CUDA Driver API ABI declarations for hl's libcuda.so.1 shim.
 *
 * A clean-room re-declaration of the *documented, stable* CUDA Driver API (cuda.h). It is deliberately
 * broad: enough of the surface — types, error codes, attribute/enum values, handle types and the
 * versioned (`_v2`/`_v3`) entry-point names — that an unmodified CUDA app (or `libcudart`) links
 * against OUR libcuda and either runs its PTX kernels on hl's software backend or gets the spec-correct
 * error for an operation the hl-gpu model cannot serve. This is the ZLUDA "ship a drop-in libcuda.so"
 * seam; hl forwards to the hl-GPU IR + Metal instead of ROCm. No NVIDIA source is used.
 *
 * The load-bearing parts — the `_v2` versioned symbol names, `CUdeviceptr` width, the
 * `cuLaunchKernel` `void** kernelParams` calling convention, and the numeric values of the result /
 * attribute enums — match NVIDIA's published headers so the ABI is compatible.
 */
#ifndef HL_CUDA_MIN_H
#define HL_CUDA_MIN_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned int       cuuint32_t;
typedef unsigned long long cuuint64_t;

/* --- result codes (full documented set; values from cuda.h) --- */
typedef enum CUresult_enum {
    CUDA_SUCCESS                              = 0,
    CUDA_ERROR_INVALID_VALUE                  = 1,
    CUDA_ERROR_OUT_OF_MEMORY                  = 2,
    CUDA_ERROR_NOT_INITIALIZED                = 3,
    CUDA_ERROR_DEINITIALIZED                  = 4,
    CUDA_ERROR_PROFILER_DISABLED             = 5,
    CUDA_ERROR_PROFILER_NOT_INITIALIZED      = 6,
    CUDA_ERROR_PROFILER_ALREADY_STARTED      = 7,
    CUDA_ERROR_PROFILER_ALREADY_STOPPED      = 8,
    CUDA_ERROR_STUB_LIBRARY                   = 34,
    CUDA_ERROR_DEVICE_UNAVAILABLE             = 46,
    CUDA_ERROR_NO_DEVICE                       = 100,
    CUDA_ERROR_INVALID_DEVICE                  = 101,
    CUDA_ERROR_DEVICE_NOT_LICENSED            = 102,
    CUDA_ERROR_INVALID_IMAGE                   = 200,
    CUDA_ERROR_INVALID_CONTEXT                 = 201,
    CUDA_ERROR_CONTEXT_ALREADY_CURRENT        = 202,
    CUDA_ERROR_MAP_FAILED                      = 205,
    CUDA_ERROR_UNMAP_FAILED                    = 206,
    CUDA_ERROR_ARRAY_IS_MAPPED                 = 207,
    CUDA_ERROR_ALREADY_MAPPED                  = 208,
    CUDA_ERROR_NO_BINARY_FOR_GPU               = 209,
    CUDA_ERROR_ALREADY_ACQUIRED                = 210,
    CUDA_ERROR_NOT_MAPPED                      = 211,
    CUDA_ERROR_NOT_MAPPED_AS_ARRAY             = 212,
    CUDA_ERROR_NOT_MAPPED_AS_POINTER           = 213,
    CUDA_ERROR_ECC_UNCORRECTABLE               = 214,
    CUDA_ERROR_UNSUPPORTED_LIMIT               = 215,
    CUDA_ERROR_CONTEXT_ALREADY_IN_USE          = 216,
    CUDA_ERROR_PEER_ACCESS_UNSUPPORTED         = 217,
    CUDA_ERROR_INVALID_PTX                     = 218,
    CUDA_ERROR_INVALID_GRAPHICS_CONTEXT        = 219,
    CUDA_ERROR_NVLINK_UNCORRECTABLE            = 220,
    CUDA_ERROR_JIT_COMPILER_NOT_FOUND          = 221,
    CUDA_ERROR_UNSUPPORTED_PTX_VERSION         = 222,
    CUDA_ERROR_JIT_COMPILATION_DISABLED        = 223,
    CUDA_ERROR_UNSUPPORTED_EXEC_AFFINITY       = 224,
    CUDA_ERROR_INVALID_SOURCE                   = 300,
    CUDA_ERROR_FILE_NOT_FOUND                    = 301,
    CUDA_ERROR_SHARED_OBJECT_SYMBOL_NOT_FOUND    = 302,
    CUDA_ERROR_SHARED_OBJECT_INIT_FAILED         = 303,
    CUDA_ERROR_OPERATING_SYSTEM                   = 304,
    CUDA_ERROR_INVALID_HANDLE                     = 400,
    CUDA_ERROR_ILLEGAL_STATE                      = 401,
    CUDA_ERROR_NOT_FOUND                          = 500,
    CUDA_ERROR_NOT_READY                          = 600,
    CUDA_ERROR_ILLEGAL_ADDRESS                    = 700,
    CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES            = 701,
    CUDA_ERROR_LAUNCH_TIMEOUT                      = 702,
    CUDA_ERROR_LAUNCH_INCOMPATIBLE_TEXTURING       = 703,
    CUDA_ERROR_PEER_ACCESS_ALREADY_ENABLED         = 704,
    CUDA_ERROR_PEER_ACCESS_NOT_ENABLED             = 705,
    CUDA_ERROR_PRIMARY_CONTEXT_ACTIVE              = 708,
    CUDA_ERROR_CONTEXT_IS_DESTROYED                = 709,
    CUDA_ERROR_ASSERT                               = 710,
    CUDA_ERROR_TOO_MANY_PEERS                       = 711,
    CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED       = 712,
    CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED           = 713,
    CUDA_ERROR_HARDWARE_STACK_ERROR                 = 714,
    CUDA_ERROR_ILLEGAL_INSTRUCTION                  = 715,
    CUDA_ERROR_MISALIGNED_ADDRESS                   = 716,
    CUDA_ERROR_INVALID_ADDRESS_SPACE                = 717,
    CUDA_ERROR_INVALID_PC                           = 718,
    CUDA_ERROR_LAUNCH_FAILED                        = 719,
    CUDA_ERROR_COOPERATIVE_LAUNCH_TOO_LARGE         = 720,
    CUDA_ERROR_NOT_PERMITTED                        = 800,
    CUDA_ERROR_NOT_SUPPORTED                        = 801,
    CUDA_ERROR_SYSTEM_NOT_READY                     = 802,
    CUDA_ERROR_SYSTEM_DRIVER_MISMATCH               = 803,
    CUDA_ERROR_COMPAT_NOT_SUPPORTED_ON_DEVICE       = 804,
    CUDA_ERROR_MPS_CONNECTION_FAILED                = 805,
    CUDA_ERROR_STREAM_CAPTURE_UNSUPPORTED           = 900,
    CUDA_ERROR_STREAM_CAPTURE_INVALIDATED           = 901,
    CUDA_ERROR_STREAM_CAPTURE_MERGE                  = 902,
    CUDA_ERROR_STREAM_CAPTURE_UNMATCHED             = 903,
    CUDA_ERROR_STREAM_CAPTURE_UNJOINED             = 904,
    CUDA_ERROR_STREAM_CAPTURE_ISOLATION           = 905,
    CUDA_ERROR_STREAM_CAPTURE_IMPLICIT            = 906,
    CUDA_ERROR_CAPTURED_EVENT                     = 907,
    CUDA_ERROR_STREAM_CAPTURE_WRONG_THREAD        = 908,
    CUDA_ERROR_TIMEOUT                            = 909,
    CUDA_ERROR_GRAPH_EXEC_UPDATE_FAILURE          = 910,
    CUDA_ERROR_EXTERNAL_DEVICE                    = 911,
    CUDA_ERROR_UNKNOWN                            = 999
} CUresult;

/* --- handle/scalar types (from cuda.h) --- */
typedef int                 CUdevice;
typedef unsigned long long  CUdeviceptr;     /* 64-bit device address; unified-memory = a host pointer */
typedef struct CUctx_st*    CUcontext;
typedef struct CUmod_st*    CUmodule;
typedef struct CUfunc_st*   CUfunction;
typedef struct CUstream_st* CUstream;
typedef struct CUevent_st*  CUevent;
typedef struct CUarray_st*         CUarray;
typedef struct CUmipmappedArray_st* CUmipmappedArray;
typedef struct CUgraph_st*         CUgraph;
typedef struct CUgraphNode_st*     CUgraphNode;
typedef struct CUgraphExec_st*     CUgraphExec;
typedef struct CUmemPoolHandle_st* CUmemoryPool;
typedef struct CUextMemory_st*     CUexternalMemory;
typedef struct CUextSemaphore_st*  CUexternalSemaphore;
typedef struct CUlinkState_st*     CUlinkState;
typedef struct CUlib_st*           CUlibrary;
typedef struct CUkern_st*          CUkernel;
typedef struct CUtexref_st*        CUtexref;
typedef struct CUsurfref_st*       CUsurfref;
typedef struct CUgraphicsResource_st* CUgraphicsResource;
typedef struct CUuserObject_st*    CUuserObject;
typedef struct CUgreenCtx_st*      CUgreenCtx;
typedef unsigned long long         CUtexObject;
typedef unsigned long long         CUsurfObject;
typedef unsigned long long         CUmemGenericAllocationHandle;

typedef struct CUuuid_st { char bytes[16]; } CUuuid;

typedef void (*CUstreamCallback)(CUstream hStream, CUresult status, void* userData);
typedef void (*CUhostFn)(void* userData);
typedef size_t (*CUoccupancyB2DSize)(int blockSize);

/* --- device attributes (values from cuda.h CUdevice_attribute) --- */
typedef enum CUdevice_attribute_enum {
    CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK = 1,
    CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X = 2,
    CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y = 3,
    CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z = 4,
    CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X = 5,
    CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y = 6,
    CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z = 7,
    CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK = 8,
    CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY = 9,
    CU_DEVICE_ATTRIBUTE_WARP_SIZE = 10,
    CU_DEVICE_ATTRIBUTE_MAX_PITCH = 11,
    CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK = 12,
    CU_DEVICE_ATTRIBUTE_CLOCK_RATE = 13,
    CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT = 14,
    CU_DEVICE_ATTRIBUTE_GPU_OVERLAP = 15,
    CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT = 16,
    CU_DEVICE_ATTRIBUTE_KERNEL_EXEC_TIMEOUT = 17,
    CU_DEVICE_ATTRIBUTE_INTEGRATED = 18,
    CU_DEVICE_ATTRIBUTE_CAN_MAP_HOST_MEMORY = 19,
    CU_DEVICE_ATTRIBUTE_COMPUTE_MODE = 20,
    CU_DEVICE_ATTRIBUTE_MAXIMUM_TEXTURE1D_WIDTH = 21,
    CU_DEVICE_ATTRIBUTE_CONCURRENT_KERNELS = 31,
    CU_DEVICE_ATTRIBUTE_ECC_ENABLED = 32,
    CU_DEVICE_ATTRIBUTE_PCI_BUS_ID = 33,
    CU_DEVICE_ATTRIBUTE_PCI_DEVICE_ID = 34,
    CU_DEVICE_ATTRIBUTE_TCC_DRIVER = 35,
    CU_DEVICE_ATTRIBUTE_MEMORY_CLOCK_RATE = 36,
    CU_DEVICE_ATTRIBUTE_GLOBAL_MEMORY_BUS_WIDTH = 37,
    CU_DEVICE_ATTRIBUTE_L2_CACHE_SIZE = 38,
    CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_MULTIPROCESSOR = 39,
    CU_DEVICE_ATTRIBUTE_ASYNC_ENGINE_COUNT = 40,
    CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING = 41,
    CU_DEVICE_ATTRIBUTE_PCI_DOMAIN_ID = 50,
    CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR = 75,
    CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR = 76,
    CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR = 81,
    CU_DEVICE_ATTRIBUTE_MULTI_GPU_BOARD = 84,
    CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY = 83,
    CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS = 89,
    CU_DEVICE_ATTRIBUTE_COMPUTE_PREEMPTION_SUPPORTED = 90,
    CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN = 97,
    CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS = 101,
    CU_DEVICE_ATTRIBUTE_DIRECT_MANAGED_MEM_ACCESS_FROM_HOST = 108,
    CU_DEVICE_ATTRIBUTE_MAX_PERSISTING_L2_CACHE_SIZE = 108,
    CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED = 115,
    CU_DEVICE_ATTRIBUTE_MAX = 130
} CUdevice_attribute;

/* --- function attributes (CUfunction_attribute) --- */
typedef enum CUfunction_attribute_enum {
    CU_FUNC_ATTRIBUTE_MAX_THREADS_PER_BLOCK = 0,
    CU_FUNC_ATTRIBUTE_SHARED_SIZE_BYTES = 1,
    CU_FUNC_ATTRIBUTE_CONST_SIZE_BYTES = 2,
    CU_FUNC_ATTRIBUTE_LOCAL_SIZE_BYTES = 3,
    CU_FUNC_ATTRIBUTE_NUM_REGS = 4,
    CU_FUNC_ATTRIBUTE_PTX_VERSION = 5,
    CU_FUNC_ATTRIBUTE_BINARY_VERSION = 6,
    CU_FUNC_ATTRIBUTE_CACHE_MODE_CA = 7,
    CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES = 8,
    CU_FUNC_ATTRIBUTE_PREFERRED_SHARED_MEMORY_CARVEOUT = 9,
    CU_FUNC_ATTRIBUTE_MAX = 11
} CUfunction_attribute;

/* --- pointer attributes (CUpointer_attribute) --- */
typedef enum CUpointer_attribute_enum {
    CU_POINTER_ATTRIBUTE_CONTEXT = 1,
    CU_POINTER_ATTRIBUTE_MEMORY_TYPE = 2,
    CU_POINTER_ATTRIBUTE_DEVICE_POINTER = 3,
    CU_POINTER_ATTRIBUTE_HOST_POINTER = 4,
    CU_POINTER_ATTRIBUTE_SYNC_MEMOPS = 6,
    CU_POINTER_ATTRIBUTE_BUFFER_ID = 7,
    CU_POINTER_ATTRIBUTE_IS_MANAGED = 8,
    CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL = 9,
    CU_POINTER_ATTRIBUTE_RANGE_START_ADDR = 11,
    CU_POINTER_ATTRIBUTE_RANGE_SIZE = 12,
    CU_POINTER_ATTRIBUTE_MAPPED = 13
} CUpointer_attribute;

typedef enum CUmemorytype_enum {
    CU_MEMORYTYPE_HOST = 1,
    CU_MEMORYTYPE_DEVICE = 2,
    CU_MEMORYTYPE_ARRAY = 3,
    CU_MEMORYTYPE_UNIFIED = 4
} CUmemorytype;

typedef enum CUlimit_enum {
    CU_LIMIT_STACK_SIZE = 0,
    CU_LIMIT_PRINTF_FIFO_SIZE = 1,
    CU_LIMIT_MALLOC_HEAP_SIZE = 2,
    CU_LIMIT_DEV_RUNTIME_SYNC_DEPTH = 3,
    CU_LIMIT_DEV_RUNTIME_PENDING_LAUNCH_COUNT = 4,
    CU_LIMIT_MAX_L2_FETCH_GRANULARITY = 5,
    CU_LIMIT_PERSISTING_L2_CACHE_SIZE = 6,
    CU_LIMIT_MAX = 7
} CUlimit;

typedef enum CUfunc_cache_enum {
    CU_FUNC_CACHE_PREFER_NONE = 0,
    CU_FUNC_CACHE_PREFER_SHARED = 1,
    CU_FUNC_CACHE_PREFER_L1 = 2,
    CU_FUNC_CACHE_PREFER_EQUAL = 3
} CUfunc_cache;

typedef enum CUsharedconfig_enum {
    CU_SHARED_MEM_CONFIG_DEFAULT_BANK_SIZE = 0,
    CU_SHARED_MEM_CONFIG_FOUR_BYTE_BANK_SIZE = 1,
    CU_SHARED_MEM_CONFIG_EIGHT_BYTE_BANK_SIZE = 2
} CUsharedconfig;

typedef enum CUjit_option_enum { CU_JIT_MAX_OPTION = 30 } CUjit_option;

typedef struct CUdevprop_st {
    int maxThreadsPerBlock;
    int maxThreadsDim[3];
    int maxGridSize[3];
    int sharedMemPerBlock;
    int totalConstantMemory;
    int SIMDWidth;
    int memPitch;
    int regsPerBlock;
    int clockRate;
    int textureAlign;
} CUdevprop;

/* cuLaunchKernelEx configuration */
typedef struct CUlaunchAttribute_st { int id; int pad; char raw[64]; } CUlaunchAttribute;
typedef struct CUlaunchConfig_st {
    unsigned int gridDimX, gridDimY, gridDimZ;
    unsigned int blockDimX, blockDimY, blockDimZ;
    unsigned int sharedMemBytes;
    CUstream hStream;
    CUlaunchAttribute* attrs;
    unsigned int numAttrs;
} CUlaunchConfig;

/* cuGetProcAddress */
typedef enum CUdriverProcAddressQueryResult_enum {
    CU_GET_PROC_ADDRESS_SUCCESS = 0,
    CU_GET_PROC_ADDRESS_SYMBOL_NOT_FOUND = 1,
    CU_GET_PROC_ADDRESS_VERSION_NOT_SUFFICIENT = 2
} CUdriverProcAddressQueryResult;

typedef enum CUmoduleLoadingMode_enum {
    CU_MODULE_EAGER_LOADING = 1,
    CU_MODULE_LAZY_LOADING = 2
} CUmoduleLoadingMode;

/* stream / event / context / alloc flags + default-stream sentinels */
#define CU_STREAM_DEFAULT       0x0
#define CU_STREAM_NON_BLOCKING  0x1
#define CU_STREAM_LEGACY        ((CUstream)0x1)
#define CU_STREAM_PER_THREAD    ((CUstream)0x2)
#define CU_EVENT_DEFAULT        0x0
#define CU_EVENT_BLOCKING_SYNC  0x1
#define CU_EVENT_DISABLE_TIMING 0x2
#define CU_MEMHOSTALLOC_PORTABLE      0x01
#define CU_MEMHOSTALLOC_DEVICEMAP     0x02
#define CU_MEMHOSTALLOC_WRITECOMBINED 0x04
#define CU_MEM_ATTACH_GLOBAL    0x1
#define CU_MEM_ATTACH_HOST      0x2
#define CU_LAUNCH_PARAM_END             ((void*)0x00)
#define CU_LAUNCH_PARAM_BUFFER_POINTER  ((void*)0x01)
#define CU_LAUNCH_PARAM_BUFFER_SIZE     ((void*)0x02)

#ifdef __cplusplus
}
#endif
#endif /* HL_CUDA_MIN_H */
