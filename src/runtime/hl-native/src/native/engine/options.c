// Authoritative engine option registry and instance-owned value store.
#include "options.h"

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct hl_option_definition {
    const char *name;
    const char *purpose;
    uint8_t ownership;
    uint8_t shape;
} hl_option_definition;

enum hl_option_ownership { HL_OPTION_LAUNCH_INPUT = 1, HL_OPTION_INTERNAL_STATE = 2, HL_OPTION_DEBUG_ONLY = 3 };

enum hl_option_shape {
    HL_OPTION_TEXT = 1,
    HL_OPTION_PATH = 2,
    HL_OPTION_INTEGER = 3,
    HL_OPTION_FLAG = 4,
    HL_OPTION_RECORDS = 5
};

#define HL_LAUNCH_OPTION(name, purpose, shape) {name, purpose, HL_OPTION_LAUNCH_INPUT, shape}
#define HL_INTERNAL_OPTION(name, purpose, shape) {name, purpose, HL_OPTION_INTERNAL_STATE, shape}
#define HL_DEBUG_OPTION(name, purpose, shape) {name, purpose, HL_OPTION_DEBUG_ONLY, shape}

enum { HL_OPTION_STORE_LIMIT = 64 * 1024 * 1024 };

static const hl_option_definition hl_option_definitions[] = {
    HL_LAUNCH_OPTION("HL_CHECKPOINT", "arm checkpoint capture over the store channel", HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_CHECKPOINT_COORDINATOR",
                       "this launch owns the domain freeze: exactly one engine per checkpoint broker", HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_CHECKPOINT_PHASE_LEDGER", "emit checkpoint phase timing records for performance gates",
                       HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_DIAGNOSTIC_PORT", "private engine diagnostic writer descriptor", HL_OPTION_INTEGER),
    HL_INTERNAL_OPTION("HL_CHECKPOINT_PHASE_CLOCK_FAIL", "inject an unavailable checkpoint phase clock",
                       HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_CHECKPOINT_PHASE_ISA", "checkpoint phase ledger guest ISA", HL_OPTION_TEXT),
    HL_INTERNAL_OPTION("HL_CHECKPOINT_PHASE_GENERATION", "checkpoint restore phase ledger generation",
                       HL_OPTION_INTEGER),
    HL_INTERNAL_OPTION("HL_CKPT_TEST_PEER_EXIT_BEFORE_JOIN",
                       "test-only capture peer that exits at its safepoint before proving membership",
                       HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_CKPT_TEST_PEER_EXIT_AFTER_JOIN",
                       "test-only capture peer that exits after proving membership and before committing",
                       HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_CKPT_TEST_FAIL_AFTER_FORK", "test-only restore failure after rebuilding descendants",
                       HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_CKPT_TEST_FAIL_TRIGGER_REATTACH",
                       "test-only restored checkpoint trigger reattachment failure", HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_CKPT_TEST_FAIL_TTY_MASK", "test-only terminal-claim mask failure", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_CHECKPOINT_POLICY", "checkpoint incompatible-resource recovery policy", HL_OPTION_INTEGER),
    HL_LAUNCH_OPTION("HL_CPUS", "guest-visible CPU quota", HL_OPTION_INTEGER),
    HL_LAUNCH_OPTION("HL_C_DIAGNOSTICS", "report retained C translation and dispatch phase counters at launch exit",
                     HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_CWD", "initial guest working directory", HL_OPTION_PATH),
    HL_LAUNCH_OPTION("HL_EGRESS_SOCKS", "SOCKS5 endpoint for external TCP egress", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_FSGEN_FILE", "shared overlay filesystem-generation file", HL_OPTION_PATH),
    HL_LAUNCH_OPTION("HL_FILE_OWNERS", "initial guest file ownership records", HL_OPTION_RECORDS),
    HL_LAUNCH_OPTION("HL_GID", "initial guest group identity", HL_OPTION_INTEGER),
    HL_LAUNCH_OPTION("HL_GUEST_ENV", "serialized Linux guest environment", HL_OPTION_RECORDS),
    HL_LAUNCH_OPTION("HL_HOSTNAME", "Linux guest hostname", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_IP", "guest virtual IPv4 address paired with HL_NETBR", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_LOWER", "ordered root filesystem lower layers", HL_OPTION_RECORDS),
    HL_LAUNCH_OPTION("HL_OVERLAY_UPPER", "writable root filesystem overlay layer", HL_OPTION_PATH),
    HL_LAUNCH_OPTION("HL_OVERLAY_WORK", "launch-private portable overlay work directory", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_MEM_MAX", "guest memory limit", HL_OPTION_INTEGER),
    HL_LAUNCH_OPTION("HL_NETBR", "shared virtual-network bridge identity", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_NETIFS", "serialized virtual-network interfaces", HL_OPTION_RECORDS),
    HL_LAUNCH_OPTION("HL_NETNS", "guest network and IPC namespace identity", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_NET_ISOLATE", "disable guest external networking", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_NET_HOST", "use the host network stack directly", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_PCACHE", "enable persistent translated-code caching", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_PCACHE_DIR", "persistent translated-code cache storage", HL_OPTION_PATH),
    HL_LAUNCH_OPTION("HL_PIDS_MAX", "guest process limit", HL_OPTION_INTEGER),
    HL_LAUNCH_OPTION("HL_PROCESS_DOMAIN", "opaque launch process ownership identity", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_LAUNCH_DOMAIN", "activation-private process tree identity", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_PUBLISH", "guest-to-host port publication rules", HL_OPTION_RECORDS),
    HL_LAUNCH_OPTION("HL_PUBLISH_DAEMON", "host daemon publishes guest ports", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_RESTORE", "restore the image held by the store channel", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_ROOTFS_RO", "mount the guest root filesystem read-only", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_SANDBOX", "apply host confinement to the untrusted worker", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_SECCOMP_BASELINE", "guest-visible launch seccomp baseline", HL_OPTION_TEXT),
    HL_LAUNCH_OPTION("HL_UID", "initial guest user identity", HL_OPTION_INTEGER),
    HL_LAUNCH_OPTION("HL_ULIMITS", "serialized Linux resource limits", HL_OPTION_RECORDS),
    HL_LAUNCH_OPTION("HL_UNTRUSTED", "route host-authority operations through the sentry", HL_OPTION_FLAG),
    HL_LAUNCH_OPTION("HL_VOLUMES", "guest volume mount specification", HL_OPTION_RECORDS),
    HL_LAUNCH_OPTION("HL_NAME_BINDS", "live guest basename projection rules", HL_OPTION_RECORDS),
    HL_INTERNAL_OPTION("HL_GUEST_ENV_ESC", "guest environment uses escaped record encoding", HL_OPTION_FLAG),
    HL_INTERNAL_OPTION("HL_GUEST_ENV_EXACT", "guest exec environment suppresses engine defaults", HL_OPTION_FLAG),
    HL_DEBUG_OPTION("HL_LOG", "debug-build logging tag selector", HL_OPTION_TEXT),
    HL_DEBUG_OPTION("HL_FATAL_DIAGNOSTICS", "fatal guest register publication", HL_OPTION_FLAG),
};

#define HL_OPTION_COUNT (sizeof hl_option_definitions / sizeof hl_option_definitions[0])

static _Thread_local hl_options *hl_bound_options;
static hl_options *hl_process_options;
static hl_options *hl_process_state;
static _Thread_local hl_options hl_default_options;
static _Thread_local int hl_default_options_ready;

void hl_options_import_environment(hl_options *options) {
    const char *fatal = getenv("HL_FATAL_DIAGNOSTICS");
    if (fatal != NULL) (void)hl_options_set(options, "HL_FATAL_DIAGNOSTICS", fatal, 0);
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    const char *selector = getenv("HL_LOG");
    if (selector != NULL) (void)hl_options_set(options, "HL_LOG", selector, 0);
#else
    (void)options;
#endif
}

static size_t hl_option_index(const char *name) {
    size_t index;
    if (name != NULL)
        for (index = 0; index < HL_OPTION_COUNT; ++index)
            if (strcmp(name, hl_option_definitions[index].name) == 0) return index;
    return HL_OPTION_COUNT;
}

static size_t hl_option_value_size(const char *value);

int hl_options_init(hl_options *options) {
    if (options == NULL) return -1;
    memset(options, 0, sizeof(*options));
    options->values = (char **)calloc(HL_OPTION_COUNT, sizeof(*options->values));
    options->value_sizes = (size_t *)calloc(HL_OPTION_COUNT, sizeof(*options->value_sizes));
    if (options->values == NULL || options->value_sizes == NULL) {
        free(options->values);
        free(options->value_sizes);
        memset(options, 0, sizeof(*options));
        return -1;
    }
    options->value_count = HL_OPTION_COUNT;
    return 0;
}

int hl_options_init_records(hl_options *options, size_t count, const char *const *names, const char *const *values) {
    size_t record;
    if (options == NULL || (count != 0 && (names == NULL || values == NULL)) || count > HL_OPTION_COUNT) return -1;
    if (hl_options_init(options) != 0) return -1;
    for (record = 0; record < count; ++record) {
        size_t index = hl_option_index(names[record]);
        size_t value_size;
        char *copy;
        if (index >= HL_OPTION_COUNT || values[record] == NULL || options->values[index] != NULL) goto fail;
        value_size = hl_option_value_size(values[record]);
        if (value_size == 0 || options->store_size > HL_OPTION_STORE_LIMIT - value_size) goto fail;
        copy = malloc(value_size);
        if (copy == NULL) goto fail;
        memcpy(copy, values[record], value_size);
        options->values[index] = copy;
        options->value_sizes[index] = value_size;
        options->store_size += value_size;
    }
    return 0;
fail:
    hl_options_destroy(options);
    return -1;
}

int hl_options_clone(hl_options *destination, const hl_options *source) {
    size_t index;
    if (destination == NULL || source == NULL || source->value_count != HL_OPTION_COUNT) return -1;
    if (hl_options_init(destination) != 0) return -1;
    for (index = 0; index < source->value_count; ++index) {
        size_t size = source->value_sizes[index];
        if (size == 0) continue;
        if (size > HL_OPTION_STORE_LIMIT || source->values[index] == NULL || source->values[index][size - 1] != 0 ||
            source->store_size > HL_OPTION_STORE_LIMIT || destination->store_size > HL_OPTION_STORE_LIMIT - size) {
            hl_options_destroy(destination);
            return -1;
        }
        destination->values[index] = malloc(size);
        if (destination->values[index] == NULL) {
            hl_options_destroy(destination);
            return -1;
        }
        memcpy(destination->values[index], source->values[index], size);
        destination->value_sizes[index] = size;
        destination->store_size += size;
    }
    return 0;
}

int hl_options_validate(const hl_options *options) {
    size_t index, total = 0;
    if (options == NULL || options->values == NULL || options->value_sizes == NULL ||
        options->value_count != HL_OPTION_COUNT || options->store_size > HL_OPTION_STORE_LIMIT)
        return -1;
    for (index = 0; index < options->value_count; ++index) {
        size_t size = options->value_sizes[index];
        if (size == 0) {
            if (options->values[index] != NULL) return -1;
            continue;
        }
        if (size > HL_OPTION_STORE_LIMIT || options->values[index] == NULL || options->values[index][size - 1] != 0 ||
            total > HL_OPTION_STORE_LIMIT - size)
            return -1;
        total += size;
    }
    return total == options->store_size ? 0 : -1;
}

void hl_options_destroy(hl_options *options) {
    size_t index;
    if (options == NULL) return;
    if (options->values != NULL)
        for (index = 0; index < options->value_count; ++index)
            free(options->values[index]);
    free(options->values);
    free(options->value_sizes);
    memset(options, 0, sizeof(*options));
}

const char *hl_options_get(const hl_options *options, const char *name) {
    size_t index = hl_option_index(name);
    if (options == NULL || options->values == NULL || index >= options->value_count) return NULL;
    return options->values[index];
}

static size_t hl_option_value_size(const char *value) {
    size_t length;
    for (length = 0; length < HL_OPTION_STORE_LIMIT; ++length)
        if (value[length] == 0) return length + 1;
    return 0;
}

int hl_options_set(hl_options *options, const char *name, const char *value, int overwrite) {
    size_t index = hl_option_index(name), value_size;
    char *copy;
    if (options == NULL || options->values == NULL || index >= options->value_count || value == NULL) return -1;
    if (!overwrite && options->values[index] != NULL) return 0;
    value_size = hl_option_value_size(value);
    if (value_size == 0 || options->store_size - options->value_sizes[index] > HL_OPTION_STORE_LIMIT - value_size)
        return -1;
    copy = (char *)malloc(value_size);
    if (copy == NULL) return -1;
    memcpy(copy, value, value_size);
    free(options->values[index]);
    options->values[index] = copy;
    options->store_size = options->store_size - options->value_sizes[index] + value_size;
    options->value_sizes[index] = value_size;
    return 0;
}

int hl_options_unset(hl_options *options, const char *name) {
    size_t index = hl_option_index(name);
    if (options == NULL || options->values == NULL || index >= options->value_count) return -1;
    free(options->values[index]);
    options->values[index] = NULL;
    options->store_size -= options->value_sizes[index];
    options->value_sizes[index] = 0;
    return 0;
}

hl_options *hl_options_bind(hl_options *options) {
    hl_options *previous = hl_bound_options;
    hl_bound_options = options;
    return previous;
}

hl_options *hl_options_bind_process(hl_options *options) {
    hl_options *previous = hl_process_options;
    hl_process_options = options;
    return previous;
}

hl_options *hl_options_bind_process_state(hl_options *options) {
    hl_options *previous = hl_process_state;
    hl_process_state = options;
    return previous;
}

static hl_options *hl_options_current(void) {
    if (hl_bound_options != NULL) return hl_bound_options;
    if (hl_process_options != NULL) return hl_process_options;
    if (!hl_default_options_ready) {
        if (hl_options_init(&hl_default_options) != 0) return NULL;
        hl_default_options_ready = 1;
        hl_options_import_environment(&hl_default_options);
    }
    return &hl_default_options;
}

int hl_options_clone_current(hl_options *destination) {
    hl_options *current = hl_options_current();
    return current == NULL ? -1 : hl_options_clone(destination, current);
}

const char *hl_option_get(const char *name) {
    return hl_options_get(hl_options_current(), name);
}

int hl_option_set(const char *name, const char *value, int overwrite) {
    return hl_options_set(hl_options_current(), name, value, overwrite);
}

int hl_option_unset(const char *name) {
    return hl_options_unset(hl_options_current(), name);
}

const char *hl_process_guest_environment_get(void) {
    const char *value = hl_options_get(hl_process_state, "HL_GUEST_ENV");
    return value != NULL ? value : hl_option_get("HL_GUEST_ENV");
}

int hl_process_guest_environment_set(const char *value) {
    hl_options *options = hl_process_state != NULL ? hl_process_state : hl_options_current();
    if (options == NULL) return -1;
    if (options->values == NULL && hl_options_init(options) != 0) return -1;
    return hl_options_set(options, "HL_GUEST_ENV", value, 1);
}

int hl_process_guest_environment_unset(void) {
    hl_options *options = hl_process_state != NULL ? hl_process_state : hl_options_current();
    if (options == NULL || options->values == NULL) return 0;
    return hl_options_unset(options, "HL_GUEST_ENV");
}

static int hl_options_clone_or_init(hl_options *destination, const hl_options *source) {
    if (source != NULL && source->values != NULL) return hl_options_clone(destination, source);
    return hl_options_init(destination);
}

void hl_exec_environment_discard(hl_exec_environment_update *update) {
    if (update == NULL) return;
    hl_options_destroy(&update->process);
    hl_options_destroy(&update->state);
    memset(update, 0, sizeof(*update));
}

int hl_exec_environment_prepare(hl_exec_environment_update *update, const char *serialized) {
    hl_options *current;
    if (update == NULL || serialized == NULL) return -1;
    memset(update, 0, sizeof(*update));
    current = hl_options_current();
    if (current == NULL || hl_options_clone_or_init(&update->process, current) != 0) goto fail;
    update->process_target = current;
    update->state_target = hl_process_state != NULL ? hl_process_state : current;
    update->separate_state = update->state_target != update->process_target;
    if (update->separate_state && hl_options_clone_or_init(&update->state, update->state_target) != 0) goto fail;
    if (hl_options_set(update->separate_state ? &update->state : &update->process, "HL_GUEST_ENV", serialized, 1) !=
            0 ||
        hl_options_set(&update->process, "HL_GUEST_ENV_ESC", "1", 1) != 0 ||
        hl_options_set(&update->process, "HL_GUEST_ENV_EXACT", "1", 1) != 0)
        goto fail;
    update->prepared = 1;
    return 0;
fail:
    hl_exec_environment_discard(update);
    return -1;
}

static void hl_options_swap(hl_options *left, hl_options *right) {
    hl_options temporary = *left;
    *left = *right;
    *right = temporary;
}

void hl_exec_environment_commit(hl_exec_environment_update *update) {
    if (update == NULL || !update->prepared) return;
    hl_options_swap(update->process_target, &update->process);
    if (update->separate_state) hl_options_swap(update->state_target, &update->state);
    update->prepared = 0;
    hl_exec_environment_discard(update);
}

void hl_option_reset(void) {
    hl_options *options = hl_options_current();
    if (options == NULL) return;
    hl_options_destroy(options);
    (void)hl_options_init(options);
    hl_options_import_environment(options);
}
