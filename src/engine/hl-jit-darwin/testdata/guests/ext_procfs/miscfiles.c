// /proc top-level text files: version, filesystems, uptime, loadavg, cmdline, self/cmdline, self/comm.
// Content shape asserted exactly (fixed strings for version/filesystems; numeric shape for uptime/loadavg).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pf.h"

int main(void) {
    char b[8192];
    int ok = 1;

    pf_read("/proc/version", b, sizeof b);
    ok &= !strncmp(b, "Linux version ", 14);

    pf_read("/proc/filesystems", b, sizeof b);
    ok &= pf_has(b, "ext4") && pf_has(b, "overlay") && pf_has(b, "tmpfs") && pf_has(b, "proc") &&
          pf_has(b, "sysfs") && pf_has(b, "cgroup2");

    // uptime: two floats, first (system uptime) > 0
    pf_read("/proc/uptime", b, sizeof b);
    double up = 0, idle = -1;
    ok &= sscanf(b, "%lf %lf", &up, &idle) == 2 && up > 0 && idle >= 0;

    // loadavg: "a b c r/t pid" — three floats, a running/total token with '/', a pid
    pf_read("/proc/loadavg", b, sizeof b);
    double l0, l1, l2;
    char rt[32]; int lp = 0;
    ok &= sscanf(b, "%lf %lf %lf %31s %d", &l0, &l1, &l2, rt, &lp) == 5 && strchr(rt, '/') && lp >= 1;

    ok &= pf_read("/proc/cmdline", b, sizeof b) > 0 && b[0];         // kernel cmdline nonempty
    ok &= pf_read("/proc/self/comm", b, sizeof b) > 0 && b[0] != '\n'; // our command name

    // /proc/self/limits: docker container default soft/hard RLIMIT_NOFILE (oracle 20480/1048576, must
    // agree with getrlimit/svc_fill_rlimit). Old hl reported soft 1024.
    pf_read("/proc/self/limits", b, sizeof b);
    {
        char *ln = strstr(b, "Max open files");
        long soft = 0, hard = 0;
        ok &= ln && sscanf(ln + 14, "%ld %ld", &soft, &hard) == 2 && soft == 20480 && hard == 1048576;
    }

    printf("miscfiles ok=%d\n", ok);
    return 0;
}
