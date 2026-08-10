#ifndef HL_BASE_H
#define HL_BASE_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32) && defined(HL_SHARED)
#if defined(HL_BUILDING_ENGINE)
#define HL_API __declspec(dllexport)
#else
#define HL_API __declspec(dllimport)
#endif
#elif defined(__GNUC__) || defined(__clang__)
#define HL_API __attribute__((visibility("default")))
#else
#define HL_API
#endif

#ifdef __cplusplus
#define HL_EXTERN_C_BEGIN extern "C" {
#define HL_EXTERN_C_END }
#else
#define HL_EXTERN_C_BEGIN
#define HL_EXTERN_C_END
#endif

#define HL_ARRAY_COUNT(value) (sizeof(value) / sizeof((value)[0]))
#define HL_ABI_HEADER                                                                                                  \
    uint32_t abi;                                                                                                      \
    uint32_t size

typedef enum hl_status {
    HL_STATUS_OK = 0,
    HL_STATUS_INVALID_ARGUMENT = 1,
    HL_STATUS_ABI_MISMATCH = 2,
    HL_STATUS_NOT_SUPPORTED = 3,
    HL_STATUS_OUT_OF_MEMORY = 4,
    HL_STATUS_RESOURCE_LIMIT = 5,
    HL_STATUS_NOT_FOUND = 6,
    HL_STATUS_ALREADY_EXISTS = 7,
    HL_STATUS_PERMISSION_DENIED = 8,
    HL_STATUS_WOULD_BLOCK = 9,
    HL_STATUS_INTERRUPTED = 10,
    HL_STATUS_IO = 11,
    HL_STATUS_PLATFORM_FAILURE = 12,
    HL_STATUS_CORRUPT = 13,
    HL_STATUS_BUSY = 14,
    HL_STATUS_NOT_DIRECTORY = 15,
    HL_STATUS_IS_DIRECTORY = 16,
    HL_STATUS_NAME_TOO_LONG = 17,
    HL_STATUS_SYMLINK_LOOP = 18,
    HL_STATUS_READ_ONLY = 19,
    HL_STATUS_DISCONNECTED = 20,
    HL_STATUS_PROCESS_LIMIT = 21,
    HL_STATUS_CROSS_DEVICE = 22,
    HL_STATUS_NOT_EMPTY = 23,
    HL_STATUS_NO_SPACE = 24,
    HL_STATUS_QUOTA = 25,
    HL_STATUS_FILE_TOO_LARGE = 26,
    HL_STATUS_TIMED_OUT = 27,
    HL_STATUS_CONNECTION_REFUSED = 28,
    HL_STATUS_CONNECTION_RESET = 29,
    HL_STATUS_NETWORK_UNREACHABLE = 30,
    HL_STATUS_ADDRESS_IN_USE = 31
} hl_status;

#endif
