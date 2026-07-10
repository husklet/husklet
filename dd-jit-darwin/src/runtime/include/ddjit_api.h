// dd-jit-darwin FFI: the typed launch contract between the Rust runtime and the C engine.
//
// The Rust side (dd-jit) builds a container purely as a typed value, serializes it into the
// position-independent `ddjit_config` wire buffer below, and calls `ddjit_spawn()`. The C side
// `posix_spawn`s the arch-matching engine with `--configfd <fd>` and writes the buffer to it — NO
// argv flag soup, NO `DD_*` environment dialect. The engine reads the buffer, populates the same
// container globals `container_init` sets, and runs the guest; guest exit `_exit()`s the worker, so
// the returned pid is the whole container's lifetime. Engine *tuning* knobs (DDJIT_*, JT, …) are a
// separate, engine-internal concern and are NOT part of this contract.
#ifndef DDJIT_API_H
#define DDJIT_API_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#define DDJIT_CONFIG_MAGIC 0x44434647u /* 'DCFG' */

// The fixed header of the wire buffer. Every `*_off` is a byte offset into the string pool that
// immediately follows this header (`buf = <ddjit_config header><pool[pool_len]>`); 0 means "unset"
// (offset 0 of the pool is always a lone NUL so 0 reads as the empty string). Strings are NUL-
// terminated; list fields reuse the same delimiters the engine already parses (see the field notes).
struct ddjit_config {
    uint32_t magic;      // DDJIT_CONFIG_MAGIC (version/sanity)
    uint32_t pool_len;   // bytes of string pool trailing this header

    uint64_t mem_max;    // cgroup memory.max bytes (0 = unlimited)
    uint32_t pids_max;   // pids.max (0 = unlimited)
    uint32_t cpus;       // online-CPU count to advertise (0 = unlimited)
    int32_t  uid;        // run uid (-1 = default/root)
    int32_t  gid;        // run gid (-1 = default/root)
    uint32_t rootfs_ro;      // bool: rootfs/overlay-upper is read-only
    uint32_t sandbox;        // bool: run under the untrusted-guest sentry
    uint32_t net_isolate;    // bool: `--network none` — refuse all non-loopback egress
    uint32_t publish_daemon; // bool: an external host forwarder owns published ports (engine skips its own)

    uint32_t rootfs_off;    // container rootfs (the writable upper for an overlay)
    uint32_t lowers_off;    // ':'-joined read-only overlay lowers, highest-priority first
    uint32_t hostname_off;  // UTS hostname
    uint32_t netns_off;     // private-loopback key (NOT the /tmp path); "" = shared
    uint32_t publish_off;   // "hostPort:containerPort,…" (tcp)
    uint32_t volumes_off;   // "[ro:]guestPath:hostDir,…"
    uint32_t ulimits_off;   // "name=soft:hard,…"
    uint32_t cwd_off;       // initial working dir inside the container
    uint32_t guest_env_off; // '\n'-joined KEY=VAL guest environment (never the host env)
    uint32_t pcache_off;    // persistent translated-code cache dir ("" = disabled)
    uint32_t netbr_off;     // user-network virtual-switch id (DD_NETBR); "" = none
    uint32_t ip_off;        // this container's IP on that switch (DD_IP); "" = none
    uint32_t fsgen_off;     // shared external-writer generation file (DD_FSGEN_FILE); "" = none
    uint32_t argv_off;      // the guest argv: NUL-separated, double-NUL terminated

    uint32_t gpu_iosurface; // bool: opt-in the host-IOSurface GPU path (--gui); render-node synth + DD_IOCTL_GPU_ALLOC
    uint32_t nopcache;      // bool: per-container persistent-cache kill switch (DDJIT_NOPCACHE) — was reserved pad
};

// `flags` bits for ddjit_spawn(): how the child is placed relative to the caller's session/terminal.
#define DDJIT_SPAWN_SETPGID 0x1u // child leads a new process group (setpgid(0,0)) — pause/kill reach it via killpg
#define DDJIT_SPAWN_TTY     0x2u // child acquires a controlling terminal (setsid + TIOCSCTTY); in/out/err = pty slave

// Spawn a container: `fork`, wire the child's stdio + placement per `flags`, `execve` the engine at
// `engine_path` as `<engine> --configfd <fd>`, and stream the serialized config buffer to it over an
// inherited pipe; returns the child pid (the container's lifetime), or -1 on failure (errno set).
//
// `in_fd`/`out_fd`/`err_fd` become the child's fd 0/1/2 via dup2 (-1 = inherit the caller's). The caller
// OWNS those fds and closes its own copies after this returns — the shim never closes them. `flags` is a
// bitwise-OR of the DDJIT_SPAWN_* bits above. No engine symbols are referenced here, so this translation
// unit links cleanly into the Rust host process without pulling the engine in.
pid_t ddjit_spawn(const char *engine_path, const uint8_t *config, size_t config_len,
                  int in_fd, int out_fd, int err_fd, uint32_t flags);

#endif /* DDJIT_API_H */
