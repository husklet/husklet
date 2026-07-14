/* dd's libnvidia-ml.so.1 — a real NVML implementation that reports a hl-fabricated
 * virtual GPU so the *genuine* closed-source `nvidia-smi` binary runs unmodified.
 *
 * There is no NVIDIA hardware on an Apple-silicon Mac; dd substitutes the driver's
 * user-space entry point (NVML) — the same "ship a drop-in .so" seam ZLUDA uses for
 * libcuda — instead of emulating the closed /dev/nvidia* kernel ioctl ABI. This
 * provides *device presence* (tier 1 of docs/ideas/CUDA_ON_METAL.md); it does NOT
 * provide CUDA compute (that is libcuda/libcudart + PTX->Metal, separate tiers).
 *
 * Device values are seeded at nvmlInit from environment set by dd's launcher:
 *   HL_CUDA_NAME   reported device name        (default "dd Metal (CUDA-sim) Device")
 *   HL_CUDA_CC     compute capability "maj.min" (default "8.6")
 *   HL_CUDA_VRAM   reported VRAM in MB          (default 4096)
 *
 * Exactly ONE device is presented. Unimplemented queries return
 * NVML_ERROR_NOT_SUPPORTED (never crash) so nvidia-smi degrades to "N/A".
 */
#define _GNU_SOURCE
#include "nvml_min.h"
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* ---- fabricated device state (single device) ---- */
static int         g_inited = 0;
static char        g_name[NVML_DEVICE_NAME_V2_BUFFER_SIZE] = "dd Metal (CUDA-sim) Device";
static int         g_cc_major = 8;
static int         g_cc_minor = 6;
static unsigned long long g_vram_bytes = 4096ULL * 1024 * 1024;
static char        g_uuid[NVML_DEVICE_UUID_BUFFER_SIZE] = "GPU-dd000000-0000-4d64-0000-000000000000";
static char        g_serial[NVML_DEVICE_SERIAL_BUFFER_SIZE] = "DD-SIM-00000001";
/* nvidia-smi refuses to run if the NVML-reported DRIVER version's major differs from the driver the
 * nvidia-smi binary itself was built for ("Mismatch in versions between nvidia-smi and NVML"). So the
 * driver/NVML version strings are seeded from env (HL_CUDA_DRIVER / HL_CUDA_NVML) — dd's launcher can
 * set them to match whichever real nvidia-smi is injected. Defaults track a common LTS driver. */
static char        g_driver_version[NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE] = "535.230.02";
static char        g_nvml_version[NVML_SYSTEM_NVML_VERSION_BUFFER_SIZE]     = "12.535.230.02";
static int         g_cuda_driver_version = 12020; /* 12.2 -> maj*1000 + min*10 */

/* A single, stable, non-null handle for the one device. `nvmlDevice_t` is an opaque
 * pointer; we back it with a fixed storage address (its contents are never read). */
static int g_device_obj;
#define HL_DEVICE_HANDLE ((nvmlDevice_t)&g_device_obj)

static void seed_from_env(void) {
    const char* n = getenv("HL_CUDA_NAME");
    if (n && *n) { strncpy(g_name, n, sizeof(g_name) - 1); g_name[sizeof(g_name) - 1] = 0; }
    const char* cc = getenv("HL_CUDA_CC");
    if (cc && *cc) {
        int maj = 0, min = 0;
        if (sscanf(cc, "%d.%d", &maj, &min) >= 1) { g_cc_major = maj; g_cc_minor = min; }
    }
    const char* v = getenv("HL_CUDA_VRAM");
    if (v && *v) {
        char* end = 0;
        unsigned long long mb = strtoull(v, &end, 10);
        if (mb > 0) g_vram_bytes = mb * 1024ULL * 1024ULL;
    }
    /* Driver / NVML version handshake (see comment at g_driver_version). */
    const char* drv = getenv("HL_CUDA_DRIVER");
    if (drv && *drv) {
        strncpy(g_driver_version, drv, sizeof(g_driver_version) - 1);
        g_driver_version[sizeof(g_driver_version) - 1] = 0;
        /* Default the NVML version string to "12.<driver>" unless explicitly overridden below. */
        snprintf(g_nvml_version, sizeof(g_nvml_version), "12.%s", drv);
    }
    const char* nv = getenv("HL_CUDA_NVML");
    if (nv && *nv) {
        strncpy(g_nvml_version, nv, sizeof(g_nvml_version) - 1);
        g_nvml_version[sizeof(g_nvml_version) - 1] = 0;
    }
    const char* cd = getenv("HL_CUDA_DRIVER_CUDA");
    if (cd && *cd) {
        int maj = 0, min = 0;
        if (sscanf(cd, "%d.%d", &maj, &min) >= 1) g_cuda_driver_version = maj * 1000 + min * 10;
    }
}

static int is_valid(nvmlDevice_t d) { return d == HL_DEVICE_HANDLE; }

/* ================= init / shutdown / strings ================= */

static nvmlReturn_t init_impl(void) {
    if (!g_inited) { seed_from_env(); g_inited = 1; }
    return NVML_SUCCESS;
}
nvmlReturn_t nvmlInit_v2(void)                    { return init_impl(); }
nvmlReturn_t nvmlInit(void)                       { return init_impl(); }
nvmlReturn_t nvmlInitWithFlags(unsigned int f)    { (void)f; return init_impl(); }
nvmlReturn_t nvmlShutdown(void)                   { g_inited = 0; return NVML_SUCCESS; }

const char* nvmlErrorString(nvmlReturn_t r) {
    switch (r) {
        case NVML_SUCCESS:                   return "The operation was successful";
        case NVML_ERROR_UNINITIALIZED:       return "Uninitialized";
        case NVML_ERROR_INVALID_ARGUMENT:    return "Invalid Argument";
        case NVML_ERROR_NOT_SUPPORTED:       return "Not Supported";
        case NVML_ERROR_NO_PERMISSION:       return "Insufficient Permissions";
        case NVML_ERROR_ALREADY_INITIALIZED: return "Already Initialized";
        case NVML_ERROR_NOT_FOUND:           return "Not Found";
        case NVML_ERROR_INSUFFICIENT_SIZE:   return "Insufficient Size";
        case NVML_ERROR_DRIVER_NOT_LOADED:   return "Driver Not Loaded";
        default:                             return "Unknown Error";
    }
}

/* ================= system-level version queries ================= */

static nvmlReturn_t copy_str(const char* src, char* dst, unsigned int len) {
    if (!dst || len == 0) return NVML_ERROR_INVALID_ARGUMENT;
    strncpy(dst, src, len - 1);
    dst[len - 1] = 0;
    return NVML_SUCCESS;
}
nvmlReturn_t nvmlSystemGetDriverVersion(char* v, unsigned int len) { return copy_str(g_driver_version, v, len); }
nvmlReturn_t nvmlSystemGetNVMLVersion(char* v, unsigned int len)   { return copy_str(g_nvml_version, v, len); }

nvmlReturn_t nvmlSystemGetCudaDriverVersion(int* v) {
    if (!v) return NVML_ERROR_INVALID_ARGUMENT;
    *v = g_cuda_driver_version; return NVML_SUCCESS;
}
nvmlReturn_t nvmlSystemGetCudaDriverVersion_v2(int* v) { return nvmlSystemGetCudaDriverVersion(v); }

/* ================= device enumeration ================= */

static nvmlReturn_t count_impl(unsigned int* c) {
    if (!g_inited) return NVML_ERROR_UNINITIALIZED;
    if (!c) return NVML_ERROR_INVALID_ARGUMENT;
    *c = 1; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetCount_v2(unsigned int* c) { return count_impl(c); }
nvmlReturn_t nvmlDeviceGetCount(unsigned int* c)    { return count_impl(c); }

static nvmlReturn_t handle_by_index_impl(unsigned int index, nvmlDevice_t* dev) {
    if (!g_inited) return NVML_ERROR_UNINITIALIZED;
    if (!dev) return NVML_ERROR_INVALID_ARGUMENT;
    if (index != 0) return NVML_ERROR_INVALID_ARGUMENT;
    *dev = HL_DEVICE_HANDLE; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetHandleByIndex_v2(unsigned int i, nvmlDevice_t* d) { return handle_by_index_impl(i, d); }
nvmlReturn_t nvmlDeviceGetHandleByIndex(unsigned int i, nvmlDevice_t* d)    { return handle_by_index_impl(i, d); }

nvmlReturn_t nvmlDeviceGetHandleByUUID(const char* uuid, nvmlDevice_t* dev) {
    if (!g_inited) return NVML_ERROR_UNINITIALIZED;
    if (!uuid || !dev) return NVML_ERROR_INVALID_ARGUMENT;
    if (strcmp(uuid, g_uuid) != 0) return NVML_ERROR_NOT_FOUND;
    *dev = HL_DEVICE_HANDLE; return NVML_SUCCESS;
}

static nvmlReturn_t handle_by_pci_impl(const char* pci, nvmlDevice_t* dev) {
    if (!g_inited) return NVML_ERROR_UNINITIALIZED;
    if (!pci || !dev) return NVML_ERROR_INVALID_ARGUMENT;
    /* Only one device on a fixed bus; accept the canonical id. */
    *dev = HL_DEVICE_HANDLE; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetHandleByPciBusId_v2(const char* p, nvmlDevice_t* d) { return handle_by_pci_impl(p, d); }
nvmlReturn_t nvmlDeviceGetHandleByPciBusId(const char* p, nvmlDevice_t* d)    { return handle_by_pci_impl(p, d); }

/* ================= per-device identity ================= */

nvmlReturn_t nvmlDeviceGetName(nvmlDevice_t d, char* name, unsigned int len) {
    if (!is_valid(d)) return NVML_ERROR_INVALID_ARGUMENT;
    return copy_str(g_name, name, len);
}
nvmlReturn_t nvmlDeviceGetUUID(nvmlDevice_t d, char* uuid, unsigned int len) {
    if (!is_valid(d)) return NVML_ERROR_INVALID_ARGUMENT;
    return copy_str(g_uuid, uuid, len);
}
nvmlReturn_t nvmlDeviceGetSerial(nvmlDevice_t d, char* serial, unsigned int len) {
    if (!is_valid(d)) return NVML_ERROR_INVALID_ARGUMENT;
    return copy_str(g_serial, serial, len);
}
nvmlReturn_t nvmlDeviceGetIndex(nvmlDevice_t d, unsigned int* index) {
    if (!is_valid(d) || !index) return NVML_ERROR_INVALID_ARGUMENT;
    *index = 0; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetMinorNumber(nvmlDevice_t d, unsigned int* minor) {
    if (!is_valid(d) || !minor) return NVML_ERROR_INVALID_ARGUMENT;
    *minor = 0; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetBrand(nvmlDevice_t d, nvmlBrandType_t* type) {
    if (!is_valid(d) || !type) return NVML_ERROR_INVALID_ARGUMENT;
    *type = NVML_BRAND_NVS; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetCudaComputeCapability(nvmlDevice_t d, int* major, int* minor) {
    if (!is_valid(d) || !major || !minor) return NVML_ERROR_INVALID_ARGUMENT;
    *major = g_cc_major; *minor = g_cc_minor; return NVML_SUCCESS;
}

/* ================= memory / utilization ================= */

nvmlReturn_t nvmlDeviceGetMemoryInfo(nvmlDevice_t d, nvmlMemory_t* m) {
    if (!is_valid(d) || !m) return NVML_ERROR_INVALID_ARGUMENT;
    m->total = g_vram_bytes;
    m->used  = 0;
    m->free  = g_vram_bytes;
    return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetMemoryInfo_v2(nvmlDevice_t d, nvmlMemory_v2_t* m) {
    if (!is_valid(d) || !m) return NVML_ERROR_INVALID_ARGUMENT;
    /* keep caller-provided version tag; fill the v2 layout */
    m->total = g_vram_bytes;
    m->reserved = 0;
    m->used  = 0;
    m->free  = g_vram_bytes;
    return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetUtilizationRates(nvmlDevice_t d, nvmlUtilization_t* u) {
    if (!is_valid(d) || !u) return NVML_ERROR_INVALID_ARGUMENT;
    u->gpu = 0; u->memory = 0; return NVML_SUCCESS;
}

/* ================= PCI info ================= */

static nvmlReturn_t pci_impl(nvmlDevice_t d, nvmlPciInfo_t* p) {
    if (!is_valid(d) || !p) return NVML_ERROR_INVALID_ARGUMENT;
    memset(p, 0, sizeof(*p));
    p->domain = 0; p->bus = 0; p->device = 0;
    p->pciDeviceId = 0x1EB810DE;   /* fabricated device:vendor (vendor 0x10DE = NVIDIA) */
    p->pciSubSystemId = 0x1EB810DE;
    snprintf(p->busId, sizeof(p->busId), "00000000:00:00.0");
    snprintf(p->busIdLegacy, sizeof(p->busIdLegacy), "0000:00:00.0");
    return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetPciInfo_v3(nvmlDevice_t d, nvmlPciInfo_t* p) { return pci_impl(d, p); }
nvmlReturn_t nvmlDeviceGetPciInfo_v2(nvmlDevice_t d, nvmlPciInfo_t* p) { return pci_impl(d, p); }
nvmlReturn_t nvmlDeviceGetPciInfo(nvmlDevice_t d, nvmlPciInfo_t* p)    { return pci_impl(d, p); }

/* ================= sensors / clocks / power ================= */

nvmlReturn_t nvmlDeviceGetTemperature(nvmlDevice_t d, nvmlTemperatureSensors_t s, unsigned int* t) {
    if (!is_valid(d) || !t) return NVML_ERROR_INVALID_ARGUMENT;
    (void)s; *t = 35; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetPowerUsage(nvmlDevice_t d, unsigned int* mw) {
    if (!is_valid(d) || !mw) return NVML_ERROR_INVALID_ARGUMENT;
    *mw = 25000; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetPowerManagementLimit(nvmlDevice_t d, unsigned int* mw) {
    if (!is_valid(d) || !mw) return NVML_ERROR_INVALID_ARGUMENT;
    *mw = 70000; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetEnforcedPowerLimit(nvmlDevice_t d, unsigned int* mw) {
    if (!is_valid(d) || !mw) return NVML_ERROR_INVALID_ARGUMENT;
    *mw = 70000; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetClockInfo(nvmlDevice_t d, nvmlClockType_t type, unsigned int* mhz) {
    if (!is_valid(d) || !mhz) return NVML_ERROR_INVALID_ARGUMENT;
    *mhz = (type == NVML_CLOCK_MEM) ? 6000 : 1500; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetMaxClockInfo(nvmlDevice_t d, nvmlClockType_t type, unsigned int* mhz) {
    if (!is_valid(d) || !mhz) return NVML_ERROR_INVALID_ARGUMENT;
    *mhz = (type == NVML_CLOCK_MEM) ? 6000 : 1500; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetFanSpeed(nvmlDevice_t d, unsigned int* pct) {
    /* Apple GPU exposes no per-GPU fan tach — realistic to report unsupported (nvidia-smi -> N/A) */
    (void)pct; return is_valid(d) ? NVML_ERROR_NOT_SUPPORTED : NVML_ERROR_INVALID_ARGUMENT;
}

/* ================= modes / states ================= */

nvmlReturn_t nvmlDeviceGetPersistenceMode(nvmlDevice_t d, nvmlEnableState_t* m) {
    if (!is_valid(d) || !m) return NVML_ERROR_INVALID_ARGUMENT;
    *m = NVML_FEATURE_DISABLED; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetDisplayMode(nvmlDevice_t d, nvmlEnableState_t* m) {
    if (!is_valid(d) || !m) return NVML_ERROR_INVALID_ARGUMENT;
    *m = NVML_FEATURE_DISABLED; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetComputeMode(nvmlDevice_t d, nvmlComputeMode_t* m) {
    if (!is_valid(d) || !m) return NVML_ERROR_INVALID_ARGUMENT;
    *m = NVML_COMPUTEMODE_DEFAULT; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetPerformanceState(nvmlDevice_t d, nvmlPstates_t* s) {
    if (!is_valid(d) || !s) return NVML_ERROR_INVALID_ARGUMENT;
    *s = NVML_PSTATE_0; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetMigMode(nvmlDevice_t d, unsigned int* cur, unsigned int* pend) {
    /* MIG unsupported on the sim device; nvidia-smi treats NOT_SUPPORTED as "N/A". */
    (void)cur; (void)pend; return is_valid(d) ? NVML_ERROR_NOT_SUPPORTED : NVML_ERROR_INVALID_ARGUMENT;
}

/* ================= running processes (report none) ================= */

static nvmlReturn_t no_procs(nvmlDevice_t d, unsigned int* count, void* infos) {
    (void)infos;
    if (!is_valid(d) || !count) return NVML_ERROR_INVALID_ARGUMENT;
    *count = 0; /* zero running processes -> caller's array untouched */
    return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetComputeRunningProcesses_v3(nvmlDevice_t d, unsigned int* c, nvmlProcessInfo_t* i)  { return no_procs(d, c, i); }
nvmlReturn_t nvmlDeviceGetComputeRunningProcesses_v2(nvmlDevice_t d, unsigned int* c, void* i)               { return no_procs(d, c, i); }
nvmlReturn_t nvmlDeviceGetComputeRunningProcesses(nvmlDevice_t d, unsigned int* c, void* i)                  { return no_procs(d, c, i); }
nvmlReturn_t nvmlDeviceGetGraphicsRunningProcesses_v3(nvmlDevice_t d, unsigned int* c, nvmlProcessInfo_t* i) { return no_procs(d, c, i); }
nvmlReturn_t nvmlDeviceGetGraphicsRunningProcesses_v2(nvmlDevice_t d, unsigned int* c, void* i)              { return no_procs(d, c, i); }
nvmlReturn_t nvmlDeviceGetGraphicsRunningProcesses(nvmlDevice_t d, unsigned int* c, void* i)                 { return no_procs(d, c, i); }

/* ================= extra device queries (nvitop / gpustat / pynvml / --query-gpu) ================= */

nvmlReturn_t nvmlDeviceGetArchitecture(nvmlDevice_t d, unsigned int* arch) {
    if (!is_valid(d) || !arch) return NVML_ERROR_INVALID_ARGUMENT;
    /* NVML_DEVICE_ARCH_*: KEPLER=2 MAXWELL=3 PASCAL=4 VOLTA=5 TURING=6 AMPERE=7 ADA=8 HOPPER=9.
     * Pick from the reported compute-capability major. */
    unsigned int a;
    switch (g_cc_major) {
        case 3: a = 2; break; case 5: a = 3; break; case 6: a = 4; break; case 7: a = (g_cc_minor >= 5) ? 6 : 5; break;
        case 8: a = (g_cc_minor >= 9) ? 8 : 7; break; case 9: a = 9; break; default: a = 7; break;
    }
    *arch = a; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetPowerState(nvmlDevice_t d, nvmlPstates_t* s) { return nvmlDeviceGetPerformanceState(d, s); }
nvmlReturn_t nvmlDeviceGetVbiosVersion(nvmlDevice_t d, char* v, unsigned int len) {
    if (!is_valid(d)) return NVML_ERROR_INVALID_ARGUMENT;
    return copy_str("00.00.00.00.00", v, len);
}
nvmlReturn_t nvmlDeviceGetCurrPcieLinkGeneration(nvmlDevice_t d, unsigned int* g) {
    if (!is_valid(d) || !g) return NVML_ERROR_INVALID_ARGUMENT; *g = 4; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetMaxPcieLinkGeneration(nvmlDevice_t d, unsigned int* g) {
    if (!is_valid(d) || !g) return NVML_ERROR_INVALID_ARGUMENT; *g = 4; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetCurrPcieLinkWidth(nvmlDevice_t d, unsigned int* w) {
    if (!is_valid(d) || !w) return NVML_ERROR_INVALID_ARGUMENT; *w = 16; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetMaxPcieLinkWidth(nvmlDevice_t d, unsigned int* w) {
    if (!is_valid(d) || !w) return NVML_ERROR_INVALID_ARGUMENT; *w = 16; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetTemperatureThreshold(nvmlDevice_t d, unsigned int type, unsigned int* t) {
    if (!is_valid(d) || !t) return NVML_ERROR_INVALID_ARGUMENT;
    (void)type; *t = 90; return NVML_SUCCESS; /* slowdown/shutdown thresholds */
}
nvmlReturn_t nvmlDeviceGetNumGpuCores(nvmlDevice_t d, unsigned int* n) {
    if (!is_valid(d) || !n) return NVML_ERROR_INVALID_ARGUMENT; *n = 4096; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetMemoryBusWidth(nvmlDevice_t d, unsigned int* w) {
    if (!is_valid(d) || !w) return NVML_ERROR_INVALID_ARGUMENT; *w = 256; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetPciInfoExt(nvmlDevice_t d, void* p) { (void)p; return is_valid(d) ? NVML_ERROR_NOT_SUPPORTED : NVML_ERROR_INVALID_ARGUMENT; }
nvmlReturn_t nvmlDeviceGetEncoderUtilization(nvmlDevice_t d, unsigned int* u, unsigned int* s) {
    if (!is_valid(d) || !u || !s) return NVML_ERROR_INVALID_ARGUMENT; *u = 0; *s = 167000; return NVML_SUCCESS;
}
nvmlReturn_t nvmlDeviceGetDecoderUtilization(nvmlDevice_t d, unsigned int* u, unsigned int* s) {
    if (!is_valid(d) || !u || !s) return NVML_ERROR_INVALID_ARGUMENT; *u = 0; *s = 167000; return NVML_SUCCESS;
}
nvmlReturn_t nvmlSystemGetProcessName(unsigned int pid, char* name, unsigned int len) {
    (void)pid; return copy_str("", name, len);
}

/* ================= private/internal export table (the "dark API") =================
 * nvidia-smi resolves `nvmlInternalGetExportTable` (an undocumented private symbol, table GUID
 * c4fe3e6c-c98f-6c4e-a327-ee696e12f7c4) right after init and uses it two ways:
 *   1. **Version handshake.** If it returns an error / a NULL table, nvidia-smi aborts with
 *      "Mismatch in versions between nvidia-smi and NVML". Returning a valid non-null table clears it.
 *   2. **Preferred internal call path** for the *default* dashboard: it tries an internal-table slot
 *      first and, when that slot returns NVML_ERROR_NOT_SUPPORTED, falls back to the documented PUBLIC
 *      NVML API this shim implements. Verified for count + handle.
 *
 * Layout was reverse-engineered by dumping the REAL libnvidia-ml.so.535.230.02's table: it is an array
 * of 245 8-byte slots; slot[0] is a header = table byte size (0x7a8); slots [1],[2] and ~33 others are
 * NULL; the rest are function pointers (some are the public `nvmlDevice*` symbols, most are internal
 * statics). We replicate that exact shape (header + NULL positions) with every populated slot returning
 * NVML_ERROR_NOT_SUPPORTED, which (a) satisfies the handshake and (b) steers nvidia-smi's query/list
 * modes — `nvidia-smi -L`, `nvidia-smi --query-gpu=… --format=csv` — fully onto our public API, where
 * they render our device correctly.
 *
 * --- RESUMABLE RE MAP (nvidia-smi 535.230.02 aarch64; addrs are absolute, binary is non-PIE) ---
 *   0x41e0c0   display command: a loop that repeatedly calls the version-gated iterator 0x447558; on
 *              w0==0 it calls the enumerate fn (0x41e4b4 -> 0x442b80); on w0!=0 (observed 0xd=13) it
 *              jumps to 0x41e4a0 = `mov w0,#999` + "Internal NVML error" and exits 255.
 *   0x447558   nvidia-smi's OWN version-gated dispatcher: caches a handler fnptr at *0x4bcfe8, guards it
 *              with a version compare (*0x4bcfe0 vs *0x4bd228), else resolves via a registry at *0x4bd200;
 *              tail-calls the resolved handler. This drives the whole default render/query iteration.
 *   0x442b80   enumerate-devices: loops indices, per device calls 0x442588 (bus-id) then 0x442848 (parse).
 *   0x442588   get-bus-id wrapper: obtains the internal table object (session global *(0x4b5000+3584) ->
 *              helper 0x452690), checks obj[0] (byte size) > 0x288, then calls obj[648] = SLOT 81 with
 *              (index, buf, size). => internal table is indexed by BYTE offset; slot N is at obj[N*8].
 *   SLOT 81 (obj[648])  get-PCI-bus-id-by-index — real lib +0x34b88: snprintf(buf,size,"%08x:%02x:%02x.0").
 *   0x442848   parse/match a bus-id string into domain/bus/dev (strtoul/strchr), also uses the table obj.
 * With SLOT 81 implemented, 0x442588 + 0x442848 + 0x442b80 all return SUCCESS (verified via ptrace) — the
 * enumerate phase works. The remaining wall is the 0x447558 version-gated iterator (returns 13), whose
 * handler is nvidia-smi's private render subsystem; reversing it is unbounded (each fn reveals 2-3 more,
 * driven by more private table slots with per-slot struct layouts). NEXT resumable step: single-step the
 * resolved 0x447558 handler to enumerate the slots its render path pulls, and reverse each in real-lib.
 *
 * The *default* full-table dashboard (`nvidia-smi` with no args) + `-q` instead run through nvidia-smi's
 * OWN version-gated C++ command dispatcher (binary offset 0x447558; it caches a per-NVML-version handler
 * pointer and tail-calls it), whose render pipeline consumes these internal-table slots directly. Deeper
 * static RE against the real driver (disassembling the real lib's slot functions + ptrace-tracing the
 * stock binary) established the contract for the first slots — e.g. **slot[81] is get-PCI-bus-id-by-index**
 * (real lib off 0x34b88: `snprintf(buf, size, "%08x:%02x:%02x.0", domain, bus, dev)`, args = index / out
 * buffer / buffer-size), and device count + handle come from the *public* API — but the full dashboard
 * pulls many more private slots, each with its OWN undocumented arg/output-struct signature (returning a
 * blind success from all of them SIGSEGVs, proving they are not uniform). Reproducing the whole pipeline is
 * slot-by-slot RE of NVIDIA's closed internal device-info ABI from a stripped binary — neither ZLUDA (which
 * returns NOT_SUPPORTED for nvmlInternalGetExportTable) nor any public source provides it, and it is the
 * same closed-ABI class this project deliberately does not emulate (cf. /dev/nvidia* ioctls). Populating a
 * *partial* slot set would only make the stock render crash/misbehave, so we keep every slot NOT_SUPPORTED:
 * that cleanly steers the list/query modes onto our public API (they render the dd device for real) and
 * makes the default dashboard fail cleanly. **Query/list = real; default dashboard = closed-ABI boundary.** */
static nvmlReturn_t hl_et_notsup(void) { return NVML_ERROR_NOT_SUPPORTED; }
#define HL_ET_SLOTS 245                     /* matches real libnvidia-ml.so.535.230.02 */
#define HL_ET_HEADER 0x7a8                  /* real slot[0] value (table byte size) */
static void* g_export_table[HL_ET_SLOTS];
nvmlReturn_t nvmlInternalGetExportTable(const void** ppExportTable, void* pExportTableId) {
    (void)pExportTableId;
    if (!ppExportTable) return NVML_ERROR_INVALID_ARGUMENT;
    if (!g_export_table[0]) {
        /* NULL slot positions observed in the real table (besides the header at [0]). */
        static const int nulls[] = {1,2,24,35,60,64,90,104,121,122,139,150,157,158,159,160,161,162,
                                     163,167,176,177,178,187,190,191,198,201,202,207,211,216,217,235,236};
        g_export_table[0] = (void*)(size_t)HL_ET_HEADER;
        for (int i = 1; i < HL_ET_SLOTS; i++) g_export_table[i] = (void*)hl_et_notsup;
        for (unsigned k = 0; k < sizeof(nulls) / sizeof(nulls[0]); k++) g_export_table[nulls[k]] = NULL;
    }
    *ppExportTable = g_export_table;
    return NVML_SUCCESS;
}
