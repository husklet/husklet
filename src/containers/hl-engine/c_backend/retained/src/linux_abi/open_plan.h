#ifndef HL_LINUX_ABI_OPEN_PLAN_H
#define HL_LINUX_ABI_OPEN_PLAN_H

#include "hl/host_services.h"

#include <stddef.h>
#include <stdint.h>

typedef enum hl_open_kind {
    HL_OPEN_ERROR = 0,
    HL_OPEN_HOST_PATH = 1,
    HL_OPEN_SYNTHETIC = 2,
    HL_OPEN_OVERLAY = 3,
    HL_OPEN_TMPFILE = 4
} hl_open_kind;

typedef enum hl_open_synthetic {
    HL_OPEN_SYNTHETIC_NONE = 0,
    HL_OPEN_SYNTHETIC_PROC = 1,
    HL_OPEN_SYNTHETIC_SYS = 2,
    HL_OPEN_SYNTHETIC_DEV = 3
} hl_open_synthetic;

typedef enum hl_open_overlay {
    HL_OPEN_OVERLAY_NONE = 0,
    HL_OPEN_OVERLAY_LOOKUP = 1,
    HL_OPEN_OVERLAY_COPY_UP = 2,
    HL_OPEN_OVERLAY_CREATE = 3
} hl_open_overlay;

enum {
    HL_OPEN_READ = 1u << 0,
    HL_OPEN_WRITE = 1u << 1,
    HL_OPEN_CREATE = 1u << 2,
    HL_OPEN_TRUNCATE = 1u << 3,
    HL_OPEN_APPEND = 1u << 4,
    HL_OPEN_PATH_ONLY = 1u << 5,
    HL_OPEN_NOFOLLOW = 1u << 6,
    HL_OPEN_DIRECTORY = 1u << 7,
    HL_OPEN_TEMPORARY = 1u << 8,
    /* openat2(RESOLVE_NO_SYMLINKS): reject a symlink in any path component. */
    HL_OPEN_NO_SYMLINKS = 1u << 9
};

typedef struct hl_open_request {
    const char *guest_path;
    size_t guest_path_size;
    hl_host_handle directory;
    uint32_t intent;
    uint32_t overlay;
    uint32_t read_only;
    uint32_t final_symlink;
} hl_open_request;

/*
 * A namespace decision only: it deliberately cannot contain a native fd.
 * `directory` is an opaque host-service handle and `path` remains relative to it.
 */
typedef struct hl_open_plan {
    hl_open_kind kind;
    hl_open_synthetic synthetic;
    hl_open_overlay overlay;
    hl_status error;
    hl_host_handle directory;
    hl_host_handle target;
    uint32_t target_type;
    uint32_t intent;
    uint32_t names_symlink;
    char path[4097];
    size_t path_size;
} hl_open_plan;

hl_status hl_open_plan_build(const hl_open_request *request, hl_open_plan *plan);

#endif
