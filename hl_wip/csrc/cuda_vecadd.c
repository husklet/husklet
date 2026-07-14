/* REAL SOFTWARE #4 — a real C program that drives the CUDA Driver API through our staged libcuda.so.
 *
 * Compiled at test time with `gcc -lcuda` against ~/.hl/cuda/aarch64, this is an ordinary CUDA host
 * program: cuInit -> cuDeviceGet -> cuCtxCreate -> cuModuleLoadData(PTX) -> cuModuleGetFunction ->
 * cuMemAlloc x3 -> cuMemcpyHtoD x2 -> cuLaunchKernel(vecadd) -> cuMemcpyDtoH -> assert c == a + b.
 * Every cu* call enters our shim, which lowers the op and ships it over $HL_GPU_EXEC to the host
 * executor. Prints "RESULT: <c0> <c1> <c2> <c3>" and the device name; exits 0 only if the sum is right. */
#include <stdio.h>
#include <string.h>
#include "cuda_min.h"

/* The canonical vecadd PTX (sm_86) — the exact source hl_cuda::adapter::ptx::VECADD_PTX carries, so the
 * host executor's injected PTX front-end compiles the identical kernel. */
static const char *VECADD_PTX =
    ".version 7.5\n"
    ".target sm_86\n"
    ".address_size 64\n"
    ".visible .entry vecadd(\n"
    "    .param .u64 vecadd_param_0,\n"
    "    .param .u64 vecadd_param_1,\n"
    "    .param .u64 vecadd_param_2,\n"
    "    .param .u32 vecadd_param_3\n"
    ") {\n"
    "    .reg .pred  %p<2>;\n"
    "    .reg .f32   %f<4>;\n"
    "    .reg .b32   %r<6>;\n"
    "    .reg .b64   %rd<11>;\n"
    "    ld.param.u64  %rd1, [vecadd_param_0];\n"
    "    ld.param.u64  %rd2, [vecadd_param_1];\n"
    "    ld.param.u64  %rd3, [vecadd_param_2];\n"
    "    ld.param.u32  %r2,  [vecadd_param_3];\n"
    "    mov.u32       %r3, %ntid.x;\n"
    "    mov.u32       %r4, %ctaid.x;\n"
    "    mov.u32       %r5, %tid.x;\n"
    "    mad.lo.s32    %r1, %r4, %r3, %r5;\n"
    "    setp.ge.s32   %p1, %r1, %r2;\n"
    "    @%p1 bra      DONE;\n"
    "    cvta.to.global.u64 %rd4, %rd1;\n"
    "    cvta.to.global.u64 %rd5, %rd2;\n"
    "    cvta.to.global.u64 %rd6, %rd3;\n"
    "    mul.wide.s32  %rd7, %r1, 4;\n"
    "    add.s64       %rd8, %rd4, %rd7;\n"
    "    add.s64       %rd9, %rd5, %rd7;\n"
    "    add.s64       %rd10, %rd6, %rd7;\n"
    "    ld.global.f32 %f1, [%rd8];\n"
    "    ld.global.f32 %f2, [%rd9];\n"
    "    add.f32       %f3, %f1, %f2;\n"
    "    st.global.f32 [%rd10], %f3;\n"
    "DONE:\n"
    "    ret;\n"
    "}\n";

#define CK(call, what)                                                    \
    do {                                                                  \
        CUresult _e = (call);                                             \
        if (_e != CUDA_SUCCESS) {                                         \
            fprintf(stderr, "FAIL %s -> CUresult %d\n", (what), _e);      \
            return 2;                                                     \
        }                                                                 \
    } while (0)

int main(void) {
    const int N = 4;
    float a[4] = {1.f, 2.f, 3.f, 4.f};
    float b[4] = {10.f, 20.f, 30.f, 40.f};
    float c[4] = {0};
    size_t bytes = N * sizeof(float);

    CK(cuInit(0), "cuInit");

    int count = -1;
    CK(cuDeviceGetCount(&count), "cuDeviceGetCount");
    printf("DEVICE_COUNT: %d\n", count);

    CUdevice dev = -1;
    CK(cuDeviceGet(&dev, 0), "cuDeviceGet");

    char name[256] = {0};
    if (cuDeviceGetName(name, sizeof(name), dev) == CUDA_SUCCESS)
        printf("DEVICE_NAME: %s\n", name);

    CUcontext ctx = 0;
    CK(cuCtxCreate(&ctx, 0, dev), "cuCtxCreate");

    CUmodule mod = 0;
    CK(cuModuleLoadData(&mod, VECADD_PTX), "cuModuleLoadData");
    CUfunction fn = 0;
    CK(cuModuleGetFunction(&fn, mod, "vecadd"), "cuModuleGetFunction");

    CUdeviceptr da = 0, db = 0, dc = 0;
    CK(cuMemAlloc(&da, bytes), "cuMemAlloc a");
    CK(cuMemAlloc(&db, bytes), "cuMemAlloc b");
    CK(cuMemAlloc(&dc, bytes), "cuMemAlloc c");
    CK(cuMemcpyHtoD(da, a, bytes), "cuMemcpyHtoD a");
    CK(cuMemcpyHtoD(db, b, bytes), "cuMemcpyHtoD b");

    int n = N;
    void *params[4] = {&da, &db, &dc, &n};
    CK(cuLaunchKernel(fn, 1, 1, 1, N, 1, 1, 0, 0, params, 0), "cuLaunchKernel");

    CK(cuMemcpyDtoH(c, dc, bytes), "cuMemcpyDtoH c");
    printf("RESULT: %.1f %.1f %.1f %.1f\n", c[0], c[1], c[2], c[3]);

    cuMemFree(da);
    cuMemFree(db);
    cuMemFree(dc);

    if (c[0] == 11.f && c[1] == 22.f && c[2] == 33.f && c[3] == 44.f) {
        printf("VECADD_OK\n");
        return 0;
    }
    fprintf(stderr, "VECADD_MISMATCH\n");
    return 1;
}
