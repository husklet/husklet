// dd/runtime/os -- the `--configfd` launch bridge (unity-included once into each engine TU).
//
// The Rust host serializes the container into the position-independent `ddjit_config` wire buffer
// (include/ddjit_api.h) and `posix_spawn`s the arch-matching engine as `<engine> --configfd <fd>`,
// streaming that buffer over `fd`. This is the ENGINE side: read + validate the buffer, then translate
// every populated field back into the exact `DD_*`/`DDJIT_*` environment variable the existing env-driven
// setup (container_init() in targets/*.c, the guest-env reader in os/linux/elf.c, the pcache/sentry
// readers) already consumes, rebuild the guest argv, and hand off to dd_run() -- the identical call the
// normal env/flag launch makes. Reusing the env path means ZERO duplication of container setup logic.
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "../include/ddjit_api.h"

// dd_run() is defined by the including target TU (linux_aarch64.c / linux_x86_64.c / darwin's jitdarwin.c).
int dd_run(const char *rootfs, int argc, char *const argv[]);

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
        if (r == 0) return -1; // premature EOF
        got += (size_t)r;
    }
    return 0;
}

// A pool string by byte offset. Offset 0 (and any out-of-range offset, defensively) reads as the empty
// string -- pool[0] is always a lone NUL, so an "unset" 0 offset naturally yields "".
static const char *cfd_str(const char *pool, uint32_t pool_len, uint32_t off) {
    if (!pool || off >= pool_len) return "";
    return pool + off;
}

// Read a `ddjit_config` (+ its trailing string pool) from `fd`, re-hydrate the engine's DD_*/DDJIT_* env,
// rebuild the guest argv, and dispatch to dd_run(). Returns dd_run()'s exit code, or a nonzero code on any
// read/validation failure. Single-shot per process (the launched guest _exit()s the worker).
int ddjit_run_configfd(int fd) {
    struct ddjit_config cfg;
    if (cfd_read_full(fd, &cfg, sizeof cfg) != 0) {
        fprintf(stderr, "dd: --configfd: short read of config header\n");
        return 78;
    }
    if (cfg.magic != DDJIT_CONFIG_MAGIC) {
        fprintf(stderr, "dd: --configfd: bad magic 0x%08x (want 0x%08x)\n", cfg.magic, DDJIT_CONFIG_MAGIC);
        return 78;
    }
    char *pool = NULL;
    if (cfg.pool_len) {
        pool = (char *)malloc(cfg.pool_len);
        if (!pool) return 78;
        if (cfd_read_full(fd, pool, cfg.pool_len) != 0) {
            fprintf(stderr, "dd: --configfd: short read of %u pool bytes\n", cfg.pool_len);
            free(pool);
            return 78;
        }
    }

    char num[32];
    const char *s;

    // scalars -> the same env vars container_init()/container_read_resource_env() read.
    if (cfg.mem_max) {
        snprintf(num, sizeof num, "%llu", (unsigned long long)cfg.mem_max);
        setenv("DD_MEM_MAX", num, 1);
    }
    if (cfg.pids_max) {
        snprintf(num, sizeof num, "%u", cfg.pids_max);
        setenv("DD_PIDS_MAX", num, 1);
    }
    if (cfg.cpus) {
        snprintf(num, sizeof num, "%u", cfg.cpus);
        setenv("DD_CPUS", num, 1);
    }
    if (cfg.rootfs_ro) setenv("DD_ROOTFS_RO", "1", 1);
    if (cfg.net_isolate) setenv("DD_NET_ISOLATE", "1", 1);
    if (cfg.publish_daemon) setenv("DD_PUBLISH_DAEMON", "1", 1);
    // GPU rung 2/3 (--gui): opt-in the host-IOSurface path. The engine getenv()s this (vfs.c
    // gpu_iosurface_on()); carrying it in the typed config — not the ambient host env — is what makes it
    // reach the engine reliably (the FFI/bridge does not forward the launcher's ambient environment).
    if (cfg.gpu_iosurface) setenv("DD_GPU_IOSURFACE", "1", 1);
    if (cfg.uid >= 0) {
        snprintf(num, sizeof num, "%d", cfg.uid);
        setenv("DD_UID", num, 1);
    }
    if (cfg.gid >= 0) {
        snprintf(num, sizeof num, "%d", cfg.gid);
        setenv("DD_GID", num, 1);
    }

    // pooled strings -> the same env vars (decode via offsets; "" means unset -> leave the env untouched).
    s = cfd_str(pool, cfg.pool_len, cfg.hostname_off);
    if (s[0]) setenv("DD_HOSTNAME", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.ulimits_off);
    if (s[0]) setenv("DD_ULIMITS", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.publish_off);
    if (s[0]) setenv("DD_PUBLISH", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.lowers_off);
    if (s[0]) setenv("DD_LOWER", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.netns_off);
    if (s[0]) setenv("DD_NETNS", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.volumes_off);
    if (s[0]) setenv("DDVOL", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.cwd_off);
    if (s[0]) setenv("DD_CWD", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.guest_env_off);
    if (s[0]) setenv("DD_GUEST_ENV", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.netbr_off);
    if (s[0]) setenv("DD_NETBR", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.ip_off);
    if (s[0]) setenv("DD_IP", s, 1);
    s = cfd_str(pool, cfg.pool_len, cfg.fsgen_off);
    if (s[0]) setenv("DD_FSGEN_FILE", s, 1);

    // persistent translated-code cache: presence of a dir enables it (DDJIT_PCACHE gate + dir).
    s = cfd_str(pool, cfg.pool_len, cfg.pcache_off);
    if (s[0]) {
        setenv("DDJIT_PCACHE", "1", 1);
        setenv("DDJIT_PCACHE_DIR", s, 1);
    }
    // untrusted-guest sentry: both gates as the engine reads them.
    if (cfg.sandbox) {
        setenv("DDJIT_UNTRUSTED", "1", 1);
        setenv("DDJIT_SANDBOX", "1", 1);
    }

    // guest argv: NUL-separated, double-NUL terminated, at argv_off. Count, then point argv2[] into the pool.
    int n = 0;
    if (cfg.argv_off && cfg.argv_off < cfg.pool_len) {
        const char *a = pool + cfg.argv_off;
        const char *end = pool + cfg.pool_len;
        while (a < end && *a) {
            n++;
            a += strlen(a) + 1;
        }
    }
    if (n == 0) {
        fprintf(stderr, "dd: --configfd: empty guest argv\n");
        free(pool);
        return 78;
    }
    char **argv2 = (char **)calloc((size_t)n + 1, sizeof(char *));
    if (!argv2) {
        free(pool);
        return 78;
    }
    {
        char *a = pool + cfg.argv_off;
        for (int i = 0; i < n; i++) {
            argv2[i] = a;
            a += strlen(a) + 1;
        }
        argv2[n] = NULL; // execv-style NULL terminator
    }

    // rootfs: "" (bare launch) maps to NULL, matching the flag path's `rootfs = NULL` default.
    const char *rootfs = cfd_str(pool, cfg.pool_len, cfg.rootfs_off);
    int rc = dd_run(rootfs[0] ? rootfs : NULL, n, argv2);
    // Single-shot process: dd_run typically _exit()s the worker and never returns; if it does, release.
    free(argv2);
    free(pool);
    return rc;
}
