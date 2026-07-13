/* Conformance harness for dd's libcuda.so.1 Driver-API shim.
 *
 * Drives the FULL guest-facing path an unmodified CUDA app (or cudart) exercises — with NO GPU —
 * across every tier of the shim:
 *   TIER 1 (real, executes on the software backend):
 *     - vecadd  : c[i]=a[i]+b[i]          (the reference kernel; bounds guard, non-multiple N)
 *     - saxpy   : y[i]=a*x[i]+y[i]        (ld.param.f32 + fma.rn.f32)
 *     - reduce  : out[i]=in[2i]+in[2i+1]  (a real pairwise reduction step; offset loads)
 *     - device-to-device copy, cuMemsetD32, async H2D on a stream, event elapsed time,
 *       cuMemAllocManaged round-trip, multi-module load.
 *   TIER 2 (semantically-correct): cuMemGetInfo, cuPointerGetAttribute, cuMemGetAddressRange,
 *       cuGetErrorName over an arbitrary code, cuOccupancyMaxActiveBlocksPerMultiprocessor.
 *   cuGetProcAddress: resolves a version-suffixed symbol (cuMemcpyHtoD_v2) AND a base name
 *       (cuMemAlloc -> _v2) and the resolved pointer actually runs.
 *   TIER 3 (present-but-honest): a graph call resolves and returns NOT_SUPPORTED; a cubin (ELF)
 *       image is rejected with INVALID_IMAGE. Present, correct error, no crash.
 * Exit 0 = all assertions pass.
 */
#define _GNU_SOURCE
#include "cuda_min.h"
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <math.h>

static int failures = 0;
#define CHECK(cond, msg) do { if (!(cond)) { fprintf(stderr, "FAIL: %s\n", msg); failures++; } \
                              else { fprintf(stderr, "ok  : %s\n", msg); } } while (0)
#define LOAD(var, name) do { *(void**)(&var) = dlsym(h, name); \
    if (!var) { fprintf(stderr, "FAIL: missing symbol %s\n", name); failures++; } } while (0)

/* c[i] = a[i] + b[i] */
static const char* VECADD_PTX =
    ".version 7.5\n.target sm_86\n.address_size 64\n"
    ".visible .entry vecadd(\n"
    "  .param .u64 vecadd_param_0, .param .u64 vecadd_param_1,\n"
    "  .param .u64 vecadd_param_2, .param .u32 vecadd_param_3\n"
    ") {\n"
    "  .reg .pred %p<2>; .reg .f32 %f<4>; .reg .b32 %r<6>; .reg .b64 %rd<11>;\n"
    "  ld.param.u64 %rd1, [vecadd_param_0];\n"
    "  ld.param.u64 %rd2, [vecadd_param_1];\n"
    "  ld.param.u64 %rd3, [vecadd_param_2];\n"
    "  ld.param.u32 %r2, [vecadd_param_3];\n"
    "  mov.u32 %r3, %ntid.x;  mov.u32 %r4, %ctaid.x;  mov.u32 %r5, %tid.x;\n"
    "  mad.lo.s32 %r1, %r4, %r3, %r5;\n"
    "  setp.ge.s32 %p1, %r1, %r2;\n"
    "  @%p1 bra $L__BB0_2;\n"
    "  cvta.to.global.u64 %rd4, %rd1;  mul.wide.s32 %rd5, %r1, 4;  add.s64 %rd6, %rd4, %rd5;\n"
    "  cvta.to.global.u64 %rd7, %rd2;  add.s64 %rd8, %rd7, %rd5;\n"
    "  ld.global.f32 %f1, [%rd8];  ld.global.f32 %f2, [%rd6];  add.f32 %f3, %f2, %f1;\n"
    "  cvta.to.global.u64 %rd9, %rd3;  add.s64 %rd10, %rd9, %rd5;\n"
    "  st.global.f32 [%rd10], %f3;\n"
    "$L__BB0_2:\n  ret;\n}\n";

/* y[i] = a*x[i] + y[i]  (SAXPY) */
static const char* SAXPY_PTX =
    ".version 7.5\n.target sm_86\n.address_size 64\n"
    ".visible .entry saxpy(\n"
    "  .param .u64 saxpy_x, .param .u64 saxpy_y, .param .f32 saxpy_a, .param .u32 saxpy_n\n"
    ") {\n"
    "  .reg .pred %p<2>; .reg .f32 %f<5>; .reg .b32 %r<6>; .reg .b64 %rd<8>;\n"
    "  ld.param.u64 %rd1, [saxpy_x];  ld.param.u64 %rd2, [saxpy_y];\n"
    "  ld.param.f32 %f1, [saxpy_a];   ld.param.u32 %r2, [saxpy_n];\n"
    "  mov.u32 %r3, %ntid.x;  mov.u32 %r4, %ctaid.x;  mov.u32 %r5, %tid.x;\n"
    "  mad.lo.s32 %r1, %r4, %r3, %r5;\n"
    "  setp.ge.s32 %p1, %r1, %r2;\n"
    "  @%p1 bra $L_END;\n"
    "  cvta.to.global.u64 %rd3, %rd1;  mul.wide.s32 %rd4, %r1, 4;  add.s64 %rd5, %rd3, %rd4;\n"
    "  cvta.to.global.u64 %rd6, %rd2;  add.s64 %rd7, %rd6, %rd4;\n"
    "  ld.global.f32 %f2, [%rd5];  ld.global.f32 %f3, [%rd7];  fma.rn.f32 %f4, %f1, %f2, %f3;\n"
    "  st.global.f32 [%rd7], %f4;\n"
    "$L_END:\n  ret;\n}\n";

/* out[i] = in[2i] + in[2i+1]  (one pairwise reduction step) */
static const char* REDUCE_PTX =
    ".version 7.5\n.target sm_86\n.address_size 64\n"
    ".visible .entry reduce_pairs(\n"
    "  .param .u64 rp_in, .param .u64 rp_out, .param .u32 rp_n\n"
    ") {\n"
    "  .reg .pred %p<2>; .reg .f32 %f<4>; .reg .b32 %r<6>; .reg .b64 %rd<9>;\n"
    "  ld.param.u64 %rd1, [rp_in];  ld.param.u64 %rd2, [rp_out];  ld.param.u32 %r2, [rp_n];\n"
    "  mov.u32 %r3, %ntid.x;  mov.u32 %r4, %ctaid.x;  mov.u32 %r5, %tid.x;\n"
    "  mad.lo.s32 %r1, %r4, %r3, %r5;\n"
    "  setp.ge.s32 %p1, %r1, %r2;\n"
    "  @%p1 bra $L_RED;\n"
    "  cvta.to.global.u64 %rd3, %rd1;  mul.wide.s32 %rd4, %r1, 8;  add.s64 %rd5, %rd3, %rd4;\n"
    "  ld.global.f32 %f1, [%rd5];  ld.global.f32 %f2, [%rd5+4];  add.f32 %f3, %f1, %f2;\n"
    "  cvta.to.global.u64 %rd6, %rd2;  mul.wide.s32 %rd7, %r1, 4;  add.s64 %rd8, %rd6, %rd7;\n"
    "  st.global.f32 [%rd8], %f3;\n"
    "$L_RED:\n  ret;\n}\n";

/* fmac(out): out[0] = fma.rn.f32(0.1, 10.0, -1.0). Fused single-rounding => ~1.49e-8; an unfused
   mul-then-add would round 0.1*10 to exactly 1.0 and collapse to 0.0. Mirrors ptx.rs. */
static const char* FMA_PTX =
    ".visible .entry fmac(.param .u64 p) {\n"
    "  .reg .f32 %f<5>; .reg .b64 %rd<3>;\n"
    "  ld.param.u64 %rd1, [p];  cvta.to.global.u64 %rd2, %rd1;\n"
    "  mov.f32 %f1, 0f3DCCCCCD;  mov.f32 %f2, 0f41200000;  mov.f32 %f3, 0fBF800000;\n"
    "  fma.rn.f32 %f4, %f1, %f2, %f3;\n"
    "  st.global.f32 [%rd2], %f4;\n  ret;\n}\n";

/* setpu(out): out[0] = (0x80000000 > 1) ? 1.0 : 0.0 under UNSIGNED compare (=> 1.0). A signed
   compare reads 0x80000000 as INT_MIN and yields 0.0. Mirrors ptx.rs. */
static const char* SETPU_PTX =
    ".visible .entry setpu(.param .u64 p) {\n"
    "  .reg .pred %p<2>; .reg .f32 %f<2>; .reg .b32 %r<3>; .reg .b64 %rd<3>;\n"
    "  ld.param.u64 %rd1, [p];  cvta.to.global.u64 %rd2, %rd1;\n"
    "  mov.u32 %r1, 2147483648;  mov.u32 %r2, 1;\n"
    "  setp.gt.u32 %p1, %r1, %r2;\n"
    "  @%p1 bra $T;\n  mov.f32 %f1, 0f00000000;  bra $S;\n"
    "$T:\n  mov.f32 %f1, 0f3F800000;\n"
    "$S:\n  st.global.f32 [%rd2], %f1;\n  ret;\n}\n";

/* mulu(out): out[0](u64) = mul.wide.u32(0x80000000, 2) = 0x1_0000_0000 (zero-extended). The signed
   wide form would sign-extend to 0xFFFFFFFF_00000000. Mirrors ptx.rs. */
static const char* MULU_PTX =
    ".visible .entry mulu(.param .u64 p) {\n"
    "  .reg .b32 %r<2>; .reg .b64 %rd<4>;\n"
    "  ld.param.u64 %rd1, [p];  cvta.to.global.u64 %rd2, %rd1;\n"
    "  mov.u32 %r1, 2147483648;\n"
    "  mul.wide.u32 %rd3, %r1, 2;\n"
    "  st.global.u64 [%rd2], %rd3;\n  ret;\n}\n";

/* driver-API function pointers */
static CUresult (*Init)(unsigned);
static CUresult (*DrvVer)(int*);
static CUresult (*DevCount)(int*);
static CUresult (*DevGet)(CUdevice*, int);
static CUresult (*DevName)(char*, int, CUdevice);
static CUresult (*DevMem)(size_t*, CUdevice);
static CUresult (*DevAttr)(int*, CUdevice_attribute, CUdevice);
static CUresult (*CtxCreate)(CUcontext*, unsigned, CUdevice);
static CUresult (*CtxDestroy)(CUcontext);
static CUresult (*MemAlloc)(CUdeviceptr*, size_t);
static CUresult (*MemFree)(CUdeviceptr);
static CUresult (*MemAllocManaged)(CUdeviceptr*, size_t, unsigned);
static CUresult (*MemGetInfo)(size_t*, size_t*);
static CUresult (*MemGetAddressRange)(CUdeviceptr*, size_t*, CUdeviceptr);
static CUresult (*H2D)(CUdeviceptr, const void*, size_t);
static CUresult (*D2H)(void*, CUdeviceptr, size_t);
static CUresult (*D2D)(CUdeviceptr, CUdeviceptr, size_t);
static CUresult (*H2DAsync)(CUdeviceptr, const void*, size_t, CUstream);
static CUresult (*MemsetD32)(CUdeviceptr, unsigned, size_t);
static CUresult (*ModLoad)(CUmodule*, const void*);
static CUresult (*ModFunc)(CUfunction*, CUmodule, const char*);
static CUresult (*ModUnload)(CUmodule);
static CUresult (*Launch)(CUfunction, unsigned, unsigned, unsigned, unsigned, unsigned, unsigned,
                          unsigned, CUstream, void**, void**);
static CUresult (*StreamCreate)(CUstream*, unsigned);
static CUresult (*StreamSync)(CUstream);
static CUresult (*StreamDestroy)(CUstream);
static CUresult (*EventCreate)(CUevent*, unsigned);
static CUresult (*EventRecord)(CUevent, CUstream);
static CUresult (*EventSync)(CUevent);
static CUresult (*EventElapsed)(float*, CUevent, CUevent);
static CUresult (*ErrStr)(CUresult, const char**);
static CUresult (*ErrName)(CUresult, const char**);
static CUresult (*PtrGetAttr)(void*, CUpointer_attribute, CUdeviceptr);
static CUresult (*Occupancy)(int*, CUfunction, int, size_t);
static CUresult (*GetProc)(const char*, void**, int, cuuint64_t);

int main(int argc, char** argv) {
    const char* so = (argc > 1) ? argv[1] : "./libcuda.so.1";
    void* h = dlopen(so, RTLD_NOW | RTLD_LOCAL);
    if (!h) { fprintf(stderr, "dlopen(%s) failed: %s\n", so, dlerror()); return 2; }

    LOAD(Init, "cuInit");                    LOAD(DrvVer, "cuDriverGetVersion");
    LOAD(DevCount, "cuDeviceGetCount");      LOAD(DevGet, "cuDeviceGet");
    LOAD(DevName, "cuDeviceGetName");        LOAD(DevMem, "cuDeviceTotalMem_v2");
    LOAD(DevAttr, "cuDeviceGetAttribute");   LOAD(CtxCreate, "cuCtxCreate_v2");
    LOAD(CtxDestroy, "cuCtxDestroy_v2");     LOAD(MemAlloc, "cuMemAlloc_v2");
    LOAD(MemFree, "cuMemFree_v2");           LOAD(MemAllocManaged, "cuMemAllocManaged");
    LOAD(MemGetInfo, "cuMemGetInfo_v2");     LOAD(MemGetAddressRange, "cuMemGetAddressRange_v2");
    LOAD(H2D, "cuMemcpyHtoD_v2");            LOAD(D2H, "cuMemcpyDtoH_v2");
    LOAD(D2D, "cuMemcpyDtoD_v2");            LOAD(H2DAsync, "cuMemcpyHtoDAsync_v2");
    LOAD(MemsetD32, "cuMemsetD32_v2");       LOAD(ModLoad, "cuModuleLoadData");
    LOAD(ModFunc, "cuModuleGetFunction");    LOAD(ModUnload, "cuModuleUnload");
    LOAD(Launch, "cuLaunchKernel");          LOAD(StreamCreate, "cuStreamCreate");
    LOAD(StreamSync, "cuStreamSynchronize"); LOAD(StreamDestroy, "cuStreamDestroy_v2");
    LOAD(EventCreate, "cuEventCreate");      LOAD(EventRecord, "cuEventRecord");
    LOAD(EventSync, "cuEventSynchronize");   LOAD(EventElapsed, "cuEventElapsedTime");
    LOAD(ErrStr, "cuGetErrorString");        LOAD(ErrName, "cuGetErrorName");
    LOAD(PtrGetAttr, "cuPointerGetAttribute");
    LOAD(Occupancy, "cuOccupancyMaxActiveBlocksPerMultiprocessor");
    LOAD(GetProc, "cuGetProcAddress");
    if (failures) return 3;

    /* ---- init / device query ---- */
    CHECK(Init(0) == CUDA_SUCCESS, "cuInit");
    int ver = 0; CHECK(DrvVer(&ver) == CUDA_SUCCESS && ver > 0, "cuDriverGetVersion");
    int count = 0; CHECK(DevCount(&count) == CUDA_SUCCESS && count == 1, "cuDeviceGetCount -> 1");
    CUdevice dev = -1; CHECK(DevGet(&dev, 0) == CUDA_SUCCESS && dev == 0, "cuDeviceGet(0)");
    char name[128] = {0}; CHECK(DevName(name, sizeof(name), dev) == CUDA_SUCCESS && name[0], "cuDeviceGetName");
    fprintf(stderr, "     device = %s\n", name);
    size_t total = 0; CHECK(DevMem(&total, dev) == CUDA_SUCCESS && total > 0, "cuDeviceTotalMem_v2");
    int warp = 0; CHECK(DevAttr(&warp, CU_DEVICE_ATTRIBUTE_WARP_SIZE, dev) == CUDA_SUCCESS && warp == 32, "warp size = 32");
    int mpc = 0; CHECK(DevAttr(&mpc, CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT, dev) == CUDA_SUCCESS && mpc > 0, "SM count > 0");

    CUcontext ctx = NULL; CHECK(CtxCreate(&ctx, 0, dev) == CUDA_SUCCESS && ctx, "cuCtxCreate_v2");

    /* ---- TIER 1: vecadd (reference) ---- */
    const int N = 2000; size_t nb = (size_t)N * sizeof(float);
    float *ha = malloc(nb), *hb = malloc(nb), *hc = malloc(nb);
    for (int i = 0; i < N; i++) { ha[i] = (float)i; hb[i] = (float)(N - i) * 0.25f; hc[i] = -1.0f; }
    CUdeviceptr da = 0, db = 0, dc = 0;
    CHECK(MemAlloc(&da, nb) == CUDA_SUCCESS && da, "cuMemAlloc_v2 a");
    CHECK(MemAlloc(&db, nb) == CUDA_SUCCESS && db, "cuMemAlloc_v2 b");
    CHECK(MemAlloc(&dc, nb) == CUDA_SUCCESS && dc, "cuMemAlloc_v2 c");
    CHECK(H2D(da, ha, nb) == CUDA_SUCCESS, "cuMemcpyHtoD_v2 a");
    CHECK(H2D(db, hb, nb) == CUDA_SUCCESS, "cuMemcpyHtoD_v2 b");
    CUmodule mod = NULL; CHECK(ModLoad(&mod, VECADD_PTX) == CUDA_SUCCESS && mod, "cuModuleLoadData(vecadd)");
    CUfunction fn = NULL; CHECK(ModFunc(&fn, mod, "vecadd") == CUDA_SUCCESS && fn, "cuModuleGetFunction(vecadd)");
    CUfunction missing = NULL; CHECK(ModFunc(&missing, mod, "nope") == CUDA_ERROR_NOT_FOUND, "missing function -> NOT_FOUND");
    int n = N; void* params[4] = { &da, &db, &dc, &n };
    unsigned block = 256, grid = (unsigned)((N + block - 1) / block);
    CUresult lr = Launch(fn, grid, 1, 1, block, 1, 1, 0, NULL, params, NULL);
    const char* es = ""; ErrStr(lr, &es);
    CHECK(lr == CUDA_SUCCESS, "cuLaunchKernel(vecadd)");
    if (lr != CUDA_SUCCESS) fprintf(stderr, "     launch error: %s\n", es);
    CHECK(D2H(hc, dc, nb) == CUDA_SUCCESS, "cuMemcpyDtoH_v2 c");
    int bad = 0;
    for (int i = 0; i < N; i++) if (hc[i] != ha[i] + hb[i]) { if (bad < 3) fprintf(stderr, "     mismatch c[%d]=%f expected %f\n", i, hc[i], ha[i] + hb[i]); bad++; }
    CHECK(bad == 0, "vecadd correct for all N elements");

    /* ---- TIER 1: device-to-device copy + cuMemsetD32 ---- */
    CUdeviceptr dd2 = 0; CHECK(MemAlloc(&dd2, nb) == CUDA_SUCCESS, "cuMemAlloc_v2 d2d dst");
    CHECK(D2D(dd2, dc, nb) == CUDA_SUCCESS, "cuMemcpyDtoD_v2");
    float* hchk = malloc(nb); CHECK(D2H(hchk, dd2, nb) == CUDA_SUCCESS, "D2H after D2D");
    int d2d_ok = 1; for (int i = 0; i < N; i++) if (hchk[i] != hc[i]) d2d_ok = 0;
    CHECK(d2d_ok, "device-to-device copy is exact");
    union { float f; unsigned u; } pat; pat.f = 3.5f;
    CHECK(MemsetD32(dd2, pat.u, N) == CUDA_SUCCESS, "cuMemsetD32_v2");
    CHECK(D2H(hchk, dd2, nb) == CUDA_SUCCESS, "D2H after memset");
    int set_ok = 1; for (int i = 0; i < N; i++) if (hchk[i] != 3.5f) set_ok = 0;
    CHECK(set_ok, "cuMemsetD32 filled every element with 3.5");

    /* ---- TIER 1: async H2D on a stream + event elapsed time ---- */
    CUstream s = NULL; CHECK(StreamCreate(&s, 0) == CUDA_SUCCESS && s, "cuStreamCreate");
    CUevent e0 = NULL, e1 = NULL;
    CHECK(EventCreate(&e0, 0) == CUDA_SUCCESS, "cuEventCreate start");
    CHECK(EventCreate(&e1, 0) == CUDA_SUCCESS, "cuEventCreate end");
    CHECK(EventRecord(e0, s) == CUDA_SUCCESS, "cuEventRecord start");
    CHECK(H2DAsync(da, ha, nb, s) == CUDA_SUCCESS, "cuMemcpyHtoDAsync_v2");
    CHECK(Launch(fn, grid, 1, 1, block, 1, 1, 0, s, params, NULL) == CUDA_SUCCESS, "cuLaunchKernel on stream");
    CHECK(EventRecord(e1, s) == CUDA_SUCCESS, "cuEventRecord end");
    CHECK(StreamSync(s) == CUDA_SUCCESS, "cuStreamSynchronize");
    float ms = -1.0f; CHECK(EventElapsed(&ms, e0, e1) == CUDA_SUCCESS && ms >= 0.0f, "cuEventElapsedTime >= 0");
    fprintf(stderr, "     elapsed = %.4f ms\n", ms);

    /* ---- TIER 1: SAXPY (ld.param.f32 + fma) ---- */
    CUmodule modS = NULL; CHECK(ModLoad(&modS, SAXPY_PTX) == CUDA_SUCCESS, "cuModuleLoadData(saxpy)");
    CUfunction fnS = NULL; CHECK(ModFunc(&fnS, modS, "saxpy") == CUDA_SUCCESS, "cuModuleGetFunction(saxpy)");
    float aval = 2.5f;
    float *hx = malloc(nb), *hy = malloc(nb), *hy0 = malloc(nb);
    for (int i = 0; i < N; i++) { hx[i] = (float)(i % 17); hy[i] = (float)(i % 5); hy0[i] = hy[i]; }
    CUdeviceptr dx = 0, dy = 0;
    CHECK(MemAlloc(&dx, nb) == CUDA_SUCCESS && MemAlloc(&dy, nb) == CUDA_SUCCESS, "cuMemAlloc saxpy x/y");
    CHECK(H2D(dx, hx, nb) == CUDA_SUCCESS && H2D(dy, hy, nb) == CUDA_SUCCESS, "H2D saxpy x/y");
    void* sp[4] = { &dx, &dy, &aval, &n };
    CHECK(Launch(fnS, grid, 1, 1, block, 1, 1, 0, NULL, sp, NULL) == CUDA_SUCCESS, "cuLaunchKernel(saxpy)");
    CHECK(D2H(hy, dy, nb) == CUDA_SUCCESS, "D2H saxpy y");
    int sax_ok = 1; for (int i = 0; i < N; i++) { float want = aval * hx[i] + hy0[i]; if (fabsf(hy[i] - want) > 1e-3f) { if (sax_ok) fprintf(stderr, "     saxpy mismatch y[%d]=%f want %f\n", i, hy[i], want); sax_ok = 0; } }
    CHECK(sax_ok, "saxpy correct: y = a*x + y");

    /* ---- TIER 1: pairwise reduction step (offset global loads) ---- */
    CUmodule modR = NULL; CHECK(ModLoad(&modR, REDUCE_PTX) == CUDA_SUCCESS, "cuModuleLoadData(reduce)");
    CUfunction fnR = NULL; CHECK(ModFunc(&fnR, modR, "reduce_pairs") == CUDA_SUCCESS, "cuModuleGetFunction(reduce)");
    const int M = N / 2; size_t mb = (size_t)M * sizeof(float);
    CUdeviceptr dout = 0; CHECK(MemAlloc(&dout, mb) == CUDA_SUCCESS, "cuMemAlloc reduce out");
    CHECK(H2D(da, ha, nb) == CUDA_SUCCESS, "reload a for reduce");
    int m = M; void* rp[3] = { &da, &dout, &m };
    unsigned rgrid = (unsigned)((M + block - 1) / block);
    CHECK(Launch(fnR, rgrid, 1, 1, block, 1, 1, 0, NULL, rp, NULL) == CUDA_SUCCESS, "cuLaunchKernel(reduce)");
    float* hout = malloc(mb); CHECK(D2H(hout, dout, mb) == CUDA_SUCCESS, "D2H reduce out");
    int red_ok = 1; for (int i = 0; i < M; i++) { float want = ha[2*i] + ha[2*i+1]; if (hout[i] != want) { if (red_ok) fprintf(stderr, "     reduce mismatch out[%d]=%f want %f\n", i, hout[i], want); red_ok = 0; } }
    CHECK(red_ok, "pairwise reduction correct: out[i]=in[2i]+in[2i+1]");

    /* ---- interpreter numeric semantics (mirror ptx.rs regression tests) ---- */
    {
        /* fma.rn.f32 must be FUSED (single rounding). Unfused would give exactly 0.0. */
        CUmodule mF = NULL; CHECK(ModLoad(&mF, FMA_PTX) == CUDA_SUCCESS, "cuModuleLoadData(fmac)");
        CUfunction fF = NULL; CHECK(ModFunc(&fF, mF, "fmac") == CUDA_SUCCESS, "cuModuleGetFunction(fmac)");
        CUdeviceptr dF = 0; CHECK(MemAlloc(&dF, sizeof(float)) == CUDA_SUCCESS, "cuMemAlloc fma out");
        void* pF[1] = { &dF };
        CHECK(Launch(fF, 1, 1, 1, 1, 1, 1, 0, NULL, pF, NULL) == CUDA_SUCCESS, "cuLaunchKernel(fmac)");
        float rf = 123.0f; CHECK(D2H(&rf, dF, sizeof(float)) == CUDA_SUCCESS, "D2H fma out");
        float want_fma = fmaf(0.1f, 10.0f, -1.0f);
        CHECK(memcmp(&rf, &want_fma, 4) == 0, "fma.rn.f32 is fused (bit-exact vs fmaf)");
        CHECK(rf != 0.0f, "fma.rn.f32 not collapsed to 0.0 by an unfused mul-then-add");
        fprintf(stderr, "     fma result = %.9g (fused), unfused would be 0\n", rf);
        MemFree(dF); ModUnload(mF);

        /* setp.gt.u32 compares UNSIGNED. */
        CUmodule mU = NULL; CHECK(ModLoad(&mU, SETPU_PTX) == CUDA_SUCCESS, "cuModuleLoadData(setpu)");
        CUfunction fU = NULL; CHECK(ModFunc(&fU, mU, "setpu") == CUDA_SUCCESS, "cuModuleGetFunction(setpu)");
        CUdeviceptr dU = 0; CHECK(MemAlloc(&dU, sizeof(float)) == CUDA_SUCCESS, "cuMemAlloc setpu out");
        void* pU[1] = { &dU };
        CHECK(Launch(fU, 1, 1, 1, 1, 1, 1, 0, NULL, pU, NULL) == CUDA_SUCCESS, "cuLaunchKernel(setpu)");
        float ru = -1.0f; CHECK(D2H(&ru, dU, sizeof(float)) == CUDA_SUCCESS, "D2H setpu out");
        CHECK(ru == 1.0f, "setp.gt.u32 0x80000000 > 1 is true (unsigned)");
        MemFree(dU); ModUnload(mU);

        /* mul.wide.u32 zero-extends. */
        CUmodule mM = NULL; CHECK(ModLoad(&mM, MULU_PTX) == CUDA_SUCCESS, "cuModuleLoadData(mulu)");
        CUfunction fM = NULL; CHECK(ModFunc(&fM, mM, "mulu") == CUDA_SUCCESS, "cuModuleGetFunction(mulu)");
        CUdeviceptr dM = 0; CHECK(MemAlloc(&dM, sizeof(unsigned long long)) == CUDA_SUCCESS, "cuMemAlloc mulu out");
        void* pM[1] = { &dM };
        CHECK(Launch(fM, 1, 1, 1, 1, 1, 1, 0, NULL, pM, NULL) == CUDA_SUCCESS, "cuLaunchKernel(mulu)");
        unsigned long long rm = 0; CHECK(D2H(&rm, dM, sizeof(rm)) == CUDA_SUCCESS, "D2H mulu out");
        CHECK(rm == 0x100000000ULL, "mul.wide.u32 0x80000000*2 zero-extends to 0x1_0000_0000");
        MemFree(dM); ModUnload(mM);
    }

    /* ---- TIER 1: cuMemAllocManaged round-trip (unified: host writes read back after kernel) ---- */
    CUdeviceptr mgd = 0, mgy = 0;
    CHECK(MemAllocManaged(&mgd, nb, CU_MEM_ATTACH_GLOBAL) == CUDA_SUCCESS && mgd, "cuMemAllocManaged x");
    CHECK(MemAllocManaged(&mgy, nb, CU_MEM_ATTACH_GLOBAL) == CUDA_SUCCESS && mgy, "cuMemAllocManaged y");
    float* mx = (float*)(size_t)mgd; float* my = (float*)(size_t)mgy;   /* unified: use directly on host */
    for (int i = 0; i < N; i++) { mx[i] = (float)i; my[i] = 1.0f; }
    void* mp[4] = { &mgd, &mgy, &aval, &n };
    CHECK(Launch(fnS, grid, 1, 1, block, 1, 1, 0, NULL, mp, NULL) == CUDA_SUCCESS, "saxpy on managed memory");
    int mg_ok = 1; for (int i = 0; i < N; i++) { float want = aval * (float)i + 1.0f; if (fabsf(my[i] - want) > 1e-3f) mg_ok = 0; }
    CHECK(mg_ok, "managed-memory round-trip (host<->kernel, no explicit copy)");

    /* ---- TIER 2: memGetInfo, pointer attributes, address range, error name, occupancy ---- */
    size_t freeB = 0, totB = 0; CHECK(MemGetInfo(&freeB, &totB) == CUDA_SUCCESS && totB > 0 && freeB <= totB, "cuMemGetInfo (free<=total)");
    unsigned int mtype = 0; CHECK(PtrGetAttr(&mtype, CU_POINTER_ATTRIBUTE_MEMORY_TYPE, da) == CUDA_SUCCESS && mtype == CU_MEMORYTYPE_DEVICE, "cuPointerGetAttribute MEMORY_TYPE=DEVICE");
    unsigned int managed = 0; CHECK(PtrGetAttr(&managed, CU_POINTER_ATTRIBUTE_IS_MANAGED, mgd) == CUDA_SUCCESS && managed == 1, "cuPointerGetAttribute IS_MANAGED on managed ptr");
    CUdeviceptr base = 0; size_t rsz = 0; CHECK(MemGetAddressRange(&base, &rsz, da + 64) == CUDA_SUCCESS && base == da && rsz == nb, "cuMemGetAddressRange resolves offset ptr");
    const char* enm = NULL; CHECK(ErrName(CUDA_ERROR_OUT_OF_MEMORY, &enm) == CUDA_SUCCESS && strcmp(enm, "CUDA_ERROR_OUT_OF_MEMORY") == 0, "cuGetErrorName(OUT_OF_MEMORY)");
    int blocks = 0; CHECK(Occupancy(&blocks, fn, 256, 0) == CUDA_SUCCESS && blocks >= 1, "cuOccupancyMaxActiveBlocksPerMultiprocessor >= 1");

    /* ---- cuGetProcAddress: version-suffixed AND base-name resolution, and the pointer runs ---- */
    void* pfn = NULL;
    CHECK(GetProc("cuMemcpyHtoD_v2", &pfn, ver, 0) == CUDA_SUCCESS && pfn != NULL, "cuGetProcAddress(cuMemcpyHtoD_v2)");
    CHECK(GetProc("cuLaunchKernel", &pfn, ver, 0) == CUDA_SUCCESS && pfn == (void*)Launch, "cuGetProcAddress(cuLaunchKernel) matches dlsym");
    void* pAlloc = NULL;
    CHECK(GetProc("cuMemAlloc", &pAlloc, ver, 0) == CUDA_SUCCESS && pAlloc != NULL, "cuGetProcAddress(cuMemAlloc) base->_v2");
    CUresult (*allocViaProc)(CUdeviceptr*, size_t) = (CUresult(*)(CUdeviceptr*, size_t))pAlloc;
    CUdeviceptr viaproc = 0; CHECK(allocViaProc(&viaproc, 128) == CUDA_SUCCESS && viaproc, "resolved cuMemAlloc pointer actually allocates");
    MemFree(viaproc);
    void* pNope = NULL; CHECK(GetProc("cuTotallyNotAThing", &pNope, ver, 0) == CUDA_ERROR_NOT_FOUND, "cuGetProcAddress(bogus) -> NOT_FOUND");

    /* ---- TIER 3: honest stubs — present, spec-correct error, no crash ---- */
    CUresult (*GraphCreate)(void*, unsigned) = NULL;
    *(void**)(&GraphCreate) = dlsym(h, "cuGraphCreate");
    CHECK(GraphCreate != NULL, "cuGraphCreate is EXPORTED (dlsym resolves)");
    if (GraphCreate) { void* g = NULL; CHECK(GraphCreate(&g, 0) == CUDA_ERROR_NOT_SUPPORTED, "cuGraphCreate -> NOT_SUPPORTED (honest stub)"); }
    /* a cubin/ELF image must be rejected: dd executes PTX only */
    static const unsigned char FAKE_CUBIN[16] = { 0x7f, 'E', 'L', 'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
    CUmodule bogus = NULL; CHECK(ModLoad(&bogus, FAKE_CUBIN) == CUDA_ERROR_INVALID_IMAGE, "cuModuleLoadData(cubin/ELF) -> INVALID_IMAGE");

    /* ---- cleanup ---- */
    ModUnload(mod); ModUnload(modS); ModUnload(modR);
    MemFree(da); MemFree(db); MemFree(dc); MemFree(dd2); MemFree(dx); MemFree(dy); MemFree(dout); MemFree(mgd); MemFree(mgy);
    StreamDestroy(s);
    CHECK(CtxDestroy(ctx) == CUDA_SUCCESS, "cuCtxDestroy_v2");
    const char* ok = NULL; CHECK(ErrStr(CUDA_SUCCESS, &ok) == CUDA_SUCCESS && strcmp(ok, "no error") == 0, "cuGetErrorString(SUCCESS)");

    free(ha); free(hb); free(hc); free(hchk); free(hx); free(hy); free(hy0); free(hout);
    dlclose(h);
    if (failures) { fprintf(stderr, "\n%d FAILURE(S)\n", failures); return 1; }
    fprintf(stderr, "\nALL PASS\n");
    return 0;
}
