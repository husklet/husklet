/* Minimal NVML ABI declarations for dd's libnvidia-ml.so.1 shim.
 *
 * These mirror the *documented* NVIDIA Management Library (NVML) C ABI from
 * nvml.h — enum values, struct layouts and function signatures — so the genuine,
 * closed-source `nvidia-smi` binary links against OUR implementation and reports a
 * hl-fabricated virtual device. We only declare what the shim needs; the layouts
 * (field order/size/padding) and the versioned symbol names (`_v2`/`_v3`) are the
 * load-bearing part and match NVIDIA's headers exactly.
 *
 * This is a clean-room re-declaration of a published C ABI (no NVIDIA source).
 */
#ifndef HL_NVML_MIN_H
#define HL_NVML_MIN_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* --- return codes (subset; values from nvml.h) --- */
typedef enum nvmlReturn_enum {
    NVML_SUCCESS = 0,
    NVML_ERROR_UNINITIALIZED = 1,
    NVML_ERROR_INVALID_ARGUMENT = 2,
    NVML_ERROR_NOT_SUPPORTED = 3,
    NVML_ERROR_NO_PERMISSION = 4,
    NVML_ERROR_ALREADY_INITIALIZED = 5,
    NVML_ERROR_NOT_FOUND = 6,
    NVML_ERROR_INSUFFICIENT_SIZE = 7,
    NVML_ERROR_DRIVER_NOT_LOADED = 9,
    NVML_ERROR_UNKNOWN = 999
} nvmlReturn_t;

/* opaque device handle */
typedef struct nvmlDevice_st* nvmlDevice_t;

/* --- buffer size constants (from nvml.h) --- */
#define NVML_DEVICE_UUID_BUFFER_SIZE                80
#define NVML_DEVICE_UUID_V2_BUFFER_SIZE             96
#define NVML_DEVICE_NAME_BUFFER_SIZE                64
#define NVML_DEVICE_NAME_V2_BUFFER_SIZE             96
#define NVML_DEVICE_SERIAL_BUFFER_SIZE              30
#define NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE      80
#define NVML_SYSTEM_NVML_VERSION_BUFFER_SIZE        80
#define NVML_DEVICE_PCI_BUS_ID_BUFFER_SIZE          32
#define NVML_DEVICE_PCI_BUS_ID_BUFFER_V2_SIZE       16

/* --- nvmlMemory_t (v1) --- */
typedef struct nvmlMemory_st {
    unsigned long long total;
    unsigned long long free;
    unsigned long long used;
} nvmlMemory_t;

/* --- nvmlMemory_v2_t --- version-tagged, extra `reserved` field --- */
typedef struct nvmlMemory_v2_st {
    unsigned int version;
    unsigned long long total;
    unsigned long long reserved;
    unsigned long long free;
    unsigned long long used;
} nvmlMemory_v2_t;

/* --- nvmlUtilization_t --- */
typedef struct nvmlUtilization_st {
    unsigned int gpu;
    unsigned int memory;
} nvmlUtilization_t;

/* --- nvmlPciInfo_t (current / _v3 layout) --- */
typedef struct nvmlPciInfo_st {
    char busIdLegacy[NVML_DEVICE_PCI_BUS_ID_BUFFER_V2_SIZE];
    unsigned int domain;
    unsigned int bus;
    unsigned int device;
    unsigned int pciDeviceId;
    unsigned int pciSubSystemId;
    char busId[NVML_DEVICE_PCI_BUS_ID_BUFFER_SIZE];
} nvmlPciInfo_t;

/* --- process info (v3 layout) --- */
typedef struct nvmlProcessInfo_st {
    unsigned int pid;
    unsigned long long usedGpuMemory;
    unsigned int gpuInstanceId;
    unsigned int computeInstanceId;
} nvmlProcessInfo_t;

/* --- enums --- */
typedef enum nvmlTemperatureSensors_enum { NVML_TEMPERATURE_GPU = 0 } nvmlTemperatureSensors_t;
typedef enum nvmlClockType_enum {
    NVML_CLOCK_GRAPHICS = 0, NVML_CLOCK_SM = 1, NVML_CLOCK_MEM = 2, NVML_CLOCK_VIDEO = 3
} nvmlClockType_t;
typedef enum nvmlComputeMode_enum {
    NVML_COMPUTEMODE_DEFAULT = 0, NVML_COMPUTEMODE_EXCLUSIVE_THREAD = 1,
    NVML_COMPUTEMODE_PROHIBITED = 2, NVML_COMPUTEMODE_EXCLUSIVE_PROCESS = 3
} nvmlComputeMode_t;
typedef enum nvmlEnableState_enum { NVML_FEATURE_DISABLED = 0, NVML_FEATURE_ENABLED = 1 } nvmlEnableState_t;
typedef enum nvmlBrandType_enum {
    NVML_BRAND_UNKNOWN = 0, NVML_BRAND_QUADRO = 1, NVML_BRAND_TESLA = 2, NVML_BRAND_NVS = 3,
    NVML_BRAND_GRID = 4, NVML_BRAND_GEFORCE = 5, NVML_BRAND_TITAN = 6
} nvmlBrandType_t;
typedef enum nvmlPstates_enum { NVML_PSTATE_0 = 0, NVML_PSTATE_UNKNOWN = 32 } nvmlPstates_t;

#ifdef __cplusplus
}
#endif
#endif /* HL_NVML_MIN_H */
