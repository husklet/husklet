// /proc/sys/{kernel,vm,net,fs} conformance. Two kinds of assertion, both byte-identical between a correct
// hl and the real-docker (OrbStack) oracle:
//   * eq()  -- deterministic, well-known kernel defaults (a stub/empty/placeholder handler is caught here).
//   * ge()  -- the app-tuning values whose ABSOLUTE number is host-variant, but which real servers compare
//              against a threshold. We assert the threshold the server needs, so both hl and the oracle pass
//              while the OLD hl constants (which triggered startup warnings a docker user never sees) fail:
//                - vm.overcommit_memory must be 1  (else redis prints its overcommit WARNING)
//                - vm.max_map_count >= 262144      (else elasticsearch refuses to boot)
//                - fs.file-max large, fs.aio-max-nr, fs.inotify.* large (else watchers hit ENOSPC)
//                - fs.mqueue.* present             (hl used to ENOENT these)
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pf.h"

static int eq(const char *path, const char *want) {
    char b[512];
    if (pf_read(path, b, sizeof b) < 0) return 0;
    return strcmp(b, want) == 0;
}
// read the first unsigned integer in the file; -1 if the file is absent.
static long long num(const char *path) {
    char b[512];
    if (pf_read(path, b, sizeof b) < 0) return -1;
    return strtoll(b, NULL, 10);
}
static int ge(const char *path, long long floor) {
    long long v = num(path);
    return v >= floor;
}
static int present(const char *path) { return num(path) >= 0; }

int main(void) {
    int ok = 1;
    // ---- deterministic well-known defaults (regression guards) ----
    ok &= eq("/proc/sys/kernel/pid_max", "4194304\n");
    ok &= eq("/proc/sys/kernel/cap_last_cap", "40\n");
    ok &= eq("/proc/sys/kernel/randomize_va_space", "2\n"); // full ASLR (oracle); hl used to ENOENT this
    ok &= eq("/proc/sys/kernel/ostype", "Linux\n");
    ok &= eq("/proc/sys/kernel/overflowuid", "65534\n");
    // Engine-backed SysV IPC has a bounded launch-scoped table. Discovery must
    // report that enforceable capacity rather than the host kernel's larger
    // defaults; consumers use these numbers to decide when ENOSPC is valid.
    ok &= eq("/proc/sys/kernel/sem", "256\t131072\t500\t512\n"); // TAB-separated, kernel format
    ok &= eq("/proc/sys/kernel/shmmax", "18446744073692774399\n");
    ok &= eq("/proc/sys/kernel/shmmni", "4096\n");
    ok &= eq("/proc/sys/net/core/somaxconn", "4096\n"); // redis/nginx backlog: >= 511, docker default 4096
    ok &= eq("/proc/sys/net/ipv4/ip_local_port_range", "32768\t60999\n");
    ok &= eq("/proc/sys/net/ipv4/tcp_congestion_control", "cubic\n");

    // ---- app-warning differentials (the OLD hl values fail these; oracle + fixed hl pass) ----
    ok &= eq("/proc/sys/vm/overcommit_memory", "1\n"); // redis warns unless exactly 1
    ok &= ge("/proc/sys/vm/max_map_count", 262144);    // elasticsearch bootstrap floor
    ok &= ge("/proc/sys/fs/file-max", 2000000);        // modern kernels report ~LONG_MAX
    ok &= ge("/proc/sys/fs/aio-max-nr", 262144);
    ok &= ge("/proc/sys/fs/inotify/max_user_instances", 1024);   // watchers (VS Code / node chokidar)
    ok &= ge("/proc/sys/fs/inotify/max_queued_events", 65536);
    ok &= ge("/proc/sys/fs/inotify/max_user_watches", 8192);
    ok &= present("/proc/sys/fs/mqueue/msg_max");      // hl used to ENOENT the whole mqueue subtree
    ok &= present("/proc/sys/fs/mqueue/msgsize_max");
    ok &= present("/proc/sys/fs/mqueue/queues_max");

    // ---- host-variant structural guards (pass on both; catch empty/placeholder) ----
    ok &= ge("/proc/sys/kernel/threads-max", 32768);
    ok &= ge("/proc/sys/fs/nr_open", 1048576);
    ok &= ge("/proc/sys/net/core/rmem_max", 131072);
    ok &= ge("/proc/sys/net/ipv4/tcp_max_syn_backlog", 128);
    { long long s = num("/proc/sys/vm/swappiness");  ok &= (s >= 0 && s <= 100); }
    ok &= ge("/proc/sys/vm/mmap_min_addr", 4096);

    printf("sysctl ok=%d\n", ok);
    return 0;
}
