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
#include <fcntl.h>
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
            if (getenv("DD_CONFIGFD_DEBUG"))
                fprintf(stderr, "[DDCONFIGFD] fd=%d read error got=%zu want=%zu errno=%d\n", fd, got, n, errno);
            return -1;
        }
        if (r == 0) {
            if (getenv("DD_CONFIGFD_DEBUG")) {
                int fl = fcntl(fd, F_GETFD, 0);
                fprintf(stderr, "[DDCONFIGFD] fd=%d eof got=%zu want=%zu fdflags=%d errno=%d\n", fd, got, n, fl, errno);
            }
            return -1;
        } // premature EOF
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
// rebuild the guest argv, and dispatch to dd_run(). The spawn shim normally enters through
// ddjit_run_configfile() below; --configfd remains supported for direct/debug launches. Returns dd_run()'s
// exit code, or a nonzero code on any read/validation failure. Single-shot per process.
int ddjit_run_configfd(int fd) {
    struct ddjit_config cfg;
    memset(&cfg, 0, sizeof cfg);

    // Phase 1 — the FROZEN 16-byte prefix: magic@0, pool_len@4, header_len@8, abi@12. Those offsets never
    // move, so a writer (ddcli) and reader (engine) built from different commits still agree on where the
    // size + ABI live. This is the skew guard: instead of trusting our own sizeof for the header (which
    // silently over/under-reads when the writer's struct differs → the old cryptic "short read of N pool
    // bytes"), we learn the writer's exact header size and validate the ABI first.
    uint8_t prefix[16];
    if (cfd_read_full(fd, prefix, sizeof prefix) != 0) {
        fprintf(stderr, "dd: --configfd: short read of config header\n");
        return 78;
    }
    uint32_t magic, pool_len, header_len, abi;
    memcpy(&magic, prefix + 0, 4);
    memcpy(&pool_len, prefix + 4, 4);
    memcpy(&header_len, prefix + 8, 4);
    memcpy(&abi, prefix + 12, 4);
    if (magic != DDJIT_CONFIG_MAGIC) {
        fprintf(stderr, "dd: --configfd: bad magic 0x%08x (want 0x%08x)\n", magic, DDJIT_CONFIG_MAGIC);
        return 78;
    }
    if (abi != DDJIT_CONFIG_ABI || header_len < sizeof prefix || header_len > 4096) {
        // A real layout/ABI mismatch (not a survivable tail-append). This is the exact failure that used to
        // masquerade as a pool short-read: the engine and ddcli were built from different commits. Say so.
        fprintf(stderr,
                "dd: --configfd: incompatible config ABI (writer abi=%u header_len=%u; reader abi=%u sizeof=%zu). "
                "The engine and launcher were built from different commits — rebuild both from the same tree and "
                "point DDJIT_DIR at that engine.\n",
                abi, header_len, DDJIT_CONFIG_ABI, sizeof cfg);
        return 78;
    }

    // Phase 2 — copy the prefix in, then consume EXACTLY the writer's header. Tolerate a size skew that is a
    // pure tail-append: read min(rest, our remaining struct) into cfg (any field the writer lacks stays 0 =
    // "unset"), and DISCARD any trailing header bytes a newer writer added so the pool starts at the right
    // offset. header_len makes this boundary explicit — no guessing from sizeof.
    memcpy(&cfg, prefix, sizeof prefix);
    uint32_t rest = header_len - (uint32_t)sizeof prefix;
    uint32_t into = 0;
    if (sizeof cfg > sizeof prefix) {
        uint32_t room = (uint32_t)(sizeof cfg - sizeof prefix);
        into = rest < room ? rest : room;
        if (into && cfd_read_full(fd, (uint8_t *)&cfg + sizeof prefix, into) != 0) {
            fprintf(stderr, "dd: --configfd: short read of config header\n");
            return 78;
        }
    }
    for (uint32_t discard = rest - into; discard;) {
        uint8_t sink[256];
        uint32_t n = discard < sizeof sink ? discard : (uint32_t)sizeof sink;
        if (cfd_read_full(fd, sink, n) != 0) {
            fprintf(stderr, "dd: --configfd: short read of config header\n");
            return 78;
        }
        discard -= n;
    }
    // cfg is now fully populated (cfg.pool_len == pool_len); the code below decodes it exactly as before.

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
    // Per-workspace VPN egress: the engine's netns.c egress_socks() getenv()s DD_EGRESS_SOCKS to funnel the
    // guest's genuine external TCP connects through this SOCKS5 proxy. Carried in the typed wire (not the
    // ambient host env, which the FFI spawn never forwards) — "" leaves it unset so direct egress is unchanged.
    s = cfd_str(pool, cfg.pool_len, cfg.egress_off);
    if (s[0]) setenv("DD_EGRESS_SOCKS", s, 1);

    // persistent translated-code cache: presence of a dir enables it (DDJIT_PCACHE gate + dir).
    s = cfd_str(pool, cfg.pool_len, cfg.pcache_off);
    if (s[0]) {
        setenv("DDJIT_PCACHE", "1", 1);
        setenv("DDJIT_PCACHE_DIR", s, 1);
    }
    // per-container persistent-cache kill switch: carried through typed launch so a single container can opt
    // out even when the runtime enables pcache defaults globally (DDJIT_NOPCACHE wins over DDJIT_PCACHE).
    if (cfg.nopcache) setenv("DDJIT_NOPCACHE", "1", 1);
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

int ddjit_run_configfile(const char *path) {
    if (!path || !path[0]) return 78;
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        fprintf(stderr, "dd: --configfile: open failed: %s\n", path);
        return 78;
    }
    unlink(path);
    int rc = ddjit_run_configfd(fd);
    close(fd);
    return rc;
}
