/* dd-shim-cudart end-to-end RUNTIME-API COMPUTE test — the functional milestone through libcudart.so.1.
 *
 * A plain C program (NOT linked against the shim) dlopen()s the built libcudart.so.1 and drives a real
 * CUDA runtime vector-add exactly as a compiled CUDA app would: register a fatbin through the nvcc glue
 * (__cudaRegisterFatBinary/__cudaRegisterFunction), cudaMalloc -> cudaMemcpy(HostToDevice) ->
 * <<<grid,block>>> (push-config -> host stub -> cudaLaunchKernel) -> cudaMemcpy(DeviceToHost), then
 * asserts the read-back output is arithmetically correct (c[i] == a[i] + b[i]).
 *
 * This proves the whole runtime path executes end-to-end — libcudart(shim) -> libcuda(shim) -> shared
 * dd-gpu IR -> dd-gpu software backend (CPU PTX interpreter) -> readback — on this host with NO GPU. It
 * is the runtime-API analogue of dd-gpu/cuda/test_cudart.c, self-contained (fatbin structs inlined).
 *
 *   build+run:  cc tests/compute.c -ldl -o /tmp/cudart_compute && /tmp/cudart_compute <path-to-libcudart.so>
 */
#include <dlfcn.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef int cudaError_t; /* cudaSuccess == 0 */
typedef struct dim3 { unsigned int x, y, z; } dim3;

enum { cudaMemcpyHostToDevice = 1, cudaMemcpyDeviceToHost = 2 };

/* c[i] = a[i] + b[i] — byte-identical to dd_gpu::ptx::VECADD_PTX. */
static const char *VECADD_PTX =
    ".version 7.5\n.target sm_86\n.address_size 64\n"
    ".visible .entry vecadd(\n"
    "    .param .u64 vecadd_param_0,\n"
    "    .param .u64 vecadd_param_1,\n"
    "    .param .u64 vecadd_param_2,\n"
    "    .param .u32 vecadd_param_3\n"
    ")\n{\n"
    "    .reg .pred  %p<2>;\n    .reg .f32   %f<4>;\n    .reg .b32   %r<6>;\n    .reg .b64   %rd<11>;\n"
    "    ld.param.u64  %rd1, [vecadd_param_0];\n"
    "    ld.param.u64  %rd2, [vecadd_param_1];\n"
    "    ld.param.u64  %rd3, [vecadd_param_2];\n"
    "    ld.param.u32  %r2,  [vecadd_param_3];\n"
    "    mov.u32       %r3, %ntid.x;\n    mov.u32       %r4, %ctaid.x;\n    mov.u32       %r5, %tid.x;\n"
    "    mad.lo.s32    %r1, %r4, %r3, %r5;\n"
    "    setp.ge.s32   %p1, %r1, %r2;\n"
    "    @%p1 bra      $L__BB0_2;\n"
    "    cvta.to.global.u64 %rd4, %rd1;\n    mul.wide.s32  %rd5, %r1, 4;\n    add.s64       %rd6, %rd4, %rd5;\n"
    "    cvta.to.global.u64 %rd7, %rd2;\n    add.s64       %rd8, %rd7, %rd5;\n"
    "    ld.global.f32 %f1, [%rd8];\n    ld.global.f32 %f2, [%rd6];\n    add.f32       %f3, %f2, %f1;\n"
    "    cvta.to.global.u64 %rd9, %rd3;\n    add.s64       %rd10, %rd9, %rd5;\n"
    "    st.global.f32 [%rd10], %f3;\n"
    "$L__BB0_2:\n    ret;\n}\n";

#define N 1024

/* --- inline synthetic uncompressed fatbin (container magic 0xBA55ED50, kind=1 PTX), matching
 *     dd-gpu/cuda/fatbin.h; the runtime shim's fatbin walker extracts the PTX from it. --- */
static unsigned char g_fatbin[4096];
static size_t build_fatbin(const char *ptx) {
    size_t plen = strlen(ptx) + 1; /* NUL-terminated payload */
    size_t fat_size = 64 + plen;
    memset(g_fatbin, 0, sizeof g_fatbin);
    /* header (16B) */
    uint32_t magic = 0xba55ed50u; memcpy(g_fatbin + 0, &magic, 4);
    uint16_t v1 = 1, hs = 16;      memcpy(g_fatbin + 4, &v1, 2); memcpy(g_fatbin + 6, &hs, 2);
    uint64_t fs = fat_size;        memcpy(g_fatbin + 8, &fs, 8);
    /* entry header (64B) at offset 16 */
    uint16_t kind = 1;             memcpy(g_fatbin + 16, &kind, 2); memcpy(g_fatbin + 18, &v1, 2);
    uint32_t ehs = 64;             memcpy(g_fatbin + 20, &ehs, 4);
    uint64_t psz = plen;           memcpy(g_fatbin + 24, &psz, 8);
    /* payload at offset 16 + 64 = 80 */
    memcpy(g_fatbin + 80, ptx, plen);
    return 80 + plen;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <libcudart.so>\n", argv[0]);
        return 2;
    }
    void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        fprintf(stderr, "dlopen failed: %s\n", dlerror());
        return 1;
    }

    cudaError_t (*cudaMalloc)(void **, size_t) = dlsym(h, "cudaMalloc");
    cudaError_t (*cudaFree)(void *) = dlsym(h, "cudaFree");
    cudaError_t (*cudaMemcpy)(void *, const void *, size_t, int) = dlsym(h, "cudaMemcpy");
    cudaError_t (*cudaDeviceSynchronize)(void) = dlsym(h, "cudaDeviceSynchronize");
    cudaError_t (*cudaGetLastError)(void) = dlsym(h, "cudaGetLastError");
    void **(*RegisterFatBinary)(void *) = dlsym(h, "__cudaRegisterFatBinary");
    void (*RegisterFatBinaryEnd)(void **) = dlsym(h, "__cudaRegisterFatBinaryEnd");
    void (*RegisterFunction)(void **, const char *, char *, const char *, int, void *, void *,
                             void *, void *, int *) = dlsym(h, "__cudaRegisterFunction");
    unsigned int (*PushCallConfiguration)(dim3, dim3, size_t, void *) =
        dlsym(h, "__cudaPushCallConfiguration");
    cudaError_t (*PopCallConfiguration)(dim3 *, dim3 *, size_t *, void *) =
        dlsym(h, "__cudaPopCallConfiguration");
    cudaError_t (*cudaLaunchKernel)(const void *, dim3, dim3, void **, size_t, void *) =
        dlsym(h, "cudaLaunchKernel");

    if (!cudaMalloc || !cudaFree || !cudaMemcpy || !cudaDeviceSynchronize || !cudaGetLastError ||
        !RegisterFatBinary || !RegisterFatBinaryEnd || !RegisterFunction || !PushCallConfiguration ||
        !PopCallConfiguration || !cudaLaunchKernel) {
        fprintf(stderr, "dlsym: a runtime entry point is missing\n");
        return 1;
    }

#define OK(call)                                                                                   \
    do {                                                                                           \
        cudaError_t _r = (call);                                                                   \
        if (_r != 0) {                                                                             \
            fprintf(stderr, "%s -> %d\n", #call, _r);                                              \
            return 1;                                                                              \
        }                                                                                          \
    } while (0)

    build_fatbin(VECADD_PTX);
    void **handle = RegisterFatBinary(g_fatbin);
    if (!handle) { fprintf(stderr, "__cudaRegisterFatBinary returned NULL\n"); return 1; }
    /* nvcc keys a kernel on its host-stub address; use a distinct static as the key. */
    static char vecadd_stub_key;
    RegisterFunction(handle, &vecadd_stub_key, "vecadd", "vecadd", -1, NULL, NULL, NULL, NULL, NULL);
    RegisterFatBinaryEnd(handle);

    float ha[N], hb[N], hc[N];
    for (int i = 0; i < N; i++) {
        ha[i] = (float)i;
        hb[i] = (float)(N - i) * 0.25f;
    }
    void *da = NULL, *db = NULL, *dc = NULL;
    OK(cudaMalloc(&da, sizeof ha));
    OK(cudaMalloc(&db, sizeof hb));
    OK(cudaMalloc(&dc, sizeof hc));
    if (!da || !db || !dc) { fprintf(stderr, "cudaMalloc gave a null device pointer\n"); return 1; }
    OK(cudaMemcpy(da, ha, sizeof ha, cudaMemcpyHostToDevice));
    OK(cudaMemcpy(db, hb, sizeof hb, cudaMemcpyHostToDevice));

    /* the <<<grid,block>>> call site nvcc lowers: push config, then the host stub pops it + launches. */
    unsigned int block = 256, grid = (N + block - 1) / block;
    dim3 gdim = { grid, 1, 1 }, bdim = { block, 1, 1 };
    if (PushCallConfiguration(gdim, bdim, 0, NULL) != 0) {
        fprintf(stderr, "__cudaPushCallConfiguration failed\n");
        return 1;
    }
    dim3 g, b; size_t sh = 0; void *st = NULL;
    OK(PopCallConfiguration(&g, &b, &sh, &st));
    int n = N;
    void *args[4] = { &da, &db, &dc, &n };
    OK(cudaLaunchKernel(&vecadd_stub_key, g, b, args, sh, st));
    OK(cudaDeviceSynchronize());

    memset(hc, 0, sizeof hc);
    OK(cudaMemcpy(hc, dc, sizeof hc, cudaMemcpyDeviceToHost));

    int bad = 0;
    for (int i = 0; i < N; i++) {
        float want = ha[i] + hb[i];
        if (fabsf(hc[i] - want) > 1e-6f) {
            if (bad < 5) fprintf(stderr, "c[%d] = %g, want %g\n", i, hc[i], want);
            bad++;
        }
    }
    if (bad) { fprintf(stderr, "vecadd MISMATCH: %d / %d elements wrong\n", bad, N); return 1; }

    OK(cudaFree(da));
    OK(cudaFree(db));
    OK(cudaFree(dc));

    printf("cudart compute OK: vecadd of %d elements correct end-to-end through libcudart.so "
           "(c[0]=%g c[512]=%g c[%d]=%g)\n",
           N, hc[0], hc[512], N - 1, hc[N - 1]);
    dlclose(h);
    return 0;
}
