/* dd-shim-cuda dlopen smoke test — the CUDA analogue of dd-shim-gl's dlopen check.
 *
 * A plain C program (NOT linked against the shim) dlopen()s the built libcuda.so.1, resolves the
 * bring-up CUDA Driver API entry points by name, drives them, and asserts sane values. This proves the
 * cdylib is a valid drop-in `libcuda`: the symbols are exported with the right C ABI and return usable
 * results — a real CUDA app / libcudart resolving `cu*` the same way would load and initialize.
 *
 *   build+run:  cc tests/smoke.c -ldl -o /tmp/cuda_smoke && /tmp/cuda_smoke <path-to-libdd_shim_cuda.so>
 */
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

typedef int CUresult; /* CUDA_SUCCESS == 0 */

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
    CUresult (*cuDriverGetVersion)(int *) = dlsym(h, "cuDriverGetVersion");
    CUresult (*cuDeviceGetCount)(int *) = dlsym(h, "cuDeviceGetCount");
    CUresult (*cuDeviceGet)(int *, int) = dlsym(h, "cuDeviceGet");
    CUresult (*cuDeviceGetName)(char *, int, int) = dlsym(h, "cuDeviceGetName");
    CUresult (*cuCtxCreate_v2)(void **, unsigned, int) = dlsym(h, "cuCtxCreate_v2");

    if (!cuInit || !cuDriverGetVersion || !cuDeviceGetCount || !cuDeviceGet ||
        !cuDeviceGetName || !cuCtxCreate_v2) {
        fprintf(stderr, "dlsym: a bring-up entry point is missing\n");
        return 1;
    }

    CUresult r = cuInit(0);
    if (r != 0) { fprintf(stderr, "cuInit -> %d\n", r); return 1; }

    int ver = 0;
    r = cuDriverGetVersion(&ver);
    if (r != 0 || ver <= 0) { fprintf(stderr, "cuDriverGetVersion -> r=%d ver=%d\n", r, ver); return 1; }

    int count = -1;
    r = cuDeviceGetCount(&count);
    if (r != 0 || count < 1) { fprintf(stderr, "cuDeviceGetCount -> r=%d count=%d\n", r, count); return 1; }

    int dev = -1;
    r = cuDeviceGet(&dev, 0);
    if (r != 0 || dev != 0) { fprintf(stderr, "cuDeviceGet -> r=%d dev=%d\n", r, dev); return 1; }

    char name[256] = {0};
    r = cuDeviceGetName(name, sizeof name, dev);
    if (r != 0 || name[0] == 0) { fprintf(stderr, "cuDeviceGetName -> r=%d\n", r); return 1; }

    void *ctx = NULL;
    r = cuCtxCreate_v2(&ctx, 0, dev);
    if (r != 0 || ctx == NULL) { fprintf(stderr, "cuCtxCreate_v2 -> r=%d ctx=%p\n", r, ctx); return 1; }

    printf("dlopen smoke OK: driver=%d.%d devices=%d dev0=\"%s\" ctx=%p\n",
           ver / 1000, (ver % 1000) / 10, count, name, ctx);
    dlclose(h);
    return 0;
}
