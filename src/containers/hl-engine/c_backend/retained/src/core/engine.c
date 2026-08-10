#include "hl/engine.h"
#include "hl/config.h"
#include "hl/linux_abi.h"
#include "engine_backend.h"
#include "options.h"

#include <stdlib.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

/* One process may embed every guest translator.  Keep registration keyed by
 * guest ISA: a constructor for one backend must never overwrite another. */
static const hl_engine_backend *production_backends[HL_GUEST_ISA_X86_64 + 1];

void hl_engine_backend_register(const hl_engine_backend *backend) {
    if (backend == NULL || backend->guest_isa > HL_GUEST_ISA_X86_64) return;
    production_backends[backend->guest_isa] = backend;
}

struct hl_engine {
    hl_engine_config config;
    hl_host_services host;
    const hl_engine_backend *backend;
    atomic_flag lock;
    hl_host_handle process;
    uint32_t state;
    uint32_t pending_termination;
    hl_linux_abi box;
    hl_linux_fd_entry *box_fds;
    hl_linux_ofd_entry *box_ofds;
    uint32_t box_initialized;
    hl_options options;
    uint32_t options_initialized;
    hl_engine_box_config box_config;
    char *owned_rootfs;
    char *owned_working_directory;
    char *owned_hostname;
    char *owned_environment;
    char *owned_box_strings[11];
    hl_engine_publish_rule *owned_publish;
    hl_host_handle executable;
    hl_engine_executable executable_config;
    unsigned char *owned_executable_image;
};

enum {
    HL_ENGINE_CREATED = 0,
    HL_ENGINE_STARTING = 1,
    HL_ENGINE_RUNNING = 2,
    HL_ENGINE_FINISHED = 3,
    HL_ENGINE_DESTROYING = 4
};

static void hl_engine_lock(hl_engine *engine) {
    while (atomic_flag_test_and_set_explicit(&engine->lock, memory_order_acquire)) {}
}

static void hl_engine_unlock(hl_engine *engine) {
    atomic_flag_clear_explicit(&engine->lock, memory_order_release);
}

static void hl_engine_yield(hl_engine *engine) {
    hl_host_result now = engine->host.clock->monotonic_ns(engine->host.context);
    uint64_t deadline;
    if (now.status != HL_STATUS_OK) return;
    deadline = now.value == UINT64_MAX ? UINT64_MAX : now.value + 1u;
    (void)engine->host.clock->sleep_until(engine->host.context, HL_HOST_CLOCK_MONOTONIC, deadline);
}

uint32_t hl_engine_abi(void) {
    return HL_ENGINE_ABI;
}

const char *hl_engine_version(void) {
    return "0.1.2";
}

enum { HL_ENGINE_STRING_LIMIT = 64 * 1024 * 1024 };

enum { HL_ENGINE_EXECUTABLE_LIMIT = 64 * 1024 * 1024 };

static hl_status hl_engine_read_executable(hl_engine *engine, hl_host_handle handle) {
    hl_host_file_metadata before = {0}, after = {0};
    uint64_t offset = 0;
    if (engine->host.file->metadata(engine->host.context, handle, &before).status != HL_STATUS_OK)
        return HL_STATUS_PLATFORM_FAILURE;
    if (before.type != HL_HOST_FILE_TYPE_REGULAR || before.size == 0 || before.size > HL_ENGINE_EXECUTABLE_LIMIT)
        return HL_STATUS_INVALID_ARGUMENT;
    engine->executable_config.image_size = (size_t)before.size;
    engine->owned_executable_image = malloc((size_t)before.size);
    if (engine->owned_executable_image == NULL) return HL_STATUS_OUT_OF_MEMORY;
    while (offset < before.size) {
        hl_host_result read =
            engine->host.file->read_at(engine->host.context, handle, offset,
                                       (hl_host_bytes){engine->owned_executable_image + offset, before.size - offset});
        if (read.status != HL_STATUS_OK || read.value == 0 || read.value > before.size - offset)
            return HL_STATUS_PLATFORM_FAILURE;
        offset += read.value;
    }
    if (engine->host.file->metadata(engine->host.context, handle, &after).status != HL_STATUS_OK)
        return HL_STATUS_PLATFORM_FAILURE;
    if (before.stable_device != after.stable_device || before.stable_object != after.stable_object ||
        before.size != after.size || before.modified_ns != after.modified_ns || before.changed_ns != after.changed_ns)
        return HL_STATUS_BUSY;
    engine->executable_config.image = engine->owned_executable_image;
    engine->executable_config.image_size = (size_t)before.size;
    return HL_STATUS_OK;
}

static char *hl_engine_copy_string(const char *value) {
    size_t length;
    char *copy;
    if (value == NULL) return NULL;
    for (length = 0; length < HL_ENGINE_STRING_LIMIT && value[length] != 0; ++length) {}
    if (length == HL_ENGINE_STRING_LIMIT) return NULL;
    copy = malloc(length + 1);
    if (copy != NULL) memcpy(copy, value, length + 1);
    return copy;
}

static int hl_engine_set_option(hl_options *options, const char *name, const char *value) {
    return value == NULL || value[0] == 0 ? 0 : hl_options_set(options, name, value, 1);
}

static int hl_engine_name_start(unsigned char c) {
    return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || c == '_';
}

static int hl_engine_name_continue(unsigned char c) {
    return hl_engine_name_start(c) || (c >= '0' && c <= '9');
}

static int hl_engine_environment_valid(const char *environment) {
    size_t offset = 0;
    if (environment == NULL) return 1;
    if (environment[0] == 0) return 0;
    while (offset < HL_ENGINE_STRING_LIMIT && environment[offset] != 0) {
        if (!hl_engine_name_start((unsigned char)environment[offset])) return 0;
        do {
            ++offset;
        } while (offset < HL_ENGINE_STRING_LIMIT && hl_engine_name_continue((unsigned char)environment[offset]));
        if (offset == HL_ENGINE_STRING_LIMIT) return 0;
        if (environment[offset++] != '=') return 0;
        while (offset < HL_ENGINE_STRING_LIMIT && environment[offset] != 0 && environment[offset] != '\n')
            ++offset;
        if (offset == HL_ENGINE_STRING_LIMIT) return 0;
        if (environment[offset] == '\n') {
            ++offset;
            if (environment[offset] == 0) return 0;
        }
    }
    return offset < HL_ENGINE_STRING_LIMIT;
}

static int hl_engine_hostname_valid(const char *hostname) {
    size_t length = 0;
    if (hostname == NULL) return 1;
    while (length <= 64 && hostname[length] != 0) {
        unsigned char c = (unsigned char)hostname[length];
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-')) return 0;
        ++length;
    }
    return length != 0 && length <= 64 && hostname[0] != '-' && hostname[length - 1] != '-';
}

static int hl_engine_nonempty_string(const char *value) {
    size_t length = 0;
    if (value == NULL) return 1;
    while (length < HL_ENGINE_STRING_LIMIT && value[length] != 0)
        ++length;
    return length != 0 && length < HL_ENGINE_STRING_LIMIT;
}

static int hl_engine_absolute_string(const char *value) {
    return value == NULL || (value[0] == '/' && hl_engine_nonempty_string(value));
}

static int hl_engine_uint_range(const char *begin, const char *end, unsigned maximum) {
    unsigned value = 0;
    if (begin == end) return 0;
    while (begin != end) {
        if (*begin < '0' || *begin > '9' || value > (maximum - (unsigned)(*begin - '0')) / 10u) return 0;
        value = value * 10u + (unsigned)(*begin++ - '0');
    }
    return value != 0 && value <= maximum;
}

static int hl_engine_volumes_valid(const char *spec) {
    const char *entry = spec;
    unsigned count = 0;
    if (spec == NULL) return 1;
    while (*entry != 0) {
        const char *end = strchr(entry, ',');
        const char *colon;
        if (end == NULL) end = entry + strlen(entry);
        if (++count > 32) return 0;
        if ((size_t)(end - entry) > 3 && ((entry[0] == 'r' && entry[1] == 'o' && entry[2] == ':') ||
                                          (entry[0] == 'r' && entry[1] == 'w' && entry[2] == ':')))
            entry += 3;
        colon = memchr(entry, ':', (size_t)(end - entry));
        if (colon == NULL || entry == colon || colon + 1 == end || entry[0] != '/' || colon[1] != '/' ||
            memchr(colon + 1, ':', (size_t)(end - colon - 1)) != NULL)
            return 0;
        if (*end == 0) return 1;
        entry = end + 1;
    }
    return 0;
}

static int hl_engine_lower_valid(const char *spec) {
    const char *entry = spec;
    if (spec == NULL) return 1;
    while (*entry != 0) {
        const char *end = strchr(entry, ':');
        if (end == NULL) end = entry + strlen(entry);
        if (entry == end || *entry != '/') return 0;
        if (*end == 0) return 1;
        entry = end + 1;
    }
    return 0;
}

static int hl_engine_limits_valid(const char *spec) {
    const char *entry = spec;
    if (spec == NULL) return 1;
    while (*entry != 0) {
        const char *end = strchr(entry, ',');
        const char *equals;
        const char *colon;
        if (end == NULL) end = entry + strlen(entry);
        equals = memchr(entry, '=', (size_t)(end - entry));
        if (equals == NULL || equals == entry || equals + 1 == end) return 0;
        colon = memchr(equals + 1, ':', (size_t)(end - equals - 1));
        if (colon != NULL &&
            (colon == equals + 1 || colon + 1 == end || memchr(colon + 1, ':', (size_t)(end - colon - 1)) != NULL))
            return 0;
        {
            const char *value = equals + 1;
            const char *value_end = colon == NULL ? end : colon;
            for (;;) {
                const char *cursor = value;
                int special = (size_t)(value_end - value) == 9 && memcmp(value, "unlimited", 9) == 0;
                if (!special && (size_t)(value_end - value) == 2 && value[0] == '-' && value[1] == '1') special = 1;
                if (!special) {
                    if (cursor == value_end) return 0;
                    while (cursor != value_end && *cursor >= '0' && *cursor <= '9')
                        ++cursor;
                    if (cursor != value_end) return 0;
                }
                if (colon == NULL || value == colon + 1) break;
                value = colon + 1;
                value_end = end;
            }
        }
        if (*end == 0) return 1;
        entry = end + 1;
    }
    return 0;
}

static int hl_engine_identity_valid(const char *value, size_t maximum) {
    size_t index = 0;
    if (value == NULL) return 1;
    while (index < maximum && value[index] != 0) {
        unsigned char c = (unsigned char)value[index++];
        if (!hl_engine_name_continue(c) && c != '-' && c != '.') return 0;
    }
    return index != 0 && index <= maximum && value[index] == 0;
}

static int hl_engine_ip_valid(const char *value) {
    unsigned part = 0, digits = 0, separators = 0;
    if (value == NULL) return 1;
    while (*value != 0) {
        if (*value >= '0' && *value <= '9') {
            part = part * 10u + (unsigned)(*value - '0');
            if (++digits > 3 || part > 255) return 0;
        } else if (*value == '.' && digits != 0 && separators < 3) {
            ++separators;
            part = 0;
            digits = 0;
        } else
            return 0;
        ++value;
    }
    return separators == 3 && digits != 0;
}

static int hl_engine_proxy_valid(const char *value) {
    const char *colon;
    if (value == NULL) return 1;
    colon = strrchr(value, ':');
    return colon != NULL && colon != value && hl_engine_uint_range(colon + 1, value + strlen(value), 65535);
}

/* Hand-laid X-macro table: clang-format is NOT idempotent on this
 * continuation-backslash block (formatting it twice yields different output), so
 * `format` followed by `format-check` could never converge and the check could not
 * be gated in CI. One field per line, by intent.
 * NOTE: the off/on markers must be the ENTIRE comment -- trailing prose on the same
 * line makes clang-format ignore them. */
/* clang-format off */
#define HL_BOX_STRING_FIELDS(X)                                                                                        \
    X(lower_layers, "HL_LOWER")                                                                                        \
    X(volumes, "HL_VOLUMES")                                                                                           \
    X(limits, "HL_ULIMITS")                                                                                            \
    X(network_namespace, "HL_NETNS") X(translation_cache, "HL_PCACHE_DIR") X(network_bridge, "HL_NETBR")               \
        X(ip, "HL_IP") X(filesystem_generation, "HL_FSGEN_FILE") X(egress_proxy, "HL_EGRESS_SOCKS")                    \
            X(file_owners, "HL_FILE_OWNERS")
/* clang-format on */

static hl_status hl_engine_apply_box(hl_engine *engine, const hl_engine_box_config *box) {
    char number[32];
    char publish[1024];
    uint32_t known_flags = HL_ENGINE_BOX_ROOTFS_READ_ONLY | HL_ENGINE_BOX_SANDBOX | HL_ENGINE_BOX_NETWORK_ISOLATED |
                           HL_ENGINE_BOX_PUBLISH_EXTERNAL | HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED |
                           HL_ENGINE_BOX_SENTRY_ONLY;
    if (box == NULL) return HL_STATUS_OK;
    /* One accepted generation. An undersized box is rejected rather than partially read. */
    if (box->abi != HL_ENGINE_BOX_ABI || box->size < sizeof(*box)) return HL_STATUS_ABI_MISMATCH;
    if ((box->flags & ~known_flags) != 0 || box->reserved != 0 || box->uid < -1 || box->gid < -1 ||
        box->checkpoint_policy > HL_CONFIG_CHECKPOINT_REFUSE ||
        (box->checkpoint_mode & ~(HL_CONFIG_CHECKPOINT_CAPTURE | HL_CONFIG_CHECKPOINT_RESTORE)) != 0)
        return HL_STATUS_INVALID_ARGUMENT;
    if (box->working_directory != NULL && box->working_directory[0] != '/') return HL_STATUS_INVALID_ARGUMENT;
    if (!hl_engine_hostname_valid(box->hostname) || !hl_engine_environment_valid(box->environment))
        return HL_STATUS_INVALID_ARGUMENT;
#define VALIDATE_BOX_STRING(field, option)                                                                             \
    if (!hl_engine_nonempty_string(box->field)) return HL_STATUS_INVALID_ARGUMENT;
    HL_BOX_STRING_FIELDS(VALIDATE_BOX_STRING)
#undef VALIDATE_BOX_STRING
    if (!hl_engine_absolute_string(box->translation_cache) || !hl_engine_absolute_string(box->filesystem_generation) ||
        !hl_engine_lower_valid(box->lower_layers) || box->publish_count > 32 ||
        ((box->publish_count == 0) != (box->publish == NULL)) || !hl_engine_volumes_valid(box->volumes) ||
        !hl_engine_limits_valid(box->limits) || !hl_engine_identity_valid(box->network_namespace, 39) ||
        !hl_engine_identity_valid(box->network_bridge, 40) || !hl_engine_ip_valid(box->ip) ||
        !hl_engine_proxy_valid(box->egress_proxy) ||
        ((box->flags & HL_ENGINE_BOX_SANDBOX) && (box->flags & HL_ENGINE_BOX_SENTRY_ONLY)) ||
        ((box->flags & HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED) && box->translation_cache != NULL) ||
        (box->ip != NULL && box->network_bridge == NULL) ||
        ((box->flags & HL_ENGINE_BOX_PUBLISH_EXTERNAL) && box->publish_count == 0) ||
        ((box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) &&
         (box->publish_count != 0 || box->network_bridge != NULL || box->ip != NULL || box->egress_proxy != NULL)))
        return HL_STATUS_INVALID_ARGUMENT;
    for (uint32_t index = 0; index < box->publish_count; ++index)
        if (box->publish[index].host_port == 0 || box->publish[index].guest_port == 0)
            return HL_STATUS_INVALID_ARGUMENT;
    if (box->checkpoint_mode != 0) {
        number[0] = (char)('0' + box->checkpoint_policy);
        number[1] = 0;
        if (hl_options_set(&engine->options, "HL_CHECKPOINT_POLICY", number, 1) != HL_STATUS_OK ||
            ((box->checkpoint_mode & HL_CONFIG_CHECKPOINT_CAPTURE) &&
             hl_options_set(&engine->options, "HL_CHECKPOINT", "1", 1) != HL_STATUS_OK) ||
            ((box->checkpoint_mode & HL_CONFIG_CHECKPOINT_RESTORE) &&
             hl_options_set(&engine->options, "HL_RESTORE", "1", 1) != HL_STATUS_OK))
            return HL_STATUS_OUT_OF_MEMORY;
    }
    engine->owned_working_directory = hl_engine_copy_string(box->working_directory);
    engine->owned_hostname = hl_engine_copy_string(box->hostname);
    engine->owned_environment = hl_engine_copy_string(box->environment);
    if ((box->working_directory != NULL && engine->owned_working_directory == NULL) ||
        (box->hostname != NULL && engine->owned_hostname == NULL) ||
        (box->environment != NULL && engine->owned_environment == NULL))
        return HL_STATUS_OUT_OF_MEMORY;
    engine->box_config = *box;
    engine->box_config.size = sizeof(engine->box_config);
    engine->box_config.working_directory = engine->owned_working_directory;
    engine->box_config.hostname = engine->owned_hostname;
    engine->box_config.environment = engine->owned_environment;
    {
        size_t string_index = 0;
#define COPY_BOX_STRING(field, option)                                                                                 \
    engine->owned_box_strings[string_index] = hl_engine_copy_string(box->field);                                       \
    if (box->field != NULL && engine->owned_box_strings[string_index] == NULL) return HL_STATUS_OUT_OF_MEMORY;         \
    engine->box_config.field = engine->owned_box_strings[string_index++];
        HL_BOX_STRING_FIELDS(COPY_BOX_STRING)
#undef COPY_BOX_STRING
        if (box->publish_count != 0) {
            engine->owned_publish = malloc((size_t)box->publish_count * sizeof(*engine->owned_publish));
            if (engine->owned_publish == NULL) return HL_STATUS_OUT_OF_MEMORY;
            memcpy(engine->owned_publish, box->publish, (size_t)box->publish_count * sizeof(*engine->owned_publish));
            engine->box_config.publish = engine->owned_publish;
        }
    }
    engine->config.box = &engine->box_config;
    if (hl_engine_set_option(&engine->options, "HL_CWD", engine->owned_working_directory) != 0 ||
        hl_engine_set_option(&engine->options, "HL_HOSTNAME", engine->owned_hostname) != 0 ||
        hl_engine_set_option(&engine->options, "HL_GUEST_ENV", engine->owned_environment) != 0)
        return HL_STATUS_OUT_OF_MEMORY;
    if (box->uid >= 0) {
        snprintf(number, sizeof(number), "%d", box->uid);
        if (hl_options_set(&engine->options, "HL_UID", number, 1) != 0) return HL_STATUS_OUT_OF_MEMORY;
    }
    if (box->gid >= 0) {
        snprintf(number, sizeof(number), "%d", box->gid);
        if (hl_options_set(&engine->options, "HL_GID", number, 1) != 0) return HL_STATUS_OUT_OF_MEMORY;
    }
    if ((box->flags & HL_ENGINE_BOX_ROOTFS_READ_ONLY) != 0 &&
        hl_options_set(&engine->options, "HL_ROOTFS_RO", "1", 1) != 0)
        return HL_STATUS_OUT_OF_MEMORY;
    if ((box->flags & HL_ENGINE_BOX_SANDBOX) != 0 && (hl_options_set(&engine->options, "HL_SANDBOX", "1", 1) != 0 ||
                                                      hl_options_set(&engine->options, "HL_UNTRUSTED", "1", 1) != 0))
        return HL_STATUS_OUT_OF_MEMORY;
    if ((box->flags & HL_ENGINE_BOX_NETWORK_ISOLATED) != 0 &&
        hl_options_set(&engine->options, "HL_NET_ISOLATE", "1", 1) != 0)
        return HL_STATUS_OUT_OF_MEMORY;
    {
#define APPLY_BOX_STRING(field, option)                                                                                \
    if (hl_engine_set_option(&engine->options, option, engine->box_config.field) != 0) return HL_STATUS_OUT_OF_MEMORY;
        HL_BOX_STRING_FIELDS(APPLY_BOX_STRING)
#undef APPLY_BOX_STRING
        {
            size_t used = 0;
            uint32_t index;
            publish[0] = 0;
            for (index = 0; index < box->publish_count; ++index) {
                const uint8_t *address = (const uint8_t *)&box->publish[index].host_ipv4_be;
                int written =
                    box->publish[index].host_ipv4_be == 0
                        ? snprintf(publish + used, sizeof publish - used, "%s%u:%u", index ? "," : "",
                                   (unsigned)box->publish[index].host_port, (unsigned)box->publish[index].guest_port)
                        : snprintf(publish + used, sizeof publish - used, "%s%u.%u.%u.%u:%u:%u", index ? "," : "",
                                   (unsigned)address[0], (unsigned)address[1], (unsigned)address[2],
                                   (unsigned)address[3], (unsigned)box->publish[index].host_port,
                                   (unsigned)box->publish[index].guest_port);
                if (written < 0 || (size_t)written >= sizeof publish - used) return HL_STATUS_INVALID_ARGUMENT;
                used += (size_t)written;
            }
            if (box->publish_count != 0 && hl_engine_set_option(&engine->options, "HL_PUBLISH", publish) != 0)
                return HL_STATUS_OUT_OF_MEMORY;
        }
        if (box->translation_cache != NULL && hl_options_set(&engine->options, "HL_PCACHE", "1", 1) != 0)
            return HL_STATUS_OUT_OF_MEMORY;
        if ((box->flags & HL_ENGINE_BOX_TRANSLATION_CACHE_DISABLED) != 0 &&
            (hl_options_unset(&engine->options, "HL_PCACHE") != 0 ||
             hl_options_unset(&engine->options, "HL_PCACHE_DIR") != 0))
            return HL_STATUS_OUT_OF_MEMORY;
        if ((box->flags & HL_ENGINE_BOX_PUBLISH_EXTERNAL) != 0 &&
            hl_options_set(&engine->options, "HL_PUBLISH_DAEMON", "1", 1) != 0)
            return HL_STATUS_OUT_OF_MEMORY;
        if ((box->flags & HL_ENGINE_BOX_SENTRY_ONLY) != 0 &&
            hl_options_set(&engine->options, "HL_UNTRUSTED", "1", 1) != 0)
            return HL_STATUS_OUT_OF_MEMORY;
    }
    return HL_STATUS_OK;
}

hl_status hl_engine_create_with_options(const hl_engine_config *config, const hl_host_services *host,
                                        const hl_options *source_options, hl_engine **out_engine) {
    hl_engine *engine;
    hl_host_handle *candidate_handles = NULL;
    hl_status status;
    if (config == NULL || host == NULL || out_engine == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_engine = NULL;
    if (config->abi != HL_ENGINE_ABI || config->size < sizeof(*config)) return HL_STATUS_ABI_MISMATCH;
    if (config->guest_isa != HL_GUEST_ISA_AARCH64 && config->guest_isa != HL_GUEST_ISA_X86_64)
        return HL_STATUS_INVALID_ARGUMENT;
    if (config->flags != 0 || config->reserved != 0) return HL_STATUS_INVALID_ARGUMENT;
    if (config->payload_size != 0 && config->payload == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (config->payload_size != 0) return HL_STATUS_NOT_SUPPORTED;
    if (config->executable != NULL &&
        (config->executable->abi != HL_ENGINE_ABI || config->executable->size < sizeof(*config->executable)))
        return HL_STATUS_ABI_MISMATCH;
    if (config->executable != NULL &&
        (config->executable->reserved != 0 || config->executable->host_handle == HL_HOST_HANDLE_INVALID ||
         config->executable->image != NULL || config->executable->image_size != 0 ||
         (config->executable->ownership != HL_ENGINE_FD_TRANSFER &&
          config->executable->ownership != HL_ENGINE_FD_BORROW)))
        return HL_STATUS_INVALID_ARGUMENT;
    if (config->fd_binding_count != 0 && config->fd_bindings == NULL) return HL_STATUS_INVALID_ARGUMENT;
    status = hl_host_services_validate(host, HL_HOST_CAP_MEMORY | HL_HOST_CAP_CLOCK | HL_HOST_CAP_SYNC);
    if (status != HL_STATUS_OK) return status;
    engine = calloc(1, sizeof(*engine));
    if (engine == NULL) return HL_STATUS_OUT_OF_MEMORY;
    memcpy(&engine->config, config, sizeof(*config));
    memcpy(&engine->host, host, sizeof(*host));
    engine->executable = HL_HOST_HANDLE_INVALID;
    if (config->executable != NULL) {
        hl_host_result cloned;
        status = hl_host_services_validate(host, HL_HOST_CAP_FILE);
        if (status != HL_STATUS_OK) goto fail;
        if (host->file->clone_for_fork == NULL || host->file->close == NULL) {
            status = HL_STATUS_ABI_MISMATCH;
            goto fail;
        }
        cloned = host->file->clone_for_fork(host->context, config->executable->host_handle);
        if (cloned.status != HL_STATUS_OK || cloned.value == HL_HOST_HANDLE_INVALID) {
            status = cloned.status == HL_STATUS_OK ? HL_STATUS_PLATFORM_FAILURE : (hl_status)cloned.status;
            goto fail;
        }
        engine->executable = cloned.value;
        engine->executable_config = (hl_engine_executable){
            HL_ENGINE_ABI, sizeof(engine->executable_config), HL_ENGINE_FD_BORROW, 0, cloned.value, NULL, 0};
        status = hl_engine_read_executable(engine, cloned.value);
        if (status != HL_STATUS_OK) goto fail;
        (void)engine->host.file->close(engine->host.context, cloned.value);
        engine->executable = HL_HOST_HANDLE_INVALID;
        engine->executable_config.host_handle = HL_HOST_HANDLE_INVALID;
        engine->config.executable = &engine->executable_config;
    }
    if ((source_options == NULL ? hl_options_clone_current(&engine->options)
                                : hl_options_clone(&engine->options, source_options)) != 0) {
        status = HL_STATUS_OUT_OF_MEMORY;
        goto fail;
    }
    hl_options_import_environment(&engine->options);
    engine->options_initialized = 1;
    engine->owned_rootfs = hl_engine_copy_string(config->rootfs);
    if (config->rootfs != NULL && engine->owned_rootfs == NULL) {
        status = HL_STATUS_OUT_OF_MEMORY;
        goto fail;
    }
    engine->config.rootfs = engine->owned_rootfs;
    {
        char value[32];
        if (config->memory_limit != 0) {
            snprintf(value, sizeof(value), "%llu", (unsigned long long)config->memory_limit);
            if (hl_options_set(&engine->options, "HL_MEM_MAX", value, 1) != 0) goto option_fail;
        }
        if (config->pid_limit != 0) {
            snprintf(value, sizeof(value), "%u", config->pid_limit);
            if (hl_options_set(&engine->options, "HL_PIDS_MAX", value, 1) != 0) goto option_fail;
        }
        if (config->cpu_limit != 0) {
            snprintf(value, sizeof(value), "%u", config->cpu_limit);
            if (hl_options_set(&engine->options, "HL_CPUS", value, 1) != 0) goto option_fail;
        }
    }
    status = hl_engine_apply_box(engine, config->box);
    if (status != HL_STATUS_OK) goto fail;
    engine->box_fds = calloc(HL_LINUX_FD_LIMIT, sizeof(*engine->box_fds));
    engine->box_ofds = calloc(HL_LINUX_OFD_LIMIT, sizeof(*engine->box_ofds));
    if (engine->box_fds == NULL || engine->box_ofds == NULL) {
        status = HL_STATUS_OUT_OF_MEMORY;
        goto fail;
    }
    status = hl_linux_abi_init(&engine->box, &engine->host, engine->box_fds, HL_LINUX_FD_LIMIT, engine->box_ofds,
                               HL_LINUX_OFD_LIMIT);
    if (status != HL_STATUS_OK) goto fail;
    engine->box_initialized = 1;
    if (config->fd_binding_count != 0) {
        uint32_t index;
        status = hl_host_services_validate(host, HL_HOST_CAP_FILE);
        if (status != HL_STATUS_OK) goto fail;
        for (index = 0; index < config->fd_binding_count; ++index) {
            const hl_engine_fd_binding *binding = &config->fd_bindings[index];
            uint32_t previous;
            if (binding->abi != HL_ENGINE_ABI || binding->size < sizeof(*binding) ||
                binding->host_handle == HL_HOST_HANDLE_INVALID || binding->guest_fd >= HL_LINUX_FD_LIMIT ||
                (binding->ownership != HL_ENGINE_FD_TRANSFER && binding->ownership != HL_ENGINE_FD_BORROW)) {
                status = HL_STATUS_INVALID_ARGUMENT;
                goto fail;
            }
            for (previous = 0; previous < index; ++previous) {
                if (config->fd_bindings[previous].guest_fd == binding->guest_fd) {
                    status = HL_STATUS_INVALID_ARGUMENT;
                    goto fail;
                }
            }
        }
        candidate_handles = malloc(config->fd_binding_count * sizeof(*candidate_handles));
        if (candidate_handles == NULL) {
            status = HL_STATUS_OUT_OF_MEMORY;
            goto fail;
        }
        for (index = 0; index < config->fd_binding_count; ++index)
            candidate_handles[index] = HL_HOST_HANDLE_INVALID;
        for (index = 0; index < config->fd_binding_count; ++index) {
            const hl_engine_fd_binding *binding = &config->fd_bindings[index];
            hl_host_result cloned = engine->host.file->clone_for_fork(engine->host.context, binding->host_handle);
            if (cloned.status != HL_STATUS_OK || cloned.value == HL_HOST_HANDLE_INVALID) {
                status = cloned.status == HL_STATUS_OK ? HL_STATUS_PLATFORM_FAILURE : (hl_status)cloned.status;
                goto fail;
            }
            candidate_handles[index] = cloned.value;
            status = hl_linux_fd_install_at(&engine->box, binding->guest_fd, candidate_handles[index],
                                            binding->status_flags, binding->descriptor_flags);
            if (status != HL_STATUS_OK) goto fail;
            candidate_handles[index] = HL_HOST_HANDLE_INVALID;
        }
        for (index = 0; index < config->fd_binding_count; ++index) {
            const hl_engine_fd_binding *binding = &config->fd_bindings[index];
            if (binding->ownership == HL_ENGINE_FD_TRANSFER)
                (void)engine->host.file->close(engine->host.context, binding->host_handle);
        }
        engine->config.fd_bindings = NULL;
        engine->config.fd_binding_count = 0;
    }
    atomic_flag_clear(&engine->lock);
    engine->backend = production_backends[config->guest_isa];
    free(candidate_handles);
    *out_engine = engine;
    if (config->executable != NULL && config->executable->ownership == HL_ENGINE_FD_TRANSFER)
        (void)host->file->close(host->context, config->executable->host_handle);
    return HL_STATUS_OK;
option_fail:
    status = HL_STATUS_OUT_OF_MEMORY;
fail:
    if (candidate_handles != NULL && engine != NULL) {
        uint32_t index;
        for (index = 0; index < config->fd_binding_count; ++index) {
            if (candidate_handles[index] != HL_HOST_HANDLE_INVALID)
                (void)engine->host.file->close(engine->host.context, candidate_handles[index]);
        }
    }
    free(candidate_handles);
    if (engine != NULL) {
        uint32_t fd;
        if (engine->box_initialized) {
            for (fd = 0; fd < engine->box.fd_capacity; ++fd) {
                hl_host_handle handle;
                if (hl_linux_fd_close(&engine->box, fd, &handle) == HL_STATUS_OK && handle != HL_HOST_HANDLE_INVALID)
                    (void)engine->host.file->close(engine->host.context, handle);
            }
            (void)hl_linux_abi_destroy(&engine->box);
        }
        free(engine->box_fds);
        free(engine->box_ofds);
        if (engine->options_initialized) hl_options_destroy(&engine->options);
        free(engine->owned_rootfs);
        if (engine->owned_executable_image != NULL) {
            memset(engine->owned_executable_image, 0, engine->executable_config.image_size);
            free(engine->owned_executable_image);
        }
        if (engine->executable != HL_HOST_HANDLE_INVALID)
            (void)engine->host.file->close(engine->host.context, engine->executable);
        free(engine->owned_working_directory);
        free(engine->owned_hostname);
        free(engine->owned_environment);
        {
            size_t index;
            for (index = 0; index < sizeof(engine->owned_box_strings) / sizeof(engine->owned_box_strings[0]); ++index)
                free(engine->owned_box_strings[index]);
            free(engine->owned_publish);
        }
        free(engine);
    }
    return status;
}

hl_status hl_engine_create(const hl_engine_config *config, const hl_host_services *host, hl_engine **out_engine) {
    return hl_engine_create_with_options(config, host, NULL, out_engine);
}

hl_status hl_engine_run(hl_engine *engine, int argc, const char *const argv[], hl_engine_exit *out_exit) {
    hl_host_result waited;
    hl_host_result closed;
    hl_host_handle process = HL_HOST_HANDLE_INVALID;
    hl_host_handle process_result = HL_HOST_HANDLE_INVALID;
    uint32_t pending;
    hl_status status;
    if (engine == NULL || argc < 0 || (argc != 0 && argv == NULL) || out_exit == NULL)
        return HL_STATUS_INVALID_ARGUMENT;
    if (out_exit->abi != HL_ENGINE_ABI || out_exit->size < sizeof(*out_exit)) return HL_STATUS_ABI_MISMATCH;
    hl_engine_lock(engine);
    if (engine->state != HL_ENGINE_CREATED) {
        hl_engine_unlock(engine);
        return HL_STATUS_BUSY;
    }
    engine->state = HL_ENGINE_STARTING;
    hl_engine_unlock(engine);
    out_exit->kind = HL_ENGINE_EXIT_ENGINE_ERROR;
    out_exit->guest_status = HL_STATUS_NOT_SUPPORTED;
    out_exit->detail = engine->config.guest_isa;
    if (engine->backend == NULL || engine->backend->guest_isa != engine->config.guest_isa ||
        engine->backend->start_process == NULL) {
        hl_engine_lock(engine);
        engine->state = HL_ENGINE_FINISHED;
        hl_engine_unlock(engine);
        return HL_STATUS_NOT_SUPPORTED;
    }
    status =
        engine->backend->start_process(&engine->host, engine->box_initialized ? &engine->box : NULL, &engine->options,
                                       &engine->config, (uint32_t)argc, argv, &process, &process_result);
    if (status != HL_STATUS_OK) {
        hl_engine_lock(engine);
        engine->state = HL_ENGINE_FINISHED;
        hl_engine_unlock(engine);
        return status;
    }
    hl_engine_lock(engine);
    engine->process = process;
    if (engine->state != HL_ENGINE_DESTROYING) engine->state = HL_ENGINE_RUNNING;
    pending = engine->pending_termination;
    hl_engine_unlock(engine);
    if (pending != 0) engine->host.process->terminate(engine->host.context, process, pending);
    waited = engine->host.process->wait(engine->host.context, process, HL_HOST_DEADLINE_INFINITE);
    hl_engine_lock(engine);
    engine->process = HL_HOST_HANDLE_INVALID;
    hl_engine_unlock(engine);
    closed = engine->host.process->close(engine->host.context, process);
    if (waited.status != HL_STATUS_OK) {
        status = (hl_status)waited.status;
    } else if (closed.status != HL_STATUS_OK) {
        status = (hl_status)closed.status;
    } else if (engine->backend->finish_process != NULL && process_result != HL_HOST_HANDLE_INVALID) {
        status = engine->backend->finish_process(&engine->host, process_result, &waited, out_exit);
        process_result = HL_HOST_HANDLE_INVALID;
    } else {
        out_exit->detail = 0;
        if (waited.detail == HL_HOST_PROCESS_EXIT_CODE) {
            out_exit->kind = HL_ENGINE_EXIT_CODE;
            out_exit->guest_status = (int32_t)waited.value;
            status = HL_STATUS_OK;
        } else if (waited.detail == HL_HOST_PROCESS_EXIT_SIGNAL) {
            out_exit->kind = HL_ENGINE_EXIT_SIGNAL;
            out_exit->guest_status = (int32_t)waited.value;
            status = HL_STATUS_OK;
        } else {
            out_exit->guest_status = HL_STATUS_CORRUPT;
            status = HL_STATUS_CORRUPT;
        }
    }
    if (process_result != HL_HOST_HANDLE_INVALID && engine->backend->release_process_result != NULL)
        engine->backend->release_process_result(&engine->host, process_result);
    hl_engine_lock(engine);
    engine->state = HL_ENGINE_FINISHED;
    hl_engine_unlock(engine);
    return status;
}

hl_status hl_engine_request(hl_engine *engine, uint32_t request, const void *data, size_t data_size) {
    uint32_t reason;
    hl_host_handle process;
    hl_status status;
    if (engine == NULL || (data_size != 0 && data == NULL)) return HL_STATUS_INVALID_ARGUMENT;
    if (request == HL_ENGINE_REQUEST_SIGNAL) {
        uint32_t signal_number;
        if (data == NULL || data_size != sizeof(signal_number)) return HL_STATUS_INVALID_ARGUMENT;
        memcpy(&signal_number, data, sizeof(signal_number));
        if (signal_number == 0 || signal_number > 64 || signal_number == 9 || signal_number == 19)
            return HL_STATUS_INVALID_ARGUMENT;
        reason = HL_HOST_PROCESS_TERMINATE_SIGNAL + signal_number;
    } else if (data_size != 0) {
        return HL_STATUS_INVALID_ARGUMENT;
    } else if (request == HL_ENGINE_REQUEST_INTERRUPT)
        reason = HL_HOST_PROCESS_TERMINATE_INTERRUPT;
    else if (request == HL_ENGINE_REQUEST_FORCE_STOP)
        reason = HL_HOST_PROCESS_TERMINATE_FORCE;
    else
        return HL_STATUS_NOT_SUPPORTED;
    hl_engine_lock(engine);
    if (engine->state == HL_ENGINE_FINISHED || engine->state == HL_ENGINE_DESTROYING) {
        hl_engine_unlock(engine);
        return HL_STATUS_BUSY;
    }
    /* HL_ENGINE_CREATED is accepted, not rejected: a caller that signals between
     * create and run must not have that signal silently discarded.  hl_engine_run
     * replays pending_termination once the guest process exists. */
    engine->pending_termination = reason;
    process = engine->process;
    hl_engine_unlock(engine);
    if (process == HL_HOST_HANDLE_INVALID) return HL_STATUS_OK;
    status = (hl_status)engine->host.process->terminate(engine->host.context, process, reason).status;
    if (status == HL_STATUS_INVALID_ARGUMENT) {
        hl_engine_lock(engine);
        if (engine->state == HL_ENGINE_FINISHED) status = HL_STATUS_BUSY;
        hl_engine_unlock(engine);
    }
    return status;
}

void hl_engine_destroy(hl_engine *engine) {
    hl_host_handle process;
    uint32_t fd;
    if (engine == NULL) return;
    hl_engine_lock(engine);
    if (engine->state == HL_ENGINE_STARTING || engine->state == HL_ENGINE_RUNNING) {
        engine->state = HL_ENGINE_DESTROYING;
        engine->pending_termination = HL_HOST_PROCESS_TERMINATE_FORCE;
        process = engine->process;
        hl_engine_unlock(engine);
        if (process != HL_HOST_HANDLE_INVALID)
            (void)engine->host.process->terminate(engine->host.context, process, HL_HOST_PROCESS_TERMINATE_FORCE);
        for (;;) {
            uint32_t state;
            hl_engine_lock(engine);
            state = engine->state;
            hl_engine_unlock(engine);
            if (state == HL_ENGINE_FINISHED) break;
            hl_engine_yield(engine);
        }
    } else {
        engine->state = HL_ENGINE_DESTROYING;
        hl_engine_unlock(engine);
    }
    if (engine->box_initialized) {
        for (fd = 0; fd < engine->box.fd_capacity; ++fd)
            (void)hl_linux_close(&engine->box, fd);
        (void)hl_linux_abi_destroy(&engine->box);
    }
    free(engine->box_fds);
    free(engine->box_ofds);
    if (engine->options_initialized) hl_options_destroy(&engine->options);
    free(engine->owned_rootfs);
    if (engine->owned_executable_image != NULL) {
        memset(engine->owned_executable_image, 0, engine->executable_config.image_size);
        free(engine->owned_executable_image);
    }
    if (engine->executable != HL_HOST_HANDLE_INVALID)
        (void)engine->host.file->close(engine->host.context, engine->executable);
    free(engine->owned_working_directory);
    free(engine->owned_hostname);
    free(engine->owned_environment);
    {
        size_t index;
        for (index = 0; index < sizeof(engine->owned_box_strings) / sizeof(engine->owned_box_strings[0]); ++index)
            free(engine->owned_box_strings[index]);
        free(engine->owned_publish);
    }
    free(engine);
}
