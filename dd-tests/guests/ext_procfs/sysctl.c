// /proc/sys/{kernel,vm,net,fs} — the fixed sysctl constants a modern Linux exposes. Byte-exact (these are
// deterministic kernel defaults, not host-derived), so a stub/empty/placeholder handler is caught here.
#include <stdio.h>
#include <string.h>
#include "pf.h"

static int eq(const char *path, const char *want) {
    char b[512];
    if (pf_read(path, b, sizeof b) < 0) return 0;
    return strcmp(b, want) == 0;
}

int main(void) {
    int ok = 1;
    ok &= eq("/proc/sys/kernel/pid_max", "4194304\n");
    ok &= eq("/proc/sys/kernel/threads-max", "63488\n");
    ok &= eq("/proc/sys/kernel/cap_last_cap", "40\n");
    ok &= eq("/proc/sys/kernel/ostype", "Linux\n");
    ok &= eq("/proc/sys/kernel/osrelease", "6.1.0\n");
    ok &= eq("/proc/sys/kernel/overflowuid", "65534\n");
    ok &= eq("/proc/sys/kernel/sem", "32000\t1024000000\t500\t32000\n"); // TAB-separated, kernel format
    ok &= eq("/proc/sys/vm/max_map_count", "65530\n");
    ok &= eq("/proc/sys/vm/swappiness", "60\n");
    ok &= eq("/proc/sys/vm/overcommit_memory", "0\n");
    ok &= eq("/proc/sys/vm/mmap_min_addr", "65536\n");
    ok &= eq("/proc/sys/net/core/somaxconn", "4096\n");
    ok &= eq("/proc/sys/net/ipv4/ip_local_port_range", "32768\t60999\n");
    ok &= eq("/proc/sys/net/ipv4/tcp_congestion_control", "cubic\n");
    ok &= eq("/proc/sys/net/ipv4/ip_forward", "0\n");
    ok &= eq("/proc/sys/fs/file-max", "1048576\n");
    ok &= eq("/proc/sys/fs/nr_open", "1048576\n");
    ok &= eq("/proc/sys/fs/pipe-max-size", "1048576\n");
    printf("sysctl ok=%d\n", ok);
    return 0;
}
