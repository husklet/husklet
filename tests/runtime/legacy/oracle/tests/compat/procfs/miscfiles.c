// /proc top-level text files: version, filesystems, uptime, loadavg, cmdline, self/cmdline, self/comm.
// Content shape asserted exactly (fixed strings for version/filesystems; numeric shape for uptime/loadavg).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
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

    // /proc/self/limits RLIMIT_NOFILE row MUST agree with getrlimit(RLIMIT_NOFILE): a guest reads the
    // soft/hard ceiling from both surfaces (glibc/JVM/systemd) and they must never disagree. The engine
    // presents the docker container view (soft 20480, hard = the enforceable guest fd ceiling, which is
    // derived from the host RLIMIT_NOFILE and capped at HL_LINUX_FD_LIMIT), so assert the RELATIONSHIP
    // against the live getrlimit value rather than a hard-coded number that drifts with the host limit.
    // Old hl reported soft 1024 and a /proc hard (1048576) that diverged from getrlimit (65536).
    pf_read("/proc/self/limits", b, sizeof b);
    {
        struct rlimit rl = {0, 0};
        long want_soft = getrlimit(RLIMIT_NOFILE, &rl) == 0 ? (long)rl.rlim_cur : -1;
        long want_hard = rl.rlim_max == RLIM_INFINITY ? -1 : (long)rl.rlim_max;
        char *ln = strstr(b, "Max open files");
        long soft = 0, hard = 0;
        ok &= ln && sscanf(ln + 14, "%ld %ld", &soft, &hard) == 2 && soft == want_soft && hard == want_hard;
    }

    printf("miscfiles ok=%d\n", ok);
    return 0;
}
