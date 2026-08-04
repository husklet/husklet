// /sys/devices/system/cpu is where lscpu, numactl and the JVM's container CPU detection read the online
// CPU set. Assert: /sys/devices/system/cpu/online parses as a CPU range list whose total count equals
// sysconf(_SC_NPROCESSORS_ONLN); the "possible" list is present and covers at least the online set; and a
// per-CPU directory (cpu0) exists. Host-value-neutral (the count derives from the container's own CPU
// allotment, matched against libc), catching an empty or inconsistent cpu sysfs tree.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "pf.h"

static int list_count(const char *s) {
    int total = 0;
    while (*s && *s != '\n') {
        char *end;
        long a = strtol(s, &end, 10);
        if (end == s) break;
        long b = a;
        if (*end == '-') b = strtol(end + 1, &end, 10);
        total += (int)(b - a + 1);
        s = (*end == ',') ? end + 1 : end;
    }
    return total;
}

int main(void) {
    char b[512];
    int online_ok = pf_read("/sys/devices/system/cpu/online", b, sizeof b) > 0;
    int online = online_ok ? list_count(b) : -1;
    int poss_ok = pf_read("/sys/devices/system/cpu/possible", b, sizeof b) > 0;
    int possible = poss_ok ? list_count(b) : -1;

    int nproc = (int)sysconf(_SC_NPROCESSORS_ONLN);
    struct stat st;
    int cpu0_ok = stat("/sys/devices/system/cpu/cpu0", &st) == 0 && S_ISDIR(st.st_mode);

    int ok = online_ok && poss_ok && online == nproc && possible >= online && cpu0_ok;
    printf("cpusysfs ok=%d\n", ok);
    return 0;
}
