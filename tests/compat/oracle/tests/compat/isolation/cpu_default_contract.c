// Deterministic contract adapter for ext_iso/cpudefault.c.
// The legacy source is preserved byte-for-byte as cpudefault.c; this derived probe asserts the same
// four-path agreement without embedding the host's varying active-CPU count in stdout.
#define _GNU_SOURCE
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int slurp(const char *path, char *buf, int cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    int n = 0, r;
    while (n < cap - 1 && (r = (int)read(fd, buf + n, (size_t)(cap - 1 - n))) > 0) n += r;
    close(fd);
    buf[n] = 0;
    return n;
}
static int cpuinfo_count(void) {
    char buf[16384];
    if (slurp("/proc/cpuinfo", buf, sizeof buf) < 0) return -1;
    int count = 0;
    for (char *line = buf; line && *line;) {
        if (strncmp(line, "processor", 9) == 0) count++;
        char *nl = strchr(line, '\n');
        line = nl ? nl + 1 : NULL;
    }
    return count;
}
static int online_count(void) {
    char buf[256];
    if (slurp("/sys/devices/system/cpu/online", buf, sizeof buf) < 0) return -1;
    int total = 0;
    for (char *p = buf; *p;) {
        if (*p < '0' || *p > '9') { p++; continue; }
        int lo = 0;
        while (*p >= '0' && *p <= '9') lo = lo * 10 + (*p++ - '0');
        int hi = lo;
        if (*p == '-') {
            p++; hi = 0;
            while (*p >= '0' && *p <= '9') hi = hi * 10 + (*p++ - '0');
        }
        total += hi - lo + 1;
    }
    return total;
}
int main(void) {
    long sys = sysconf(_SC_NPROCESSORS_ONLN);
    cpu_set_t set;
    CPU_ZERO(&set);
    int affinity = sched_getaffinity(0, sizeof set, &set) == 0 ? CPU_COUNT(&set) : -1;
    int cpuinfo = cpuinfo_count(), online = online_count();
    int consistent = sys == affinity && sys == cpuinfo && sys == online && sys >= 1;
    printf("cpu-default consistent=%d multicore=%d\n", consistent, sys >= 2);
    return 0;
}
