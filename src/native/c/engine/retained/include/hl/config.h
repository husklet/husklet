#ifndef HL_CONFIG_H
#define HL_CONFIG_H

#include "hl/base.h"
#include "hl/network.h"

HL_EXTERN_C_BEGIN

#define HL_CONFIG_MAGIC UINT32_C(0x484c4346)
/* The one accepted launch generation. Any other value is rejected with HL_STATUS_ABI_MISMATCH -- that is
 * what catches a prebuilt archive built against different headers. */
#define HL_CONFIG_ABI 1u
/* checkpoint_mode: which directions of the store channel this launch uses. Zero disables checkpointing.
 * The channel itself is a descriptor activation hands the engine; nothing here names a location. */
#define HL_CONFIG_CHECKPOINT_CAPTURE 1u
#define HL_CONFIG_CHECKPOINT_RESTORE 2u
/* Zero is "no policy asked for": restore is permissive, capture keeps refusing what it cannot capture. */
#define HL_CONFIG_CHECKPOINT_DEFAULT 0u
#define HL_CONFIG_CHECKPOINT_RECONNECT 1u
#define HL_CONFIG_CHECKPOINT_DISCARD_OPTIONAL 2u
#define HL_CONFIG_CHECKPOINT_REFUSE 3u
#define HL_CONFIG_NETWORK_VIRTUAL 0u
#define HL_CONFIG_NETWORK_ISOLATED 1u
#define HL_CONFIG_NETWORK_HOST 2u
#define HL_LAUNCH_RESULT_MAGIC UINT32_C(0x484c5253)
#define HL_LAUNCH_RESULT_ABI 1u
#define HL_CONFIG_SANDBOX_ENABLED 1u
#define HL_CONFIG_UNTRUSTED_ONLY 2u

typedef struct hl_launch_config {
    uint32_t magic;
    uint32_t pool_size;
    uint32_t header_size;
    uint32_t abi;
    uint64_t memory_limit;
    uint32_t pid_limit;
    uint32_t cpu_limit;
    int32_t uid;
    int32_t gid;
    uint32_t rootfs_read_only;
    uint32_t sandbox;
    uint32_t network_isolated;
    uint32_t publish_external;
    uint32_t rootfs_offset;
    uint32_t lower_layers_offset;
    uint32_t hostname_offset;
    uint32_t network_namespace_offset;
    uint32_t publish_offset;
    uint32_t volumes_offset;
    uint32_t limits_offset;
    uint32_t working_directory_offset;
    uint32_t environment_offset;
    uint32_t translation_cache_offset;
    uint32_t network_bridge_offset;
    uint32_t ip_offset;
    uint32_t filesystem_generation_offset;
    uint32_t arguments_offset;
    uint32_t translation_cache_disabled;
    uint32_t egress_proxy_offset;
    uint32_t debug_log_offset;
    /* HL_CONFIG_CHECKPOINT_CAPTURE|RESTORE. */
    uint32_t checkpoint_mode;
    /* HL_CONFIG_CHECKPOINT_*: behavior when a saved external resource cannot be reconstructed. */
    uint32_t checkpoint_policy;
    /* Existing 0600 result leaf created by the launcher; zero preserves direct CLI exit semantics. */
    uint32_t result_path_offset;
    uint32_t publish_count;
    uint32_t network_interfaces_offset;
    uint32_t file_owners_offset;
    uint32_t reserved;
    /* Opaque host-generated launch ownership domain. Zero is never valid. */
    uint64_t process_domain[2];
    /* Host path whose authority is granted solely for loading the initial main executable. */
    uint32_t executable_host_offset;
    uint32_t network_transport;
    /* NUL-terminated lower path records starting at lower_layers_offset. */
    uint32_t lower_layer_count;
    uint32_t overlay_work_offset;
    /* Newline records: absolute-host-path<TAB>alias[<TAB>alias...]. */
    uint32_t name_binds_offset;
    uint32_t reserved2;
} hl_launch_config;

typedef enum hl_launch_result_kind {
    HL_LAUNCH_RESULT_CODE = 1,
    HL_LAUNCH_RESULT_SIGNAL = 2,
    HL_LAUNCH_RESULT_FAULT = 3,
    HL_LAUNCH_RESULT_ENGINE_ERROR = 4
} hl_launch_result_kind;

typedef struct hl_launch_result {
    uint32_t magic;
    uint32_t abi;
    uint32_t kind;
    int32_t guest_status;
    int32_t engine_status;
    uint32_t reserved;
    uint64_t detail;
} hl_launch_result;


HL_EXTERN_C_END

#endif
