// runc/Docker list every -v/--mount bind (and --tmpfs) as its own line in /proc/mounts and
// /proc/self/mountinfo. findmnt, df, and the JVM/container mount-discovery scan those tables; a bind
// that resolves for reads but is missing from the mount tables makes them see a false mount namespace.
// Driven with HL_VOL=/mnt:<hostdir>. Verdict ok=1 iff the bind mountpoint appears in BOTH tables.
#include <stdio.h>
#include <string.h>
#include "pf.h"

int main(void) {
    char b[8192];
    int ok = 1;
    ok &= (pf_read("/proc/self/mountinfo", b, sizeof b) > 0 && strstr(b, " /mnt ") != 0);
    ok &= (pf_read("/proc/mounts", b, sizeof b) > 0 && strstr(b, " /mnt ") != 0);
    printf("bindmount ok=%d\n", ok);
    return 0;
}
