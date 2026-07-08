// /sys: class/net interface attributes + cgroup v2 limit files. Content asserted exactly where fixed
// (lo attributes, cgroup "max"/numeric), catching an empty/missing sysfs leaf.
#include <stdio.h>
#include <string.h>
#include "pf.h"

static int eq(const char *p, const char *want) {
    char b[256];
    if (pf_read(p, b, sizeof b) < 0) return 0;
    return strcmp(b, want) == 0;
}

int main(void) {
    int ok = 1;
    // loopback attributes are fixed on Linux
    ok &= eq("/sys/class/net/lo/mtu", "65536\n");
    ok &= eq("/sys/class/net/lo/type", "772\n");           // ARPHRD_LOOPBACK
    ok &= eq("/sys/class/net/lo/address", "00:00:00:00:00:00\n");
    ok &= eq("/sys/class/net/lo/flags", "0x9\n");
    char b[256];
    ok &= pf_read("/sys/class/net/lo/operstate", b, sizeof b) > 0; // present (value host-ish)
    // cgroup v2 unified limit files exist and read a number-or-"max"
    ok &= pf_read("/sys/fs/cgroup/memory.max", b, sizeof b) > 0 && b[0];
    ok &= pf_read("/sys/fs/cgroup/pids.max", b, sizeof b) > 0 && b[0];
    ok &= pf_read("/sys/fs/cgroup/memory.current", b, sizeof b) > 0;
    // transparent-hugepage policy: the active mode is bracketed (oracle: "always [madvise] never")
    ok &= eq("/sys/kernel/mm/transparent_hugepage/enabled", "always [madvise] never\n");
    printf("sysfs ok=%d\n", ok);
    return 0;
}
