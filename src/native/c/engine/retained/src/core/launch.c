// Production launch bridge. Wire parsing is owned by the portable hl config library. This file maps the
// validated launch model onto an owned option store scoped to this launch.
//
// A host launcher serializes the container into the position-independent hl_launch_config wire buffer
// and spawns the architecture-matching Linux engine as `<engine> --configfile <path>`. This is the engine
// side: open, unlink, read, and validate the buffer, then translate
// every populated field into the launch-owned option store, rebuilds the guest argv, and enters
// the Linux guest engine.
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "../../include/hl/config.h"
#include "launch.h"
#include "options.h"

/* Portable core/unit links intentionally do not include a production host implementation. Production engines
 * resolve these hooks from host/private.c; weak lookup keeps the config reader portable without sacrificing
 * private-descriptor registration when that implementation is present. */
extern void hl_host_private_init(void) __attribute__((weak));
extern int hl_host_process_fd_private_add(int descriptor) __attribute__((weak));
extern void hl_host_process_fd_private_remove(int descriptor) __attribute__((weak));
/* Distinguishes "this host has no private band" (negative) from "it has one and
 * the add still failed", which is the difference between a tolerable refusal and
 * a real error at the registration site below. */
extern int hl_host_process_fd_private_floor(void) __attribute__((weak));

// Read exactly `n` bytes from `fd` into `buf`, looping over short reads. Returns 0 on success, -1 on
// EOF/error -- a truncated buffer is a hard failure (a partial config must never launch a container).
static int cfd_read_full(int fd, void *buf, size_t n) {
    uint8_t *p = (uint8_t *)buf;
    size_t got = 0;
    while (got < n) {
        ssize_t r = read(fd, p + got, n - got);
        if (r < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (r == 0) { return -1; } // premature EOF
        got += (size_t)r;
    }
    return 0;
}

static const char *launch_string(const hl_launch_config *config, const char *pool, uint32_t offset) {
    const char *value = "";
    if (offset != 0 && hl_launch_config_string(config, pool, offset, &value, NULL) != HL_STATUS_OK) return NULL;
    return value;
}

static int launch_strings_valid(const hl_launch_config *config, const char *pool) {
    const uint32_t offsets[] = {
        config->rootfs_offset,
        config->lower_layers_offset,
        config->overlay_work_offset,
        config->hostname_offset,
        config->network_namespace_offset,
        config->volumes_offset,
        config->limits_offset,
        config->working_directory_offset,
        config->environment_offset,
        config->translation_cache_offset,
        config->network_bridge_offset,
        config->ip_offset,
        config->filesystem_generation_offset,
        config->egress_proxy_offset,
        config->debug_log_offset,
        config->result_path_offset,
        config->network_interfaces_offset,
        config->file_owners_offset,
        config->executable_host_offset,
        config->name_binds_offset,
    };
    size_t i;
    for (i = 0; i < sizeof offsets / sizeof offsets[0]; i++) {
        if (offsets[i] != 0 && hl_launch_config_string(config, pool, offsets[i], NULL, NULL) != HL_STATUS_OK) return 0;
    }
    return 1;
}

static int launch_publish(const hl_launch_config *config, const char *pool, char *output, size_t capacity) {
    const hl_engine_publish_rule *rules;
    size_t used = 0;
    uint32_t index;
    if (config->publish_count == 0) {
        output[0] = 0;
        return 0;
    }
    if (hl_launch_config_publish(config, pool, &rules) != HL_STATUS_OK) return -1;
    for (index = 0; index < config->publish_count; ++index) {
        const uint8_t *address = (const uint8_t *)&rules[index].host_ipv4_be;
        int written =
            rules[index].host_ipv4_be == 0
                ? snprintf(output + used, capacity - used, "%s%u:%u", index ? "," : "",
                           (unsigned)rules[index].host_port, (unsigned)rules[index].guest_port)
                : snprintf(output + used, capacity - used, "%s%u.%u.%u.%u:%u:%u", index ? "," : "",
                           (unsigned)address[0], (unsigned)address[1], (unsigned)address[2], (unsigned)address[3],
                           (unsigned)rules[index].host_port, (unsigned)rules[index].guest_port);
        if (written < 0 || (size_t)written >= capacity - used) return -1;
        used += (size_t)written;
    }
    return 0;
}

static int launch_lowers(const hl_launch_config *config, const char *pool, char *output, size_t capacity) {
    size_t used = 0;
    uint32_t offset = config->lower_layers_offset;
    // Validation ties a zero count to a zero offset, so there is nothing to emit.
    if (config->lower_layer_count == 0) {
        output[0] = 0;
        return 0;
    }
    for (uint32_t index = 0; index < config->lower_layer_count; index++) {
        const char *path = pool + offset;
        size_t length = strlen(path);
        if (used + length + (index != 0) + 1 > capacity) return -1;
        if (index != 0) output[used++] = '\n';
        memcpy(output + used, path, length);
        used += length;
        output[used] = 0;
        offset += (uint32_t)length + 1;
    }
    return 0;
}

// Decode an open launch config and invoke the selected runtime.
static int hl_read_config_file(int fd, hl_launch_runner runner) {
    enum { HL_LAUNCH_HEADER_LIMIT = 4096, HL_LAUNCH_POOL_LIMIT = 64 * 1024 * 1024 };

    hl_launch_config cfg;
    uint8_t prefix[16];
    uint8_t *wire = NULL;
    const char *pool = NULL;
    uint32_t magic;
    uint32_t pool_size;
    uint32_t header_size;
    uint32_t abi;
    size_t wire_size;

    if (cfd_read_full(fd, prefix, sizeof prefix) != 0) return 78;
    memcpy(&magic, prefix + 0, 4);
    memcpy(&pool_size, prefix + 4, 4);
    memcpy(&header_size, prefix + 8, 4);
    memcpy(&abi, prefix + 12, 4);
    if (magic != HL_CONFIG_MAGIC || abi != HL_CONFIG_ABI || header_size < sizeof(hl_launch_config) ||
        header_size > HL_LAUNCH_HEADER_LIMIT || pool_size == 0 || pool_size > HL_LAUNCH_POOL_LIMIT)
        return 78;
    wire_size = (size_t)header_size + (size_t)pool_size;
    if (wire_size < header_size) return 78;
    wire = (uint8_t *)malloc(wire_size);
    if (wire == NULL) return 78;
    memcpy(wire, prefix, sizeof prefix);
    if (cfd_read_full(fd, wire + sizeof prefix, wire_size - sizeof prefix) != 0) {
        free(wire);
        return 78;
    }
    if (hl_launch_config_validate(wire, wire_size, &cfg, &pool) != HL_STATUS_OK || !launch_strings_valid(&cfg, pool) ||
        (cfg.publish_count != 0 && hl_launch_config_publish(&cfg, pool, NULL) != HL_STATUS_OK) ||
        hl_launch_config_arguments_validate(&cfg, pool, NULL) != HL_STATUS_OK) {
        free(wire);
        return 78;
    }

    hl_options options;
    char num[32];
    char publish[1024];
    char lowers[8192];
    char process_domain[33];
    const char *s;
    if (hl_options_init(&options) != 0) {
        free(wire);
        return 78;
    }
#define APPLY_OPTION(name, value)                                                                                      \
    do {                                                                                                               \
        if (hl_options_set(&options, (name), (value), 1) != 0) goto option_failure;                                    \
    } while (0)

    // Scalars populate the same process-local values container initialization reads.
    if (cfg.memory_limit) {
        snprintf(num, sizeof num, "%llu", (unsigned long long)cfg.memory_limit);
        APPLY_OPTION("HL_MEM_MAX", num);
    }
    if (cfg.pid_limit) {
        snprintf(num, sizeof num, "%u", cfg.pid_limit);
        APPLY_OPTION("HL_PIDS_MAX", num);
    }
    if (cfg.cpu_limit) {
        snprintf(num, sizeof num, "%u", cfg.cpu_limit);
        APPLY_OPTION("HL_CPUS", num);
    }
    if (cfg.rootfs_read_only) APPLY_OPTION("HL_ROOTFS_RO", "1");
    if (cfg.network_isolated) APPLY_OPTION("HL_NET_ISOLATE", "1");
    if (cfg.network_transport == HL_CONFIG_NETWORK_HOST) APPLY_OPTION("HL_NET_HOST", "1");
    if (cfg.publish_external) APPLY_OPTION("HL_PUBLISH_DAEMON", "1");
    if (cfg.uid >= 0) {
        snprintf(num, sizeof num, "%d", cfg.uid);
        APPLY_OPTION("HL_UID", num);
    }
    if (cfg.gid >= 0) {
        snprintf(num, sizeof num, "%d", cfg.gid);
        APPLY_OPTION("HL_GID", num);
    }
    snprintf(process_domain, sizeof process_domain, "%016llx%016llx", (unsigned long long)cfg.process_domain[0],
             (unsigned long long)cfg.process_domain[1]);
    APPLY_OPTION("HL_PROCESS_DOMAIN", process_domain);

    // Pooled strings are copied by hl_option_set; an empty field leaves the option unset.
    s = launch_string(&cfg, pool, cfg.hostname_offset);
    if (s[0]) APPLY_OPTION("HL_HOSTNAME", s);
    s = launch_string(&cfg, pool, cfg.limits_offset);
    if (s[0]) APPLY_OPTION("HL_ULIMITS", s);
    if (launch_publish(&cfg, pool, publish, sizeof publish) != 0) goto option_failure;
    if (publish[0]) APPLY_OPTION("HL_PUBLISH", publish);
    if (launch_lowers(&cfg, pool, lowers, sizeof lowers) != 0) goto option_failure;
    if (lowers[0]) APPLY_OPTION("HL_LOWER", lowers);
    s = launch_string(&cfg, pool, cfg.overlay_work_offset);
    if (s[0]) APPLY_OPTION("HL_OVERLAY_WORK", s);
    s = launch_string(&cfg, pool, cfg.network_namespace_offset);
    if (cfg.network_transport != HL_CONFIG_NETWORK_HOST) APPLY_OPTION("HL_NETNS", s[0] ? s : process_domain);
    s = launch_string(&cfg, pool, cfg.volumes_offset);
    if (s[0]) APPLY_OPTION("HL_VOLUMES", s);
    s = launch_string(&cfg, pool, cfg.name_binds_offset);
    if (s[0]) APPLY_OPTION("HL_NAME_BINDS", s);
    s = launch_string(&cfg, pool, cfg.working_directory_offset);
    if (s[0]) APPLY_OPTION("HL_CWD", s);
    s = launch_string(&cfg, pool, cfg.environment_offset);
    if (s[0]) APPLY_OPTION("HL_GUEST_ENV", s);
    s = launch_string(&cfg, pool, cfg.network_bridge_offset);
    if (s[0]) APPLY_OPTION("HL_NETBR", s);
    s = launch_string(&cfg, pool, cfg.ip_offset);
    if (s[0]) APPLY_OPTION("HL_IP", s);
    s = launch_string(&cfg, pool, cfg.network_interfaces_offset);
    if (s[0]) APPLY_OPTION("HL_NETIFS", s);
    s = launch_string(&cfg, pool, cfg.file_owners_offset);
    if (s[0]) APPLY_OPTION("HL_FILE_OWNERS", s);
    s = launch_string(&cfg, pool, cfg.filesystem_generation_offset);
    if (s[0]) APPLY_OPTION("HL_FSGEN_FILE", s);
    // Per-workspace VPN egress: netns.c reads HL_EGRESS_SOCKS to funnel the
    // guest's genuine external TCP connects through this SOCKS5 proxy. Carried in the typed wire (not the
    // ambient host env, which the FFI spawn never forwards) — "" leaves it unset so direct egress is unchanged.
    s = launch_string(&cfg, pool, cfg.egress_proxy_offset);
    if (s[0]) APPLY_OPTION("HL_EGRESS_SOCKS", s);
    if (cfg.checkpoint_mode & HL_CONFIG_CHECKPOINT_CAPTURE) APPLY_OPTION("HL_CHECKPOINT", "1");
    if (cfg.checkpoint_mode & HL_CONFIG_CHECKPOINT_RESTORE) APPLY_OPTION("HL_RESTORE", "1");
    {
        char checkpoint_policy[2] = {(char)('0' + cfg.checkpoint_policy), 0};
        APPLY_OPTION("HL_CHECKPOINT_POLICY", checkpoint_policy);
    }
#if defined(HL_ENABLE_LOGGING) && HL_ENABLE_LOGGING
    s = launch_string(&cfg, pool, cfg.debug_log_offset);
    if (s[0]) APPLY_OPTION("HL_LOG", s);
#endif

    // Persistent translated-code cache: presence of a directory enables it.
    s = launch_string(&cfg, pool, cfg.translation_cache_offset);
    if (s[0]) {
        APPLY_OPTION("HL_PCACHE", "1");
        APPLY_OPTION("HL_PCACHE_DIR", s);
    }
    /* A typed per-container disable removes cache activation instead of creating a second kill-switch contract. */
    if (cfg.translation_cache_disabled) {
        if (hl_options_unset(&options, "HL_PCACHE") != 0 || hl_options_unset(&options, "HL_PCACHE_DIR") != 0)
            goto option_failure;
    }
    // Untrusted sentry routing is independently useful for compatibility/security tests; public sandbox
    // mode additionally confines the worker. Value 1 retains the ABI4 public behavior, while value 2 selects
    // sentry-only routing without applying a Seatbelt profile to paths supplied by a developer harness.
    if (cfg.sandbox) {
        APPLY_OPTION("HL_UNTRUSTED", "1");
        if (cfg.sandbox == HL_CONFIG_SANDBOX_ENABLED) APPLY_OPTION("HL_SANDBOX", "1");
    }

    // guest argv: NUL-separated, double-NUL terminated, at argv_off. Count, then point argv2[] into the pool.
    size_t argument_count = 0;
    (void)hl_launch_config_arguments_validate(&cfg, pool, &argument_count);
    char **argv2 = (char **)calloc(argument_count + 1, sizeof(char *));
    if (!argv2) {
        hl_options_destroy(&options);
        free(wire);
        return 78;
    }
    for (size_t i = 0; i < argument_count; i++) {
        const char *argument = NULL;
        if (hl_launch_config_argument(&cfg, pool, i, &argument, NULL) != HL_STATUS_OK) {
            free(argv2);
            hl_options_destroy(&options);
            free(wire);
            return 78;
        }
        argv2[i] = (char *)argument;
    }

    // rootfs: "" (bare launch) maps to NULL, matching the flag path's `rootfs = NULL` default.
    const char *rootfs = launch_string(&cfg, pool, cfg.rootfs_offset);
    hl_options *previous_options = hl_options_bind_process(&options);
    const char *result_path = launch_string(&cfg, pool, cfg.result_path_offset);
    const char *executable_host = launch_string(&cfg, pool, cfg.executable_host_offset);
    int rc = runner(rootfs[0] ? rootfs : NULL, executable_host[0] ? executable_host : NULL, (uint32_t)argument_count,
                    argv2, &options, result_path[0] ? result_path : NULL);
    (void)hl_options_bind_process(previous_options);
    // Single-shot process: the guest usually exits the worker; if it returns, release temporary storage.
    free(argv2);
    hl_options_destroy(&options);
    free(wire);
#undef APPLY_OPTION
    return rc;

option_failure:
    hl_options_destroy(&options);
    free(wire);
#undef APPLY_OPTION
    return 78;
}

int hl_run_config_file_with(const char *path, hl_launch_runner runner) {
    if (runner == NULL) return 78;
    if (!path || !path[0]) return 78;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 78;
    /* The unlinked launch wire remains open while the runner owns pointers
     * decoded from it.  It is engine control state, never a guest descriptor. */
    if (hl_host_private_init != NULL) hl_host_private_init();
    if (hl_host_process_fd_private_add != NULL && hl_host_process_fd_private_add(fd) != 0) {
        int floor = hl_host_process_fd_private_floor != NULL ? hl_host_process_fd_private_floor() : -1;
        if (floor >= 0) {
            close(fd);
            return 78;
        }
    }
    unlink(path);
    int rc = hl_read_config_file(fd, runner);
    if (hl_host_process_fd_private_remove != NULL) hl_host_process_fd_private_remove(fd);
    close(fd);
    return rc;
}
