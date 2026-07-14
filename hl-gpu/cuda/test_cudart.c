/* Conformance harness for dd's libcudart.so — the CUDA Runtime API shim over the libcuda driver shim.
 *
 * Drives the full runtime path an unmodified CUDA app exercises — with NO GPU:
 *   TIER 0 (stateful runtime API): cudaGetDeviceCount / cudaGetDeviceProperties / cudaSetDevice /
 *     cudaGetDevice / cudaDeviceSynchronize / cudaMalloc / cudaMemcpy(kind) / cudaMemset / cudaFree /
 *     streams / events + the thread-local last-error cell + cudaGetErrorString/Name + versions.
 *   TIER 1 (kernel launch via the compiler registration glue + fatbin->PTX): build a SYNTHETIC
 *     UNCOMPRESSED fatbin wrapping the vecadd PTX, register it through __cudaRegisterFatBinary /
 *     __cudaRegisterFunction, and drive c[i]=a[i]+b[i] end-to-end through cudart:
 *       __cudaPushCallConfiguration -> host stub -> __cudaPopCallConfiguration -> cudaLaunchKernel.
 *   Also: a SASS-only synthetic fatbin (kind=2 only) must surface cudaErrorInvalidKernelImage.
 * Exit 0 = all assertions pass.
 */
#define _GNU_SOURCE
#include "cudart_min.h"
#include "fatbin.h"        /* the fatbin container layout — we OWN the synthetic blob format */
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;
#define CHECK(cond, msg) do { if (!(cond)) { fprintf(stderr, "FAIL: %s\n", msg); failures++; } \
                              else { fprintf(stderr, "ok  : %s\n", msg); } } while (0)

/* c[i] = a[i] + b[i] — the reference PTX the driver already runs. */
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

/* Build a SYNTHETIC uncompressed fatbin: [container header][entry header][payload].
 * Returns a malloc'd buffer; *out points at the container start. `kind` = DD_FATBIN_KIND_PTX/ELF. */
static unsigned char* build_fatbin(const void* payload, size_t payload_len, unsigned short kind,
                                   unsigned char** out_container) {
    size_t total = sizeof(DdFatBinHeader) + sizeof(DdFatBinEntry) + payload_len;
    unsigned char* buf = (unsigned char*)calloc(1, total);
    DdFatBinHeader h; memset(&h, 0, sizeof(h));
    h.magic = DD_FATBIN_MAGIC; h.version = 1; h.header_size = (unsigned short)sizeof(DdFatBinHeader);
    h.fat_size = sizeof(DdFatBinEntry) + payload_len;
    memcpy(buf, &h, sizeof(h));
    DdFatBinEntry e; memset(&e, 0, sizeof(e));
    e.kind = kind; e.version = 1; e.header_size = (unsigned int)sizeof(DdFatBinEntry);
    e.payload_size = payload_len; e.arch = 86; e.major = 7; e.minor = 5; e.flags = 0; /* uncompressed */
    memcpy(buf + sizeof(h), &e, sizeof(e));
    memcpy(buf + sizeof(h) + sizeof(e), payload, payload_len);
    *out_container = buf;
    return buf;
}

/* fatbin.h robustness: malformed / truncated / hostile containers must return NULL cleanly — never a
 * crash, an OOB read, or an infinite loop. Every case here reads ONLY within its own allocation (the
 * walk is bounded by the container's declared fat_size), so a clean run under ASan proves no OOB. */
static void test_fatbin_malformed(void) {
    unsigned long long n = 12345; /* poisoned; a NULL return must not write it */
    /* 1. NULL image */
    CHECK(hl_fatbin_extract_ptx(NULL, &n) == NULL, "fatbin: NULL image -> NULL");
    /* 2. garbage (bad magic) */
    unsigned char junk[16] = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };
    CHECK(hl_fatbin_extract_ptx(junk, &n) == NULL, "fatbin: bad magic -> NULL");
    /* 3. wrapper with a NULL container pointer */
    DdFatBinWrapper wnull = { (int)DD_FATBIN_WRAPPER_MAGIC, 1, NULL, NULL };
    CHECK(hl_fatbin_extract_ptx(&wnull, &n) == NULL, "fatbin: wrapper NULL data -> NULL");
    /* 4. container magic but header_size < sizeof(header) */
    {
        DdFatBinHeader h; memset(&h, 0, sizeof h);
        h.magic = DD_FATBIN_MAGIC; h.header_size = 4; h.fat_size = 0;
        unsigned char b[sizeof(DdFatBinHeader)]; memcpy(b, &h, sizeof h);
        CHECK(hl_fatbin_extract_ptx(b, &n) == NULL, "fatbin: header_size<16 -> NULL");
    }
    /* 5. valid header, zero fat_size (no entries) */
    {
        DdFatBinHeader h; memset(&h, 0, sizeof h);
        h.magic = DD_FATBIN_MAGIC; h.header_size = (unsigned short)sizeof h; h.fat_size = 0;
        unsigned char b[sizeof(DdFatBinHeader)]; memcpy(b, &h, sizeof h);
        CHECK(hl_fatbin_extract_ptx(b, &n) == NULL, "fatbin: zero fat_size -> NULL");
    }
    /* 6. entry header_size = 0 (< sizeof entry) -> loop breaks (guarantees forward progress), NULL */
    {
        unsigned char *c = NULL; unsigned char *buf = build_fatbin("x", 2, DD_FATBIN_KIND_PTX, &c);
        DdFatBinEntry e; memcpy(&e, c + sizeof(DdFatBinHeader), sizeof e);
        e.header_size = 0; memcpy(c + sizeof(DdFatBinHeader), &e, sizeof e);
        CHECK(hl_fatbin_extract_ptx(c, &n) == NULL, "fatbin: entry header_size=0 -> NULL (no infinite loop)");
        free(buf);
    }
    /* 7. PTX entry whose payload_size runs far past fat_size -> bounded by end, NULL (no OOB read) */
    {
        unsigned char *c = NULL; unsigned char *buf = build_fatbin("hello", 6, DD_FATBIN_KIND_PTX, &c);
        DdFatBinEntry e; memcpy(&e, c + sizeof(DdFatBinHeader), sizeof e);
        e.payload_size = 0xffffffffULL; memcpy(c + sizeof(DdFatBinHeader), &e, sizeof e);
        CHECK(hl_fatbin_extract_ptx(c, &n) == NULL, "fatbin: payload beyond end -> NULL (no OOB)");
        free(buf);
    }
    /* 8. compressed PTX entry -> NULL (tier-1 decodes uncompressed only) */
    {
        unsigned char *c = NULL; unsigned char *buf = build_fatbin("hello", 6, DD_FATBIN_KIND_PTX, &c);
        DdFatBinEntry e; memcpy(&e, c + sizeof(DdFatBinHeader), sizeof e);
        e.flags = DD_FATBIN_FLAG_COMPRESS; memcpy(c + sizeof(DdFatBinHeader), &e, sizeof e);
        CHECK(hl_fatbin_extract_ptx(c, &n) == NULL, "fatbin: compressed PTX -> NULL");
        free(buf);
    }
    /* 9. truncated container: fat_size honest, but the entry's payload_size runs 4 bytes past the buffer */
    {
        size_t total = sizeof(DdFatBinHeader) + sizeof(DdFatBinEntry) + 4;
        unsigned char *b = (unsigned char *)calloc(1, total);
        DdFatBinHeader h; memset(&h, 0, sizeof h);
        h.magic = DD_FATBIN_MAGIC; h.header_size = (unsigned short)sizeof h;
        h.fat_size = sizeof(DdFatBinEntry) + 4; memcpy(b, &h, sizeof h);
        DdFatBinEntry e; memset(&e, 0, sizeof e);
        e.kind = DD_FATBIN_KIND_PTX; e.header_size = (unsigned int)sizeof e; e.payload_size = 8; /* > 4 left */
        memcpy(b + sizeof h, &e, sizeof e);
        CHECK(hl_fatbin_extract_ptx(b, &n) == NULL, "fatbin: truncated payload -> NULL (no OOB)");
        free(b);
    }
    /* 10. sanity: a well-formed PTX entry still extracts exactly (the trim + copy path stays correct) */
    {
        const char *ptx = ".version 7.5\n";
        unsigned char *c = NULL; unsigned char *buf = build_fatbin(ptx, strlen(ptx) + 1, DD_FATBIN_KIND_PTX, &c);
        unsigned long long got_len = 0;
        char *got = hl_fatbin_extract_ptx(c, &got_len);
        CHECK(got && strcmp(got, ptx) == 0 && got_len == strlen(ptx), "fatbin: well-formed PTX round-trips");
        free(got); free(buf);
    }
}

/* --- the host-stub side of a kernel<<<>>> launch, as nvcc would lower it --- */
static void* g_vecadd_handle = NULL; /* the host-stub pointer registered for vecadd */

/* the host stub nvcc emits for `vecadd`: recover the pushed config and forward to cudaLaunchKernel. */
static void vecadd_stub(void* a, void* b, void* c, int n) {
    void* args[4] = { &a, &b, &c, &n };
    dim3 g, blk; size_t sh; cudaStream_t st;
    __cudaPopCallConfiguration(&g, &blk, &sh, &st);
    cudaLaunchKernel(g_vecadd_handle, g, blk, args, sh, st);
}

int main(void) {
    /* ---- TIER 0: device query ---- */
    int count = -1;
    CHECK(cudaGetDeviceCount(&count) == cudaSuccess && count == 1, "cudaGetDeviceCount -> 1");
    CHECK(cudaSetDevice(0) == cudaSuccess, "cudaSetDevice(0)");
    int dev = -1; CHECK(cudaGetDevice(&dev) == cudaSuccess && dev == 0, "cudaGetDevice -> 0");
    CHECK(cudaSetDevice(1) == cudaErrorInvalidDevice, "cudaSetDevice(1) -> InvalidDevice");
    (void)cudaGetLastError(); /* drain the deliberate error */

    struct cudaDeviceProp prop;
    CHECK(cudaGetDeviceProperties(&prop, 0) == cudaSuccess, "cudaGetDeviceProperties");
    fprintf(stderr, "     device = %s  cc %d.%d  SMs %d  warp %d  mem %.0f MB\n",
            prop.name, prop.major, prop.minor, prop.multiProcessorCount, prop.warpSize,
            (double)prop.totalGlobalMem / (1024.0 * 1024.0));
    CHECK(prop.name[0] != 0, "cudaDeviceProp.name populated");
    CHECK(prop.totalGlobalMem > 0, "cudaDeviceProp.totalGlobalMem > 0");
    CHECK(prop.major >= 1, "cudaDeviceProp.major >= 1");
    CHECK(prop.warpSize == 32, "cudaDeviceProp.warpSize == 32");
    CHECK(prop.multiProcessorCount > 0, "cudaDeviceProp.multiProcessorCount > 0");
    CHECK(prop.maxThreadsPerBlock == 1024, "cudaDeviceProp.maxThreadsPerBlock == 1024");

    int rtver = 0, drvver = 0;
    CHECK(cudaRuntimeGetVersion(&rtver) == cudaSuccess && rtver > 0, "cudaRuntimeGetVersion");
    CHECK(cudaDriverGetVersion(&drvver) == cudaSuccess && drvver > 0, "cudaDriverGetVersion");

    /* ---- TIER 0: error reporting + thread-local last-error ---- */
    CHECK(cudaGetLastError() == cudaSuccess, "cudaGetLastError starts clean");
    CHECK(cudaMemcpy((void*)16, (void*)16, 8, (enum cudaMemcpyKind)99) == cudaErrorInvalidMemcpyDirection,
          "bad memcpy kind -> InvalidMemcpyDirection");
    CHECK(cudaPeekAtLastError() == cudaErrorInvalidMemcpyDirection, "cudaPeekAtLastError sees sticky error");
    CHECK(cudaGetLastError() == cudaErrorInvalidMemcpyDirection, "cudaGetLastError drains it");
    CHECK(cudaGetLastError() == cudaSuccess, "last-error cleared after drain");
    CHECK(strcmp(cudaGetErrorName(cudaErrorMemoryAllocation), "cudaErrorMemoryAllocation") == 0,
          "cudaGetErrorName(MemoryAllocation)");
    CHECK(strcmp(cudaGetErrorString(cudaSuccess), "no error") == 0, "cudaGetErrorString(Success)");

    /* ---- TIER 0: memory round-trip (what .cuda()/.cpu() ride) ---- */
    const int N = 2000; size_t nb = (size_t)N * sizeof(float);
    float *ha = malloc(nb), *hb = malloc(nb), *hc = malloc(nb);
    for (int i = 0; i < N; i++) { ha[i] = (float)i; hb[i] = (float)(N - i) * 0.25f; hc[i] = -1.0f; }
    void *da = NULL, *db = NULL, *dc = NULL;
    CHECK(cudaMalloc(&da, nb) == cudaSuccess && da, "cudaMalloc a");
    CHECK(cudaMalloc(&db, nb) == cudaSuccess && db, "cudaMalloc b");
    CHECK(cudaMalloc(&dc, nb) == cudaSuccess && dc, "cudaMalloc c");
    CHECK(cudaMemcpy(da, ha, nb, cudaMemcpyHostToDevice) == cudaSuccess, "cudaMemcpy H2D a");
    CHECK(cudaMemcpy(db, hb, nb, cudaMemcpyHostToDevice) == cudaSuccess, "cudaMemcpy H2D b");
    CHECK(cudaMemset(dc, 0, nb) == cudaSuccess, "cudaMemset c");
    /* H2D then D2H round-trip through a device buffer, no kernel */
    CHECK(cudaMemcpy(hc, da, nb, cudaMemcpyDeviceToHost) == cudaSuccess, "cudaMemcpy D2H a->hc");
    int rt_ok = 1; for (int i = 0; i < N; i++) if (hc[i] != ha[i]) rt_ok = 0;
    CHECK(rt_ok, "memory round-trip H2D->D2H exact");

    /* ---- TIER 0: streams + events ---- */
    cudaStream_t s = NULL; CHECK(cudaStreamCreate(&s) == cudaSuccess && s, "cudaStreamCreate");
    cudaEvent_t e0 = NULL, e1 = NULL;
    CHECK(cudaEventCreate(&e0) == cudaSuccess, "cudaEventCreate start");
    CHECK(cudaEventCreate(&e1) == cudaSuccess, "cudaEventCreate end");
    CHECK(cudaEventRecord(e0, s) == cudaSuccess, "cudaEventRecord start");

    /* ---- TIER 1: register a SYNTHETIC uncompressed fatbin (PTX kind=1) via the nvcc glue ---- */
    unsigned char *container = NULL;
    unsigned char *fatbuf = build_fatbin(VECADD_PTX, strlen(VECADD_PTX) + 1, DD_FATBIN_KIND_PTX, &container);
    /* the __fatBinC_Wrapper_t nvcc would hand __cudaRegisterFatBinary */
    DdFatBinWrapper wrapper = { (int)DD_FATBIN_WRAPPER_MAGIC, 1, container, NULL };
    void** handle = __cudaRegisterFatBinary(&wrapper);
    CHECK(handle != NULL, "__cudaRegisterFatBinary returns a handle");
    g_vecadd_handle = (void*)vecadd_stub; /* nvcc uses the host stub's address as the key */
    __cudaRegisterFunction(handle, (const char*)vecadd_stub, "vecadd", "vecadd", -1, NULL, NULL, NULL, NULL, NULL);
    __cudaRegisterFatBinaryEnd(handle);

    /* ---- TIER 1: launch c=a+b through cudart (push-config -> host stub -> cudaLaunchKernel) ---- */
    int n = N;
    unsigned int block = 256, grid = (unsigned)((N + block - 1) / block);
    dim3 gdim = { grid, 1, 1 }, bdim = { block, 1, 1 };
    /* the <<<grid,block>>> call site nvcc lowers: push config, then call the host stub */
    CHECK(__cudaPushCallConfiguration(gdim, bdim, 0, s) == 0, "__cudaPushCallConfiguration");
    vecadd_stub(da, db, dc, n);
    CHECK(cudaGetLastError() == cudaSuccess, "cudaLaunchKernel(vecadd) via glue -> success");
    CHECK(cudaEventRecord(e1, s) == cudaSuccess, "cudaEventRecord end");
    CHECK(cudaStreamSynchronize(s) == cudaSuccess, "cudaStreamSynchronize");
    CHECK(cudaDeviceSynchronize() == cudaSuccess, "cudaDeviceSynchronize");
    float elapsed = -1.0f;
    CHECK(cudaEventElapsedTime(&elapsed, e0, e1) == cudaSuccess && elapsed >= 0.0f, "cudaEventElapsedTime >= 0");

    CHECK(cudaMemcpy(hc, dc, nb, cudaMemcpyDeviceToHost) == cudaSuccess, "cudaMemcpy D2H result");
    int bad = 0;
    for (int i = 0; i < N; i++) if (hc[i] != ha[i] + hb[i]) { if (bad < 3) fprintf(stderr, "     mismatch c[%d]=%f want %f\n", i, hc[i], ha[i] + hb[i]); bad++; }
    CHECK(bad == 0, "SYNTHETIC FATBIN vecadd correct: c[i]=a[i]+b[i] for all N via cudart");

    /* ---- TIER 1: a SASS-only fatbin (kind=2 only, no PTX) must be rejected cleanly ---- */
    static const unsigned char FAKE_ELF[32] = { 0x7f, 'E', 'L', 'F', 2, 1, 1, 0 };
    unsigned char *sass_container = NULL;
    build_fatbin(FAKE_ELF, sizeof(FAKE_ELF), DD_FATBIN_KIND_ELF, &sass_container);
    DdFatBinWrapper sass_wrapper = { (int)DD_FATBIN_WRAPPER_MAGIC, 1, sass_container, NULL };
    void** sass_handle = __cudaRegisterFatBinary(&sass_wrapper);
    static char sass_stub_key; /* a distinct host-fun key */
    __cudaRegisterFunction(sass_handle, &sass_stub_key, "ghost", "ghost", -1, NULL, NULL, NULL, NULL, NULL);
    {
        void* noargs[1] = { NULL };
        dim3 one = { 1, 1, 1 };
        cudaError_t le = cudaLaunchKernel(&sass_stub_key, one, one, noargs, 0, 0);
        CHECK(le == cudaErrorInvalidKernelImage, "SASS-only fatbin -> cudaErrorInvalidKernelImage (no crash)");
        (void)cudaGetLastError();
    }
    /* an unregistered host function -> InvalidDeviceFunction */
    {
        static char bogus_key;
        dim3 one = { 1, 1, 1 }; void* noargs[1] = { NULL };
        CHECK(cudaLaunchKernel(&bogus_key, one, one, noargs, 0, 0) == cudaErrorInvalidDeviceFunction,
              "unregistered host fn -> cudaErrorInvalidDeviceFunction");
        (void)cudaGetLastError();
    }

    /* ---- fatbin.h robustness: malformed/truncated containers reject cleanly (no crash/OOB) ---- */
    test_fatbin_malformed();

    /* ---- cleanup ---- */
    __cudaUnregisterFatBinary(handle);
    __cudaUnregisterFatBinary(sass_handle);
    cudaEventDestroy(e0); cudaEventDestroy(e1); cudaStreamDestroy(s);
    cudaFree(da); cudaFree(db); cudaFree(dc);
    CHECK(cudaFree(NULL) == cudaSuccess, "cudaFree(NULL) is a no-op success");
    free(ha); free(hb); free(hc); free(fatbuf); free(sass_container);

    if (failures) { fprintf(stderr, "\n%d FAILURE(S)\n", failures); return 1; }
    fprintf(stderr, "\nALL PASS\n");
    return 0;
}
