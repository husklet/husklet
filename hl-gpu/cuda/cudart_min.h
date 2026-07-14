/* Clean-room CUDA Runtime API ABI declarations for dd's libcudart.so shim.
 *
 * A clean-room re-declaration of the *documented, stable* CUDA Runtime API (cuda_runtime_api.h) plus
 * the private compiler-emitted registration glue (__cudaRegister* / __cuda*CallConfiguration) that
 * nvcc targets. Enough of the surface that an unmodified CUDA app links against OUR libcudart, queries
 * the device, round-trips memory, and launches kernels delivered as fatbins. libcudart is a thin upper
 * layer over dd's libcuda Driver-API shim (docs/ideas/CUDART_PLAN.md). No NVIDIA source is used.
 *
 * The load-bearing parts — the cudaError_t enum values, the cudaMemcpyKind values, the dim3 layout,
 * and the cudaLaunchKernel / __cuda* calling conventions — match NVIDIA's published headers so the ABI
 * is compatible. cudaDeviceProp is a faithful clean-room reconstruction of the CUDA 12.x layout.
 */
#ifndef HL_CUDART_MIN_H
#define HL_CUDART_MIN_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* --- runtime result codes (values from driver_types.h) --- */
typedef enum cudaError {
    cudaSuccess                       = 0,
    cudaErrorInvalidValue             = 1,
    cudaErrorMemoryAllocation         = 2,
    cudaErrorInitializationError      = 3,
    cudaErrorCudartUnloading          = 4,
    cudaErrorProfilerDisabled         = 5,
    cudaErrorInvalidConfiguration     = 9,
    cudaErrorInvalidPitchValue        = 12,
    cudaErrorInvalidSymbol            = 13,
    cudaErrorInvalidDevicePointer     = 17,
    cudaErrorInvalidMemcpyDirection   = 21,
    cudaErrorInsufficientDriver       = 35,
    cudaErrorInvalidDeviceFunction    = 98,
    cudaErrorNoDevice                 = 100,
    cudaErrorInvalidDevice            = 101,
    cudaErrorDeviceNotLicensed        = 102,
    cudaErrorInvalidKernelImage       = 200,
    cudaErrorDeviceUninitialized      = 201,
    cudaErrorNoKernelImageForDevice   = 209,
    cudaErrorInvalidPtx               = 218,
    cudaErrorUnsupportedPtxVersion    = 222,
    cudaErrorInvalidSource            = 300,
    cudaErrorFileNotFound             = 301,
    cudaErrorOperatingSystem          = 304,
    cudaErrorInvalidResourceHandle    = 400,
    cudaErrorIllegalState             = 401,
    cudaErrorSymbolNotFound           = 500,
    cudaErrorNotReady                 = 600,
    cudaErrorIllegalAddress           = 700,
    cudaErrorLaunchOutOfResources     = 701,
    cudaErrorLaunchTimeout            = 702,
    cudaErrorLaunchFailure            = 719,
    cudaErrorNotPermitted             = 800,
    cudaErrorNotSupported             = 801,
    cudaErrorUnknown                  = 999
} cudaError_t;

/* --- memcpy direction --- */
enum cudaMemcpyKind {
    cudaMemcpyHostToHost     = 0,
    cudaMemcpyHostToDevice   = 1,
    cudaMemcpyDeviceToHost   = 2,
    cudaMemcpyDeviceToDevice = 3,
    cudaMemcpyDefault        = 4
};

/* --- grid/block dimensions (C ABI: three unsigned ints) --- */
typedef struct dim3 { unsigned int x, y, z; } dim3;

/* --- opaque handles (a runtime stream/event IS a driver CUstream/CUevent under the hood) --- */
typedef struct CUstream_st* cudaStream_t;
typedef struct CUevent_st*  cudaEvent_t;

typedef struct cudaUUID_t { char bytes[16]; } cudaUUID_t;

/* --- cudaDeviceProp: faithful clean-room reconstruction of the CUDA 12.x layout --- */
struct cudaDeviceProp {
    char           name[256];
    cudaUUID_t     uuid;
    char           luid[8];
    unsigned int   luidDeviceNodeMask;
    size_t         totalGlobalMem;
    size_t         sharedMemPerBlock;
    int            regsPerBlock;
    int            warpSize;
    size_t         memPitch;
    int            maxThreadsPerBlock;
    int            maxThreadsDim[3];
    int            maxGridSize[3];
    int            clockRate;
    size_t         totalConstMem;
    int            major;
    int            minor;
    size_t         textureAlignment;
    size_t         texturePitchAlignment;
    int            deviceOverlap;
    int            multiProcessorCount;
    int            kernelExecTimeoutEnabled;
    int            integrated;
    int            canMapHostMemory;
    int            computeMode;
    int            maxTexture1D;
    int            maxTexture1DMipmap;
    int            maxTexture1DLinear;
    int            maxTexture2D[2];
    int            maxTexture2DMipmap[2];
    int            maxTexture2DLinear[3];
    int            maxTexture2DGather[2];
    int            maxTexture3D[3];
    int            maxTexture3DAlt[3];
    int            maxTextureCubemap;
    int            maxTexture1DLayered[2];
    int            maxTexture2DLayered[3];
    int            maxTextureCubemapLayered[2];
    int            maxSurface1D;
    int            maxSurface2D[2];
    int            maxSurface3D[3];
    int            maxSurface1DLayered[2];
    int            maxSurface2DLayered[3];
    int            maxSurfaceCubemap;
    int            maxSurfaceCubemapLayered[2];
    size_t         surfaceAlignment;
    int            concurrentKernels;
    int            ECCEnabled;
    int            pciBusID;
    int            pciDeviceID;
    int            pciDomainID;
    int            tccDriver;
    int            asyncEngineCount;
    int            unifiedAddressing;
    int            memoryClockRate;
    int            memoryBusWidth;
    int            l2CacheSize;
    int            persistingL2CacheMaxSize;
    int            maxThreadsPerMultiProcessor;
    int            streamPrioritiesSupported;
    int            globalL1CacheSupported;
    int            localL1CacheSupported;
    size_t         sharedMemPerMultiprocessor;
    int            regsPerMultiprocessor;
    int            managedMemory;
    int            isMultiGpuBoard;
    int            multiGpuBoardGroupID;
    int            hostNativeAtomicSupported;
    int            singleToDoublePrecisionPerfRatio;
    int            pageableMemoryAccess;
    int            concurrentManagedAccess;
    int            computePreemptionSupported;
    int            canUseHostPointerForRegisteredMem;
    int            cooperativeLaunch;
    int            cooperativeMultiDeviceLaunch;
    size_t         sharedMemPerBlockOptin;
    int            pageableMemoryAccessUsesHostPageTables;
    int            directManagedMemAccessFromHost;
    int            maxBlocksPerMultiProcessor;
    int            accessPolicyMaxWindowSize;
    size_t         reservedSharedMemPerBlock;
    int            hostRegisterSupported;
    int            sparseCudaArraySupported;
    int            hostRegisterReadOnlySupported;
    int            timelineSemaphoreInteropSupported;
    int            memoryPoolsSupported;
    int            gpuDirectRDMASupported;
    unsigned int   gpuDirectRDMAFlushWritesOptions;
    int            gpuDirectRDMAWritesOrdering;
    unsigned int   memoryPoolSupportedHandleTypes;
    int            deferredMappingCudaArraySupported;
    int            ipcEventSupported;
    int            clusterLaunch;
    int            unifiedFunctionPointers;
    int            reserved2[2];
    int            reserved1[1];
    int            reserved[60];
};

/* =================================================================================================
 * Tier 0 — stateful runtime API
 * ================================================================================================= */
cudaError_t cudaGetDeviceCount(int* count);
cudaError_t cudaSetDevice(int device);
cudaError_t cudaGetDevice(int* device);
cudaError_t cudaDeviceSynchronize(void);
cudaError_t cudaGetDeviceProperties(struct cudaDeviceProp* prop, int device);
cudaError_t cudaGetDeviceProperties_v2(struct cudaDeviceProp* prop, int device);

cudaError_t cudaGetLastError(void);
cudaError_t cudaPeekAtLastError(void);
const char* cudaGetErrorString(cudaError_t error);
const char* cudaGetErrorName(cudaError_t error);
cudaError_t cudaDriverGetVersion(int* driverVersion);
cudaError_t cudaRuntimeGetVersion(int* runtimeVersion);

cudaError_t cudaMalloc(void** devPtr, size_t size);
cudaError_t cudaFree(void* devPtr);
cudaError_t cudaMemcpy(void* dst, const void* src, size_t count, enum cudaMemcpyKind kind);
cudaError_t cudaMemcpyAsync(void* dst, const void* src, size_t count, enum cudaMemcpyKind kind,
                            cudaStream_t stream);
cudaError_t cudaMemset(void* devPtr, int value, size_t count);
cudaError_t cudaMemsetAsync(void* devPtr, int value, size_t count, cudaStream_t stream);
cudaError_t cudaMemGetInfo(size_t* free, size_t* total);

cudaError_t cudaStreamCreate(cudaStream_t* pStream);
cudaError_t cudaStreamCreateWithFlags(cudaStream_t* pStream, unsigned int flags);
cudaError_t cudaStreamDestroy(cudaStream_t stream);
cudaError_t cudaStreamSynchronize(cudaStream_t stream);

cudaError_t cudaEventCreate(cudaEvent_t* event);
cudaError_t cudaEventCreateWithFlags(cudaEvent_t* event, unsigned int flags);
cudaError_t cudaEventDestroy(cudaEvent_t event);
cudaError_t cudaEventRecord(cudaEvent_t event, cudaStream_t stream);
cudaError_t cudaEventSynchronize(cudaEvent_t event);
cudaError_t cudaEventElapsedTime(float* ms, cudaEvent_t start, cudaEvent_t end);

/* =================================================================================================
 * Tier 1 — kernel launch + compiler registration glue (the private nvcc-targeted ABI)
 * ================================================================================================= */
cudaError_t cudaLaunchKernel(const void* func, dim3 gridDim, dim3 blockDim, void** args,
                             size_t sharedMem, cudaStream_t stream);

void** __cudaRegisterFatBinary(void* fatCubin);
void   __cudaRegisterFatBinaryEnd(void** fatCubinHandle);
void   __cudaUnregisterFatBinary(void** fatCubinHandle);
void   __cudaRegisterFunction(void** fatCubinHandle, const char* hostFun, char* deviceFun,
                              const char* deviceName, int thread_limit, void* tid, void* bid,
                              void* bDim, void* gDim, int* wSize);
void   __cudaRegisterVar(void** fatCubinHandle, char* hostVar, char* deviceAddress,
                         const char* deviceName, int ext, size_t size, int constant, int global);
unsigned int __cudaPushCallConfiguration(dim3 gridDim, dim3 blockDim, size_t sharedMem, void* stream);
cudaError_t  __cudaPopCallConfiguration(dim3* gridDim, dim3* blockDim, size_t* sharedMem, void* stream);

#ifdef __cplusplus
}
#endif
#endif /* HL_CUDART_MIN_H */
