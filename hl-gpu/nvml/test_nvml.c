/* dlopen ABI test for dd's libnvidia-ml.so.1 shim.
 *
 * Proves the shim answers the exact NVML call sequence `nvidia-smi` uses, with the
 * seeded (env) values, WITHOUT any GPU — mirroring how the real closed nvidia-smi
 * binary links against our .so. Run under `HL_CUDA_NAME=... HL_CUDA_CC=... HL_CUDA_VRAM=...`.
 *
 * Exit 0 = all assertions pass; nonzero = a mismatch (prints which).
 */
#define _GNU_SOURCE
#include "nvml_min.h"
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

static int failures = 0;
#define CHECK(cond, msg) do { if (!(cond)) { fprintf(stderr, "FAIL: %s\n", msg); failures++; } \
                              else { fprintf(stderr, "ok  : %s\n", msg); } } while (0)

#define LOAD(var, name) do { \
    *(void**)(&var) = dlsym(h, name); \
    if (!var) { fprintf(stderr, "FAIL: missing symbol %s\n", name); failures++; } \
} while (0)

int main(int argc, char** argv) {
    const char* so = (argc > 1) ? argv[1] : "./libnvidia-ml.so.1";
    void* h = dlopen(so, RTLD_NOW | RTLD_LOCAL);
    if (!h) { fprintf(stderr, "dlopen(%s) failed: %s\n", so, dlerror()); return 2; }

    nvmlReturn_t (*Init)(void);
    nvmlReturn_t (*Count)(unsigned int*);
    nvmlReturn_t (*ByIndex)(unsigned int, nvmlDevice_t*);
    nvmlReturn_t (*GetName)(nvmlDevice_t, char*, unsigned int);
    nvmlReturn_t (*GetUUID)(nvmlDevice_t, char*, unsigned int);
    nvmlReturn_t (*GetMem)(nvmlDevice_t, nvmlMemory_t*);
    nvmlReturn_t (*GetCC)(nvmlDevice_t, int*, int*);
    nvmlReturn_t (*GetTemp)(nvmlDevice_t, nvmlTemperatureSensors_t, unsigned int*);
    nvmlReturn_t (*GetPower)(nvmlDevice_t, unsigned int*);
    nvmlReturn_t (*GetPci)(nvmlDevice_t, nvmlPciInfo_t*);
    nvmlReturn_t (*GetProcs)(nvmlDevice_t, unsigned int*, nvmlProcessInfo_t*);
    nvmlReturn_t (*DrvVer)(char*, unsigned int);
    nvmlReturn_t (*Shutdown)(void);
    const char*  (*ErrStr)(nvmlReturn_t);

    LOAD(Init,     "nvmlInit_v2");
    LOAD(Count,    "nvmlDeviceGetCount_v2");
    LOAD(ByIndex,  "nvmlDeviceGetHandleByIndex_v2");
    LOAD(GetName,  "nvmlDeviceGetName");
    LOAD(GetUUID,  "nvmlDeviceGetUUID");
    LOAD(GetMem,   "nvmlDeviceGetMemoryInfo");
    LOAD(GetCC,    "nvmlDeviceGetCudaComputeCapability");
    LOAD(GetTemp,  "nvmlDeviceGetTemperature");
    LOAD(GetPower, "nvmlDeviceGetPowerUsage");
    LOAD(GetPci,   "nvmlDeviceGetPciInfo_v3");
    LOAD(GetProcs, "nvmlDeviceGetComputeRunningProcesses_v3");
    LOAD(DrvVer,   "nvmlSystemGetDriverVersion");
    LOAD(Shutdown, "nvmlShutdown");
    LOAD(ErrStr,   "nvmlErrorString");
    if (failures) return 3;

    /* expected seeded values (env, with defaults matching the shim) */
    const char* exp_name = getenv("HL_CUDA_NAME"); if (!exp_name || !*exp_name) exp_name = "dd Metal (CUDA-sim) Device";
    const char* exp_cc   = getenv("HL_CUDA_CC");   if (!exp_cc   || !*exp_cc)   exp_cc   = "8.6";
    const char* exp_vram = getenv("HL_CUDA_VRAM"); if (!exp_vram || !*exp_vram) exp_vram = "4096";
    int exp_maj = 8, exp_min = 6; sscanf(exp_cc, "%d.%d", &exp_maj, &exp_min);
    unsigned long long exp_bytes = strtoull(exp_vram, NULL, 10) * 1024ULL * 1024ULL;

    /* the nvidia-smi call sequence */
    CHECK(Init() == NVML_SUCCESS, "nvmlInit_v2 -> SUCCESS");

    unsigned int n = 999;
    CHECK(Count(&n) == NVML_SUCCESS && n == 1, "nvmlDeviceGetCount_v2 -> 1 device");

    nvmlDevice_t dev = NULL;
    CHECK(ByIndex(0, &dev) == NVML_SUCCESS && dev != NULL, "nvmlDeviceGetHandleByIndex_v2(0) -> handle");
    CHECK(ByIndex(1, &dev) == NVML_ERROR_INVALID_ARGUMENT, "index 1 -> INVALID_ARGUMENT");
    ByIndex(0, &dev);

    char name[NVML_DEVICE_NAME_V2_BUFFER_SIZE] = {0};
    CHECK(GetName(dev, name, sizeof(name)) == NVML_SUCCESS, "nvmlDeviceGetName -> SUCCESS");
    CHECK(strcmp(name, exp_name) == 0, "device name matches seed");
    fprintf(stderr, "     name = %s\n", name);

    char uuid[NVML_DEVICE_UUID_BUFFER_SIZE] = {0};
    CHECK(GetUUID(dev, uuid, sizeof(uuid)) == NVML_SUCCESS && strncmp(uuid, "GPU-", 4) == 0, "nvmlDeviceGetUUID -> GPU-...");
    fprintf(stderr, "     uuid = %s\n", uuid);

    nvmlMemory_t mem; memset(&mem, 0, sizeof(mem));
    CHECK(GetMem(dev, &mem) == NVML_SUCCESS, "nvmlDeviceGetMemoryInfo -> SUCCESS");
    CHECK(mem.total == exp_bytes, "total VRAM matches seed");
    fprintf(stderr, "     total = %llu bytes (%llu MB)\n", mem.total, mem.total / (1024*1024));

    int maj = 0, min = 0;
    CHECK(GetCC(dev, &maj, &min) == NVML_SUCCESS && maj == exp_maj && min == exp_min, "compute capability matches seed");
    fprintf(stderr, "     cc = %d.%d\n", maj, min);

    unsigned int temp = 0;
    CHECK(GetTemp(dev, NVML_TEMPERATURE_GPU, &temp) == NVML_SUCCESS, "nvmlDeviceGetTemperature -> SUCCESS");

    unsigned int mw = 0;
    CHECK(GetPower(dev, &mw) == NVML_SUCCESS && mw > 0, "nvmlDeviceGetPowerUsage -> SUCCESS");

    nvmlPciInfo_t pci; memset(&pci, 0, sizeof(pci));
    CHECK(GetPci(dev, &pci) == NVML_SUCCESS && pci.busId[0] != 0, "nvmlDeviceGetPciInfo_v3 -> busId");
    fprintf(stderr, "     pci busId = %s\n", pci.busId);

    unsigned int pcount = 7;
    CHECK(GetProcs(dev, &pcount, NULL) == NVML_SUCCESS && pcount == 0, "compute running processes -> 0");

    char drv[NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE] = {0};
    CHECK(DrvVer(drv, sizeof(drv)) == NVML_SUCCESS && drv[0] != 0, "nvmlSystemGetDriverVersion -> SUCCESS");
    fprintf(stderr, "     driver = %s\n", drv);

    CHECK(strcmp(ErrStr(NVML_ERROR_NOT_SUPPORTED), "Not Supported") == 0, "nvmlErrorString(NOT_SUPPORTED)");
    CHECK(Shutdown() == NVML_SUCCESS, "nvmlShutdown -> SUCCESS");

    if (failures) { fprintf(stderr, "\n%d FAILURE(S)\n", failures); return 1; }
    fprintf(stderr, "\nALL PASS\n");
    return 0;
}
