// CPU topology must be self-consistent across the three synthesized surfaces software cross-checks: the
// number of "processor" blocks in /proc/cpuinfo, the number of per-CPU "cpuN" lines in /proc/stat, and
// sysconf(_SC_NPROCESSORS_CONF). The JVM, Go's runtime.NumCPU fallbacks and OpenMP read these and assume
// they agree; a stub that reports one processor in cpuinfo but several cpuN lines in stat (or disagrees
// with sysconf) mis-sizes thread pools. Assert all three counts are equal and >= 1. Derived, oracle-neutral.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

static int count_prefix_lines(const char *b, const char *prefix) {
    int c = 0;
    size_t pl = strlen(prefix);
    for (const char *p = b; p && *p;) {
        if (!strncmp(p, prefix, pl)) c++;
        const char *nl = strchr(p, '\n');
        p = nl ? nl + 1 : 0;
    }
    return c;
}

int main(void) {
    char b[1 << 16];
    pf_read("/proc/cpuinfo", b, sizeof b);
    int cpuinfo_n = count_prefix_lines(b, "processor");

    pf_read("/proc/stat", b, sizeof b);
    // per-cpu lines are "cpu0", "cpu1", ...; the aggregate "cpu " line does not start with a digit after.
    int stat_n = 0;
    for (const char *p = b; p && *p;) {
        if (!strncmp(p, "cpu", 3) && p[3] >= '0' && p[3] <= '9') stat_n++;
        const char *nl = strchr(p, '\n');
        p = nl ? nl + 1 : 0;
    }

    int conf = (int)sysconf(_SC_NPROCESSORS_CONF);
    int ok = cpuinfo_n >= 1 && cpuinfo_n == stat_n && cpuinfo_n == conf;
    printf("cpucount ok=%d\n", ok);
    return 0;
}
