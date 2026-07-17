/* Minimal CUDA Driver API header — the subset the real vecadd C program below uses.
 *
 * This is a faithful, hand-written stand-in for NVIDIA's <cuda.h>: it declares the driver-API entry
 * points with their real prototypes and applies the SAME versioned-symbol remapping the vendor header
 * does (e.g. `cuCtxCreate` -> `cuCtxCreate_v2`), so the C program is written against the ordinary,
 * un-suffixed names exactly as any real CUDA program is, and the linker resolves the `_v2` symbols our
 * staged libcuda.so actually exports. No NVIDIA headers are installed on this host; a real program only
 * needs these declarations to compile + link against libcuda. */
#ifndef CUDA_MIN_H
#define CUDA_MIN_H
#include <stddef.h>

typedef int            CUresult;
typedef int            CUdevice;
typedef unsigned long long CUdeviceptr; /* 64-bit device address, as in the real header */
typedef struct CUctx_st   *CUcontext;
typedef struct CUmod_st   *CUmodule;
typedef struct CUfunc_st  *CUfunction;
typedef struct CUstream_st *CUstream;

#define CUDA_SUCCESS 0

/* Versioned-symbol remapping — identical in spirit to <cuda.h>'s __CUDA_API_VERSION block. */
#define cuCtxCreate    cuCtxCreate_v2
#define cuMemAlloc     cuMemAlloc_v2
#define cuMemcpyHtoD   cuMemcpyHtoD_v2
#define cuMemcpyDtoH   cuMemcpyDtoH_v2
#define cuMemFree      cuMemFree_v2

#ifdef __cplusplus
extern "C" {
#endif

CUresult cuInit(unsigned int Flags);
CUresult cuDeviceGetCount(int *count);
CUresult cuDeviceGet(CUdevice *device, int ordinal);
CUresult cuDeviceGetName(char *name, int len, CUdevice dev);
CUresult cuCtxCreate(CUcontext *pctx, unsigned int flags, CUdevice dev);
CUresult cuModuleLoadData(CUmodule *module, const void *image);
CUresult cuModuleGetFunction(CUfunction *hfunc, CUmodule hmod, const char *name);
CUresult cuMemAlloc(CUdeviceptr *dptr, size_t bytesize);
CUresult cuMemcpyHtoD(CUdeviceptr dst, const void *src, size_t ByteCount);
CUresult cuMemcpyDtoH(void *dst, CUdeviceptr src, size_t ByteCount);
CUresult cuMemFree(CUdeviceptr dptr);
CUresult cuLaunchKernel(CUfunction f,
                        unsigned int gridDimX, unsigned int gridDimY, unsigned int gridDimZ,
                        unsigned int blockDimX, unsigned int blockDimY, unsigned int blockDimZ,
                        unsigned int sharedMemBytes, CUstream hStream,
                        void **kernelParams, void **extra);

#ifdef __cplusplus
}
#endif
#endif /* CUDA_MIN_H */
