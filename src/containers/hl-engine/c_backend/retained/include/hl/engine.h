#ifndef HL_ENGINE_H
#define HL_ENGINE_H

#include "hl/base.h"
#include "hl/host_services.h"
#include "hl/network.h"

HL_EXTERN_C_BEGIN

#define HL_ENGINE_ABI 5u
/* The one accepted box generation. A box declaring anything else, or smaller than the struct below, is
 * rejected outright -- there is no per-generation field-presence gating to read past. */
#define HL_ENGINE_BOX_ABI 1u

typedef struct hl_engine hl_engine;

/* Highest descriptor number available to a guest launch; descriptors at and above this value are reserved
 * for engine-private host authority. Returns zero when the host cannot provide a disjoint range. */
HL_API uint32_t hl_engine_guest_fd_limit(void);

typedef enum hl_guest_isa { HL_GUEST_ISA_AARCH64 = 1, HL_GUEST_ISA_X86_64 = 2 } hl_guest_isa;

typedef enum hl_engine_exit_kind {
    HL_ENGINE_EXIT_NONE = 0,
    HL_ENGINE_EXIT_CODE = 1,
    HL_ENGINE_EXIT_SIGNAL = 2,
    HL_ENGINE_EXIT_FAULT = 3,
    HL_ENGINE_EXIT_ENGINE_ERROR = 4
} hl_engine_exit_kind;

typedef enum hl_engine_request_kind {
    HL_ENGINE_REQUEST_INTERRUPT = 1,
    HL_ENGINE_REQUEST_FORCE_STOP = 2,
    HL_ENGINE_REQUEST_SIGNAL = 3
} hl_engine_request_kind;

typedef enum hl_engine_fd_ownership {
    /* Ownership transfers only after hl_engine_create succeeds. */
    HL_ENGINE_FD_TRANSFER = 1,
    /* Engine clones the handle and never closes the caller's original. */
    HL_ENGINE_FD_BORROW = 2
} hl_engine_fd_ownership;

typedef struct hl_engine_fd_binding {
    HL_ABI_HEADER;
    uint32_t guest_fd;
    uint32_t status_flags;
    uint32_t descriptor_flags;
    uint32_t ownership;
    hl_host_handle host_handle;
} hl_engine_fd_binding;

typedef struct hl_engine_executable {
    HL_ABI_HEADER;
    uint32_t ownership;
    uint32_t reserved;
    hl_host_handle host_handle;
    /* Engine-populated immutable image. Callers must pass NULL/zero. */
    const void *image;
    size_t image_size;
} hl_engine_executable;

enum {
    HL_ENGINE_BOX_ROOTFS_READ_ONLY = 1u << 0,
    HL_ENGINE_BOX_SANDBOX = 1u << 1,
    HL_ENGINE_BOX_NETWORK_ISOLATED = 1u << 2,
    HL_ENGINE_BOX_PUBLISH_EXTERNAL = 1u << 3,
    HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED = 1u << 4,
    /* Route authority operations through the sentry without host worker confinement. */
    HL_ENGINE_BOX_SENTRY_ONLY = 1u << 5
};

/*
 * Generic Linux-box settings.  Every pointed-to string is copied by
 * hl_engine_create; the caller may release or change it after that call.
 * uid/gid use -1 to inherit the engine default.
 */
typedef struct hl_engine_box_config {
    HL_ABI_HEADER;
    uint32_t flags;
    int32_t uid;
    int32_t gid;
    uint32_t reserved;
    const char *working_directory;
    const char *hostname;
    /* Newline-separated [A-Za-z_][A-Za-z0-9_]*=VALUE records; NULL selects engine defaults. */
    const char *environment;
    /* Owned launcher settings. NULL means unspecified; non-NULL strings must be nonempty. */
    const char *lower_layers;
    const hl_engine_publish_rule *publish;
    uint32_t publish_count;
    const char *volumes;
    const char *limits;
    const char *network_namespace;
    const char *translation_cache;
    const char *network_bridge;
    const char *ip;
    const char *filesystem_generation;
    const char *egress_proxy;
    /* Newline-separated normalized-relative-path<TAB>uid<TAB>gid records. */
    const char *file_owners;
    /* HL_CONFIG_CHECKPOINT_CAPTURE|RESTORE; zero disables checkpointing. The image travels over the
     * store channel activation hands the engine -- there is no location to name here. */
    uint32_t checkpoint_mode;
    /* HL_CONFIG_CHECKPOINT_*; zero asks for no policy (permissive restore). */
    uint32_t checkpoint_policy;
} hl_engine_box_config;

typedef struct hl_engine_config {
    HL_ABI_HEADER;
    uint32_t guest_isa;
    /* Must be zero. No public engine flags are currently defined. */
    uint32_t flags;
    uint64_t memory_limit;
    uint32_t pid_limit;
    uint32_t cpu_limit;
    /* Reserved for a future executable-image API. Nonempty payloads are not currently supported. */
    const void *payload;
    size_t payload_size;
    /* Optional Linux root filesystem path copied by hl_engine_create. */
    const char *rootfs;
    const hl_engine_fd_binding *fd_bindings;
    uint32_t fd_binding_count;
    /* Must be zero. */
    uint32_t reserved;
    /* Optional, ABI-versioned Linux-box settings. */
    const hl_engine_box_config *box;
    /* Optional authority for the initial main executable only. Never reused by exec or an interpreter. */
    const hl_engine_executable *executable;
} hl_engine_config;

typedef struct hl_engine_exit {
    HL_ABI_HEADER;
    uint32_t kind;
    int32_t guest_status;
    uint64_t detail;
} hl_engine_exit;

HL_API uint32_t hl_engine_abi(void);
HL_API const char *hl_engine_version(void);
/* host, its callback-group tables, and its context remain valid until destroy. Config strings are copied. */
HL_API hl_status hl_engine_create(const hl_engine_config *config, const hl_host_services *host, hl_engine **out_engine);
HL_API hl_status hl_engine_run(hl_engine *engine, int argc, const char *const argv[], hl_engine_exit *out_exit);
HL_API hl_status hl_engine_request(hl_engine *engine, uint32_t request, const void *data, size_t data_size);
/* May be called from another thread after run has started. It force-stops and joins the active run before freeing. */
HL_API void hl_engine_destroy(hl_engine *engine);

HL_EXTERN_C_END

#endif
