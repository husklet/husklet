/* hl-shim-cuda end-to-end COMPUTE test — the functional milestone through the deployed cdylib.
 *
 * A plain C program (NOT linked against the shim) dlopen()s the built libcuda.so.1 and drives a real
 * CUDA vector-add exactly as an unmodified CUDA app / libcudart would: init → device → context →
 * cuMemAlloc → cuMemcpyHtoD → cuModuleLoadData(PTX) → cuModuleGetFunction → cuLaunchKernel →
 * cuMemcpyDtoH, then asserts the read-back output is arithmetically correct (c[i] == a[i] + b[i]).
 *
 * This proves the whole guest path executes end-to-end — libcuda(shim) → shared hl-gpu IR → hl-gpu
 * software backend (the CPU PTX interpreter, hl-gpu/cuda/cuda_shim.c's parity oracle) → readback — on
 * this host with NO GPU. It is the CUDA analogue of hl-shim-gl reaching parity with gl_shim.c.
 *
 *   build+run:  cc tests/compute.c -ldl -o /tmp/cuda_compute && /tmp/cuda_compute <path-to-libcuda.so>
 */
#include <dlfcn.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

typedef int CUresult; /* CUDA_SUCCESS == 0 */
typedef unsigned long long CUdeviceptr;

/* Canonical nvcc-style PTX (sm_86) for vecadd(const float* a, const float* b, float* c, int n):
 * c[i] = a[i] + b[i], with the standard mad-computed global index and an `if (i >= n) return;` guard.
 * Byte-identical to hl_gpu::ptx::VECADD_PTX (the shim's parity reference kernel). */
static const char *VECADD_PTX =
    ".version 7.5\n"
    ".target sm_86\n"
    ".address_size 64\n"
    ".visible .entry vecadd(\n"
    "    .param .u64 vecadd_param_0,\n"
    "    .param .u64 vecadd_param_1,\n"
    "    .param .u64 vecadd_param_2,\n"
    "    .param .u32 vecadd_param_3\n"
    ")\n"
    "{\n"
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
    "    @%p1 bra      $L__BB0_2;\n"
    "    cvta.to.global.u64 %rd4, %rd1;\n"
    "    mul.wide.s32  %rd5, %r1, 4;\n"
    "    add.s64       %rd6, %rd4, %rd5;\n"
    "    cvta.to.global.u64 %rd7, %rd2;\n"
    "    add.s64       %rd8, %rd7, %rd5;\n"
    "    ld.global.f32 %f1, [%rd8];\n"
    "    ld.global.f32 %f2, [%rd6];\n"
    "    add.f32       %f3, %f2, %f1;\n"
    "    cvta.to.global.u64 %rd9, %rd3;\n"
    "    add.s64       %rd10, %rd9, %rd5;\n"
    "    st.global.f32 [%rd10], %f3;\n"
    "$L__BB0_2:\n"
    "    ret;\n"
    "}\n";

#define N 1024

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <libcuda.so>\n", argv[0]);
        return 2;
    }
    void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        fprintf(stderr, "dlopen failed: %s\n", dlerror());
        return 1;
    }

    CUresult (*cuInit)(unsigned) = dlsym(h, "cuInit");
    CUresult (*cuDeviceGet)(int *, int) = dlsym(h, "cuDeviceGet");
    CUresult (*cuCtxCreate_v2)(void **, unsigned, int) = dlsym(h, "cuCtxCreate_v2");
    CUresult (*cuMemAlloc_v2)(CUdeviceptr *, size_t) = dlsym(h, "cuMemAlloc_v2");
    CUresult (*cuMemcpyHtoD_v2)(CUdeviceptr, const void *, size_t) = dlsym(h, "cuMemcpyHtoD_v2");
    CUresult (*cuMemcpyDtoH_v2)(void *, CUdeviceptr, size_t) = dlsym(h, "cuMemcpyDtoH_v2");
    CUresult (*cuModuleLoadData)(void **, const void *) = dlsym(h, "cuModuleLoadData");
    CUresult (*cuModuleGetFunction)(void **, void *, const char *) = dlsym(h, "cuModuleGetFunction");
    CUresult (*cuLaunchKernel)(void *, unsigned, unsigned, unsigned, unsigned, unsigned, unsigned,
                               unsigned, void *, void **, void **) = dlsym(h, "cuLaunchKernel");
    CUresult (*cuCtxSynchronize)(void) = dlsym(h, "cuCtxSynchronize");
    CUresult (*cuMemFree_v2)(CUdeviceptr) = dlsym(h, "cuMemFree_v2");

    if (!cuInit || !cuDeviceGet || !cuCtxCreate_v2 || !cuMemAlloc_v2 || !cuMemcpyHtoD_v2 ||
        !cuMemcpyDtoH_v2 || !cuModuleLoadData || !cuModuleGetFunction || !cuLaunchKernel ||
        !cuCtxSynchronize || !cuMemFree_v2) {
        fprintf(stderr, "dlsym: a compute entry point is missing\n");
        return 1;
    }

#define OK(call)                                                                                   \
    do {                                                                                           \
        CUresult _r = (call);                                                                      \
        if (_r != 0) {                                                                             \
            fprintf(stderr, "%s -> %d\n", #call, _r);                                              \
            return 1;                                                                              \
        }                                                                                          \
    } while (0)

    OK(cuInit(0));
    int dev = -1;
    OK(cuDeviceGet(&dev, 0));
    void *ctx = NULL;
    OK(cuCtxCreate_v2(&ctx, 0, dev));

    float ha[N], hb[N], hc[N];
    for (int i = 0; i < N; i++) {
        ha[i] = (float)i;
        hb[i] = (float)(N - i) * 0.25f;
    }

    CUdeviceptr da = 0, db = 0, dc = 0;
    OK(cuMemAlloc_v2(&da, sizeof ha));
    OK(cuMemAlloc_v2(&db, sizeof hb));
    OK(cuMemAlloc_v2(&dc, sizeof hc));
    if (!da || !db || !dc) { fprintf(stderr, "cuMemAlloc gave a null device pointer\n"); return 1; }

    OK(cuMemcpyHtoD_v2(da, ha, sizeof ha));
    OK(cuMemcpyHtoD_v2(db, hb, sizeof hb));

    void *module = NULL, *func = NULL;
    OK(cuModuleLoadData(&module, VECADD_PTX));
    OK(cuModuleGetFunction(&func, module, "vecadd"));
    if (!func) { fprintf(stderr, "cuModuleGetFunction gave a null function\n"); return 1; }

    int n = N;
    void *params[4] = {&da, &db, &dc, &n};
    unsigned grid = (N + 255) / 256;
    OK(cuLaunchKernel(func, grid, 1, 1, 256, 1, 1, 0, NULL, params, NULL));
    OK(cuCtxSynchronize());

    memset(hc, 0, sizeof hc);
    OK(cuMemcpyDtoH_v2(hc, dc, sizeof hc));

    int bad = 0;
    for (int i = 0; i < N; i++) {
        float want = ha[i] + hb[i];
        if (fabsf(hc[i] - want) > 1e-6f) {
            if (bad < 5) fprintf(stderr, "c[%d] = %g, want %g\n", i, hc[i], want);
            bad++;
        }
    }
    if (bad) { fprintf(stderr, "vecadd MISMATCH: %d / %d elements wrong\n", bad, N); return 1; }

    OK(cuMemFree_v2(da));
    OK(cuMemFree_v2(db));
    OK(cuMemFree_v2(dc));

    printf("compute OK: vecadd of %d elements correct end-to-end through libcuda.so "
           "(c[0]=%g c[512]=%g c[%d]=%g)\n",
           N, hc[0], hc[512], N - 1, hc[N - 1]);
    dlclose(h);
    return 0;
}
