/* dd's libcudart.so — the CUDA Runtime API shim, layered on dd's libcuda Driver-API shim.
 *
 * cudart is the upper edge an app (or a framework like PyTorch) links against. It is THIN over the
 * driver for the stateful surface (malloc / memcpy / memset / streams / events / device query), and it
 * owns the compiler-emitted *registration glue* (__cudaRegisterFatBinary/Function/... + the
 * <<<grid,block>>> call-config stack) plus a cudart-side thread-local last-error cell and the
 * CUresult -> cudaError_t map. See docs/ideas/CUDART_PLAN.md (tiers 0 + 1).
 *
 * How a kernel launch flows (nvcc lowering, §1c of the plan):
 *   __cudaPushCallConfiguration(grid, block, shmem, stream);   // stash config on a TLS stack
 *   kernel_host_stub(args);                                     // the host stub nvcc emits
 *     -> __cudaPopCallConfiguration(&g, &b, &sh, &st);          // recover it
 *     -> cudaLaunchKernel((void*)kernel_host_stub, g, b, args, sh, st);
 *   cudaLaunchKernel maps the host-stub pointer -> registered fatbin handle -> extracted PTX module
 *   (cuModuleLoadData) -> cuModuleGetFunction(deviceName) -> cuLaunchKernel on the driver.
 *
 * Fatbin -> PTX: the driver's cuModuleLoadData now walks the fatbin (fatbin.h) and extracts the
 * embedded UNCOMPRESSED PTX, so __cudaRegisterFatBinary just forwards the fatCubin to it. SASS-only or
 * compressed fatbins have no usable PTX and surface a clean cudaErrorInvalidKernelImage (never a crash).
 *
 * Runtime is thin: it forwards to libcuda (DT_NEEDED), which runs the PTX on dd's software backend.
 */
#define _GNU_SOURCE
#include "cudart_min.h"
#include "cuda_min.h"      /* driver types (CUresult, CUdeviceptr, CUstream, ...) */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* =================================================================================================
 * driver entry points we call (declared here; resolved from libcuda at load time via DT_NEEDED)
 * ================================================================================================= */
extern CUresult cuInit(unsigned int);
extern CUresult cuDriverGetVersion(int*);
extern CUresult cuDeviceGetCount(int*);
extern CUresult cuDeviceGet(CUdevice*, int);
extern CUresult cuDeviceGetName(char*, int, CUdevice);
extern CUresult cuDeviceTotalMem_v2(size_t*, CUdevice);
extern CUresult cuDeviceComputeCapability(int*, int*, CUdevice);
extern CUresult cuDeviceGetAttribute(int*, CUdevice_attribute, CUdevice);
extern CUresult cuDevicePrimaryCtxRetain(CUcontext*, CUdevice);
extern CUresult cuCtxSetCurrent(CUcontext);
extern CUresult cuCtxSynchronize(void);
extern CUresult cuMemAlloc_v2(CUdeviceptr*, size_t);
extern CUresult cuMemFree_v2(CUdeviceptr);
extern CUresult cuMemGetInfo_v2(size_t*, size_t*);
extern CUresult cuMemcpy(CUdeviceptr, CUdeviceptr, size_t);
extern CUresult cuMemcpyHtoD_v2(CUdeviceptr, const void*, size_t);
extern CUresult cuMemcpyDtoH_v2(void*, CUdeviceptr, size_t);
extern CUresult cuMemcpyDtoD_v2(CUdeviceptr, CUdeviceptr, size_t);
extern CUresult cuMemsetD8_v2(CUdeviceptr, unsigned char, size_t);
extern CUresult cuStreamCreate(CUstream*, unsigned int);
extern CUresult cuStreamDestroy_v2(CUstream);
extern CUresult cuStreamSynchronize(CUstream);
extern CUresult cuEventCreate(CUevent*, unsigned int);
extern CUresult cuEventDestroy_v2(CUevent);
extern CUresult cuEventRecord(CUevent, CUstream);
extern CUresult cuEventSynchronize(CUevent);
extern CUresult cuEventElapsedTime(float*, CUevent, CUevent);
extern CUresult cuModuleLoadData(CUmodule*, const void*);
extern CUresult cuModuleGetFunction(CUfunction*, CUmodule, const char*);
extern CUresult cuModuleUnload(CUmodule);
extern CUresult cuLaunchKernel(CUfunction, unsigned, unsigned, unsigned, unsigned, unsigned, unsigned,
                               unsigned, CUstream, void**, void**);

/* =================================================================================================
 * CUresult -> cudaError_t map (cudart returns a DIFFERENT enum from the driver)
 * ================================================================================================= */
static cudaError_t map_err(CUresult r) {
    switch (r) {
        case CUDA_SUCCESS:                     return cudaSuccess;
        case CUDA_ERROR_INVALID_VALUE:         return cudaErrorInvalidValue;
        case CUDA_ERROR_OUT_OF_MEMORY:         return cudaErrorMemoryAllocation;
        case CUDA_ERROR_NOT_INITIALIZED:       return cudaErrorInitializationError;
        case CUDA_ERROR_DEINITIALIZED:         return cudaErrorCudartUnloading;
        case CUDA_ERROR_PROFILER_DISABLED:     return cudaErrorProfilerDisabled;
        case CUDA_ERROR_NO_DEVICE:             return cudaErrorNoDevice;
        case CUDA_ERROR_INVALID_DEVICE:        return cudaErrorInvalidDevice;
        case CUDA_ERROR_DEVICE_NOT_LICENSED:   return cudaErrorDeviceNotLicensed;
        case CUDA_ERROR_INVALID_IMAGE:         return cudaErrorInvalidKernelImage;
        case CUDA_ERROR_INVALID_CONTEXT:       return cudaErrorDeviceUninitialized;
        case CUDA_ERROR_NO_BINARY_FOR_GPU:     return cudaErrorNoKernelImageForDevice;
        case CUDA_ERROR_INVALID_PTX:           return cudaErrorInvalidPtx;
        case CUDA_ERROR_UNSUPPORTED_PTX_VERSION:return cudaErrorUnsupportedPtxVersion;
        case CUDA_ERROR_INVALID_SOURCE:        return cudaErrorInvalidSource;
        case CUDA_ERROR_FILE_NOT_FOUND:        return cudaErrorFileNotFound;
        case CUDA_ERROR_OPERATING_SYSTEM:      return cudaErrorOperatingSystem;
        case CUDA_ERROR_INVALID_HANDLE:        return cudaErrorInvalidResourceHandle;
        case CUDA_ERROR_ILLEGAL_STATE:         return cudaErrorIllegalState;
        case CUDA_ERROR_NOT_FOUND:             return cudaErrorSymbolNotFound;
        case CUDA_ERROR_NOT_READY:             return cudaErrorNotReady;
        case CUDA_ERROR_ILLEGAL_ADDRESS:       return cudaErrorIllegalAddress;
        case CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES:return cudaErrorLaunchOutOfResources;
        case CUDA_ERROR_LAUNCH_TIMEOUT:        return cudaErrorLaunchTimeout;
        case CUDA_ERROR_LAUNCH_FAILED:         return cudaErrorLaunchFailure;
        case CUDA_ERROR_NOT_PERMITTED:         return cudaErrorNotPermitted;
        case CUDA_ERROR_NOT_SUPPORTED:         return cudaErrorNotSupported;
        default:                               return cudaErrorUnknown;
    }
}

/* =================================================================================================
 * thread-local last-error cell (cudart-owned state the driver shim does not have)
 * ================================================================================================= */
static __thread cudaError_t g_last_error = cudaSuccess;
/* record + return: a nonzero result becomes the sticky last error (drained by cudaGetLastError). */
static cudaError_t rec(cudaError_t e) { if (e != cudaSuccess) g_last_error = e; return e; }

/* =================================================================================================
 * lazy runtime init: cuInit + retain/bind the device-0 primary context (current-device TLS)
 * ================================================================================================= */
static int          g_rt_inited = 0;
static __thread int g_device = 0;

static cudaError_t ensure_init(void) {
    if (g_rt_inited) return cudaSuccess;
    CUresult r = cuInit(0);
    if (r != CUDA_SUCCESS) return rec(map_err(r));
    CUcontext ctx = NULL;
    r = cuDevicePrimaryCtxRetain(&ctx, 0);
    if (r != CUDA_SUCCESS) return rec(map_err(r));
    cuCtxSetCurrent(ctx);
    g_rt_inited = 1;
    return cudaSuccess;
}

/* =================================================================================================
 * Tier 0 — device management
 * ================================================================================================= */
cudaError_t cudaGetDeviceCount(int* count) {
    if (!count) return rec(cudaErrorInvalidValue);
    if (ensure_init() != cudaSuccess) { *count = 0; return g_last_error; }
    return rec(map_err(cuDeviceGetCount(count)));
}
cudaError_t cudaSetDevice(int device) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    if (device != 0) return rec(cudaErrorInvalidDevice);
    CUcontext ctx = NULL;
    CUresult r = cuDevicePrimaryCtxRetain(&ctx, 0);
    if (r != CUDA_SUCCESS) return rec(map_err(r));
    cuCtxSetCurrent(ctx);
    g_device = device;
    return rec(cudaSuccess);
}
cudaError_t cudaGetDevice(int* device) {
    if (!device) return rec(cudaErrorInvalidValue);
    if (ensure_init() != cudaSuccess) return g_last_error;
    *device = g_device;
    return rec(cudaSuccess);
}
cudaError_t cudaDeviceSynchronize(void) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuCtxSynchronize()));
}
static cudaError_t fill_props(struct cudaDeviceProp* p, int device) {
    if (!p || device != 0) return cudaErrorInvalidDevice;
    memset(p, 0, sizeof(*p));
    CUdevice dev = 0;
    if (cuDeviceGet(&dev, device) != CUDA_SUCCESS) return cudaErrorInvalidDevice;
    cuDeviceGetName(p->name, (int)sizeof(p->name), dev);
    size_t total = 0; cuDeviceTotalMem_v2(&total, dev); p->totalGlobalMem = total;
    cuDeviceComputeCapability(&p->major, &p->minor, dev);
    int v = 0;
#define ATTR(field, a) do { if (cuDeviceGetAttribute(&v, (a), dev) == CUDA_SUCCESS) p->field = v; } while (0)
    ATTR(warpSize,                    CU_DEVICE_ATTRIBUTE_WARP_SIZE);
    ATTR(maxThreadsPerBlock,          CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK);
    ATTR(maxThreadsDim[0],            CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X);
    ATTR(maxThreadsDim[1],            CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y);
    ATTR(maxThreadsDim[2],            CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z);
    ATTR(maxGridSize[0],              CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X);
    ATTR(maxGridSize[1],              CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y);
    ATTR(maxGridSize[2],              CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z);
    ATTR(regsPerBlock,                CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK);
    ATTR(clockRate,                   CU_DEVICE_ATTRIBUTE_CLOCK_RATE);
    ATTR(multiProcessorCount,         CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT);
    ATTR(integrated,                  CU_DEVICE_ATTRIBUTE_INTEGRATED);
    ATTR(canMapHostMemory,            CU_DEVICE_ATTRIBUTE_CAN_MAP_HOST_MEMORY);
    ATTR(computeMode,                 CU_DEVICE_ATTRIBUTE_COMPUTE_MODE);
    ATTR(concurrentKernels,           CU_DEVICE_ATTRIBUTE_CONCURRENT_KERNELS);
    ATTR(ECCEnabled,                  CU_DEVICE_ATTRIBUTE_ECC_ENABLED);
    ATTR(memoryClockRate,             CU_DEVICE_ATTRIBUTE_MEMORY_CLOCK_RATE);
    ATTR(memoryBusWidth,              CU_DEVICE_ATTRIBUTE_GLOBAL_MEMORY_BUS_WIDTH);
    ATTR(l2CacheSize,                 CU_DEVICE_ATTRIBUTE_L2_CACHE_SIZE);
    ATTR(maxThreadsPerMultiProcessor, CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_MULTIPROCESSOR);
    ATTR(asyncEngineCount,            CU_DEVICE_ATTRIBUTE_ASYNC_ENGINE_COUNT);
    ATTR(unifiedAddressing,           CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING);
    ATTR(managedMemory,               CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY);
    ATTR(concurrentManagedAccess,     CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS);
    ATTR(pageableMemoryAccess,        CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS);
#undef ATTR
    if (cuDeviceGetAttribute(&v, CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK, dev) == CUDA_SUCCESS)
        p->sharedMemPerBlock = (size_t)v;
    if (cuDeviceGetAttribute(&v, CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY, dev) == CUDA_SUCCESS)
        p->totalConstMem = (size_t)v;
    if (cuDeviceGetAttribute(&v, CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR, dev) == CUDA_SUCCESS)
        p->sharedMemPerMultiprocessor = (size_t)v;
    p->textureAlignment = 512;
    return cudaSuccess;
}
cudaError_t cudaGetDeviceProperties_v2(struct cudaDeviceProp* prop, int device) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(fill_props(prop, device));
}
cudaError_t cudaGetDeviceProperties(struct cudaDeviceProp* prop, int device) {
    return cudaGetDeviceProperties_v2(prop, device);
}

/* =================================================================================================
 * Tier 0 — error reporting
 * ================================================================================================= */
cudaError_t cudaGetLastError(void) { cudaError_t e = g_last_error; g_last_error = cudaSuccess; return e; }
cudaError_t cudaPeekAtLastError(void) { return g_last_error; }

const char* cudaGetErrorName(cudaError_t e) {
    switch (e) {
        case cudaSuccess:                     return "cudaSuccess";
        case cudaErrorInvalidValue:           return "cudaErrorInvalidValue";
        case cudaErrorMemoryAllocation:       return "cudaErrorMemoryAllocation";
        case cudaErrorInitializationError:    return "cudaErrorInitializationError";
        case cudaErrorCudartUnloading:        return "cudaErrorCudartUnloading";
        case cudaErrorInvalidConfiguration:   return "cudaErrorInvalidConfiguration";
        case cudaErrorInvalidMemcpyDirection: return "cudaErrorInvalidMemcpyDirection";
        case cudaErrorInvalidDeviceFunction:  return "cudaErrorInvalidDeviceFunction";
        case cudaErrorNoDevice:               return "cudaErrorNoDevice";
        case cudaErrorInvalidDevice:          return "cudaErrorInvalidDevice";
        case cudaErrorInvalidKernelImage:     return "cudaErrorInvalidKernelImage";
        case cudaErrorDeviceUninitialized:    return "cudaErrorDeviceUninitialized";
        case cudaErrorNoKernelImageForDevice: return "cudaErrorNoKernelImageForDevice";
        case cudaErrorInvalidPtx:             return "cudaErrorInvalidPtx";
        case cudaErrorInvalidResourceHandle:  return "cudaErrorInvalidResourceHandle";
        case cudaErrorSymbolNotFound:         return "cudaErrorSymbolNotFound";
        case cudaErrorNotReady:               return "cudaErrorNotReady";
        case cudaErrorIllegalAddress:         return "cudaErrorIllegalAddress";
        case cudaErrorLaunchOutOfResources:   return "cudaErrorLaunchOutOfResources";
        case cudaErrorLaunchFailure:          return "cudaErrorLaunchFailure";
        case cudaErrorNotSupported:           return "cudaErrorNotSupported";
        case cudaErrorUnknown:                return "cudaErrorUnknown";
        default:                              return "cudaErrorUnknown";
    }
}
const char* cudaGetErrorString(cudaError_t e) {
    switch (e) {
        case cudaSuccess:                     return "no error";
        case cudaErrorInvalidValue:           return "invalid argument";
        case cudaErrorMemoryAllocation:       return "out of memory";
        case cudaErrorInitializationError:    return "initialization error";
        case cudaErrorCudartUnloading:        return "driver shutting down";
        case cudaErrorInvalidConfiguration:   return "invalid configuration argument";
        case cudaErrorInvalidMemcpyDirection: return "invalid copy direction for memcpy";
        case cudaErrorInvalidDeviceFunction:  return "invalid device function";
        case cudaErrorNoDevice:               return "no CUDA-capable device is detected";
        case cudaErrorInvalidDevice:          return "invalid device ordinal";
        case cudaErrorInvalidKernelImage:     return "device kernel image is invalid";
        case cudaErrorDeviceUninitialized:    return "invalid device context";
        case cudaErrorNoKernelImageForDevice: return "no kernel image is available for execution on the device";
        case cudaErrorInvalidPtx:             return "a PTX JIT compilation failed";
        case cudaErrorInvalidResourceHandle:  return "invalid resource handle";
        case cudaErrorSymbolNotFound:         return "named symbol not found";
        case cudaErrorNotReady:               return "device not ready";
        case cudaErrorIllegalAddress:         return "an illegal memory access was encountered";
        case cudaErrorLaunchOutOfResources:   return "too many resources requested for launch";
        case cudaErrorLaunchFailure:          return "unspecified launch failure";
        case cudaErrorNotSupported:           return "operation not supported";
        case cudaErrorUnknown:                return "unknown error";
        default:                              return "unknown error";
    }
}
cudaError_t cudaDriverGetVersion(int* v) {
    if (!v) return rec(cudaErrorInvalidValue);
    return rec(map_err(cuDriverGetVersion(v)));
}
cudaError_t cudaRuntimeGetVersion(int* v) {
    if (!v) return rec(cudaErrorInvalidValue);
    *v = 12020; /* 12.2 */
    return rec(cudaSuccess);
}

/* =================================================================================================
 * Tier 0 — memory (a CUdeviceptr IS a host pointer on unified memory, so a void* device pointer works)
 * ================================================================================================= */
cudaError_t cudaMalloc(void** devPtr, size_t size) {
    if (!devPtr) return rec(cudaErrorInvalidValue);
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    CUdeviceptr dp = 0;
    CUresult r = cuMemAlloc_v2(&dp, size);
    if (r != CUDA_SUCCESS) { *devPtr = NULL; return rec(map_err(r)); }
    *devPtr = (void*)(size_t)dp;
    return rec(cudaSuccess);
}
cudaError_t cudaFree(void* devPtr) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    if (!devPtr) return rec(cudaSuccess); /* cudaFree(NULL) is a no-op success */
    return rec(map_err(cuMemFree_v2((CUdeviceptr)(size_t)devPtr)));
}
cudaError_t cudaMemcpy(void* dst, const void* src, size_t count, enum cudaMemcpyKind kind) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    CUresult r;
    switch (kind) {
        case cudaMemcpyHostToDevice:   r = cuMemcpyHtoD_v2((CUdeviceptr)(size_t)dst, src, count); break;
        case cudaMemcpyDeviceToHost:   r = cuMemcpyDtoH_v2(dst, (CUdeviceptr)(size_t)src, count); break;
        case cudaMemcpyDeviceToDevice: r = cuMemcpyDtoD_v2((CUdeviceptr)(size_t)dst, (CUdeviceptr)(size_t)src, count); break;
        case cudaMemcpyHostToHost: {
            if (count && (!dst || !src)) return rec(cudaErrorInvalidValue);
            if (count) memcpy(dst, src, count);
            r = CUDA_SUCCESS;
            break;
        }
        case cudaMemcpyDefault:        r = cuMemcpy((CUdeviceptr)(size_t)dst, (CUdeviceptr)(size_t)src, count); break;
        default:                       return rec(cudaErrorInvalidMemcpyDirection);
    }
    return rec(map_err(r));
}
cudaError_t cudaMemcpyAsync(void* dst, const void* src, size_t count, enum cudaMemcpyKind kind,
                            cudaStream_t stream) {
    (void)stream; /* synchronous executor: async == sync */
    return cudaMemcpy(dst, src, count, kind);
}
cudaError_t cudaMemset(void* devPtr, int value, size_t count) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuMemsetD8_v2((CUdeviceptr)(size_t)devPtr, (unsigned char)value, count)));
}
cudaError_t cudaMemsetAsync(void* devPtr, int value, size_t count, cudaStream_t stream) {
    (void)stream; return cudaMemset(devPtr, value, count);
}
cudaError_t cudaMemGetInfo(size_t* freeB, size_t* totalB) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuMemGetInfo_v2(freeB, totalB)));
}

/* =================================================================================================
 * Tier 0 — streams / events (a runtime handle IS the driver handle)
 * ================================================================================================= */
cudaError_t cudaStreamCreate(cudaStream_t* pStream) {
    if (!pStream) return rec(cudaErrorInvalidValue);
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuStreamCreate((CUstream*)pStream, 0)));
}
cudaError_t cudaStreamCreateWithFlags(cudaStream_t* pStream, unsigned int flags) {
    if (!pStream) return rec(cudaErrorInvalidValue);
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuStreamCreate((CUstream*)pStream, flags)));
}
cudaError_t cudaStreamDestroy(cudaStream_t stream) {
    return rec(map_err(cuStreamDestroy_v2((CUstream)stream)));
}
cudaError_t cudaStreamSynchronize(cudaStream_t stream) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuStreamSynchronize((CUstream)stream)));
}
cudaError_t cudaEventCreate(cudaEvent_t* event) {
    if (!event) return rec(cudaErrorInvalidValue);
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuEventCreate((CUevent*)event, 0)));
}
cudaError_t cudaEventCreateWithFlags(cudaEvent_t* event, unsigned int flags) {
    if (!event) return rec(cudaErrorInvalidValue);
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuEventCreate((CUevent*)event, flags)));
}
cudaError_t cudaEventDestroy(cudaEvent_t event) {
    return rec(map_err(cuEventDestroy_v2((CUevent)event)));
}
cudaError_t cudaEventRecord(cudaEvent_t event, cudaStream_t stream) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    return rec(map_err(cuEventRecord((CUevent)event, (CUstream)stream)));
}
cudaError_t cudaEventSynchronize(cudaEvent_t event) {
    return rec(map_err(cuEventSynchronize((CUevent)event)));
}
cudaError_t cudaEventElapsedTime(float* ms, cudaEvent_t start, cudaEvent_t end) {
    if (!ms) return rec(cudaErrorInvalidValue);
    return rec(map_err(cuEventElapsedTime(ms, (CUevent)start, (CUevent)end)));
}

/* =================================================================================================
 * Tier 1 — compiler registration glue + <<<>>> call-config stack + cudaLaunchKernel
 * ================================================================================================= */
/* one registered fatbin: the fatCubin nvcc handed us + the lazily-loaded PTX module. */
typedef struct {
    const void* fatcubin;   /* -> __fatBinC_Wrapper_t (driver extracts the PTX) */
    CUmodule    module;     /* lazily loaded on first launch */
    int         loaded;
    CUresult    load_res;
} FatBin;

/* host-stub pointer -> (fatbin handle, device kernel name). */
typedef struct {
    const void* host_fun;
    FatBin*     fat;
    char        name[256];
    CUfunction  fn;         /* cached cuModuleGetFunction result */
    int         have_fn;
} FuncReg;

#define FUNC_MAX 4096
static FuncReg g_funcs[FUNC_MAX];
static int     g_nfunc = 0;

/* thread-local <<<grid,block,shmem,stream>>> call-config stack. */
typedef struct { dim3 grid, block; size_t shmem; void* stream; } CallCfg;
#define CFG_MAX 32
static __thread CallCfg g_cfg[CFG_MAX];
static __thread int     g_cfg_sp = 0;

void** __cudaRegisterFatBinary(void* fatCubin) {
    ensure_init();
    FatBin* fb = (FatBin*)calloc(1, sizeof(*fb));
    if (!fb) return NULL;
    fb->fatcubin = fatCubin;
    return (void**)fb; /* opaque handle */
}
void __cudaRegisterFatBinaryEnd(void** fatCubinHandle) { (void)fatCubinHandle; /* finalize marker */ }

void __cudaRegisterFunction(void** fatCubinHandle, const char* hostFun, char* deviceFun,
                            const char* deviceName, int thread_limit, void* tid, void* bid,
                            void* bDim, void* gDim, int* wSize) {
    (void)deviceFun; (void)thread_limit; (void)tid; (void)bid; (void)bDim; (void)gDim; (void)wSize;
    if (g_nfunc >= FUNC_MAX) return;
    FuncReg* fr = &g_funcs[g_nfunc++];
    fr->host_fun = (const void*)hostFun;
    fr->fat      = (FatBin*)fatCubinHandle;
    fr->have_fn  = 0;
    snprintf(fr->name, sizeof(fr->name), "%s", deviceName ? deviceName : "");
}
void __cudaRegisterVar(void** fatCubinHandle, char* hostVar, char* deviceAddress,
                       const char* deviceName, int ext, size_t size, int constant, int global) {
    /* dd's PTX model parses only kernel entries, not .global variables -> record but nothing to bind. */
    (void)fatCubinHandle; (void)hostVar; (void)deviceAddress; (void)deviceName;
    (void)ext; (void)size; (void)constant; (void)global;
}
void __cudaUnregisterFatBinary(void** fatCubinHandle) {
    FatBin* fb = (FatBin*)fatCubinHandle;
    if (!fb) return;
    if (fb->loaded && fb->module) cuModuleUnload(fb->module);
    /* drop any function registrations pointing at this fatbin */
    for (int i = 0; i < g_nfunc; i++) if (g_funcs[i].fat == fb) g_funcs[i].fat = NULL;
    free(fb);
}

unsigned int __cudaPushCallConfiguration(dim3 gridDim, dim3 blockDim, size_t sharedMem, void* stream) {
    if (g_cfg_sp >= CFG_MAX) return 1; /* nonzero -> generated stub skips the launch */
    g_cfg[g_cfg_sp].grid = gridDim; g_cfg[g_cfg_sp].block = blockDim;
    g_cfg[g_cfg_sp].shmem = sharedMem; g_cfg[g_cfg_sp].stream = stream;
    g_cfg_sp++;
    return 0; /* success: proceed to the host stub */
}
cudaError_t __cudaPopCallConfiguration(dim3* gridDim, dim3* blockDim, size_t* sharedMem, void* stream) {
    if (g_cfg_sp <= 0) return cudaErrorInvalidConfiguration;
    CallCfg* c = &g_cfg[--g_cfg_sp];
    if (gridDim)   *gridDim = c->grid;
    if (blockDim)  *blockDim = c->block;
    if (sharedMem) *sharedMem = c->shmem;
    if (stream)    *(void**)stream = c->stream; /* stream arg points at a cudaStream_t slot */
    return cudaSuccess;
}

cudaError_t cudaLaunchKernel(const void* func, dim3 gridDim, dim3 blockDim, void** args,
                             size_t sharedMem, cudaStream_t stream) {
    cudaError_t e = ensure_init(); if (e != cudaSuccess) return e;
    /* resolve the host-stub pointer -> registered fatbin + device kernel name */
    FuncReg* fr = NULL;
    for (int i = 0; i < g_nfunc; i++) if (g_funcs[i].host_fun == func) { fr = &g_funcs[i]; break; }
    if (!fr || !fr->fat) return rec(cudaErrorInvalidDeviceFunction);
    FatBin* fb = fr->fat;
    /* lazily load the module (driver walks the fatbin -> extracts PTX -> compiles) */
    if (!fb->loaded) { fb->load_res = cuModuleLoadData(&fb->module, fb->fatcubin); fb->loaded = 1; }
    if (fb->load_res != CUDA_SUCCESS) return rec(map_err(fb->load_res));
    if (!fr->have_fn) {
        CUresult r = cuModuleGetFunction(&fr->fn, fb->module, fr->name);
        if (r != CUDA_SUCCESS) return rec(map_err(r));
        fr->have_fn = 1;
    }
    CUresult r = cuLaunchKernel(fr->fn, gridDim.x, gridDim.y, gridDim.z,
                                blockDim.x, blockDim.y, blockDim.z,
                                (unsigned)sharedMem, (CUstream)stream, args, NULL);
    return rec(map_err(r));
}
