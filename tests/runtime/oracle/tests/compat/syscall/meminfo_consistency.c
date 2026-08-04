// Memory-size + uptime cross-source consistency. A container runtime sizes its heap off whichever source
// it happens to read -- glibc get_phys_pages()/sysconf(_SC_PHYS_PAGES) via sysinfo(2), the JVM/Go via
// /proc/meminfo and /sys/fs/cgroup/memory.max -- so those sources MUST report the SAME machine size, or one
// runtime sizes for 8 GiB while another sizes for the real host RAM (OOM / mis-tuned GC). This asserts the
// native-true relationships (not absolute byte values, which are dynamic):
//   - sysinfo.totalram * mem_unit == /proc/meminfo MemTotal (kB -> bytes), exactly (both derive from the same
//     page total on Linux; the engine's old sysinfo hardcoded 8 GiB and disagreed with /proc/meminfo -> RED);
//   - freeram <= totalram, and MemFree/MemAvailable <= MemTotal;
//   - sysinfo.uptime is monotonic and agrees with /proc/uptime within a few seconds (the old sysinfo returned
//     a constant 3600 that never advanced and disagreed with /proc/uptime -> RED);
//   - procs > 0, mem_unit >= 1.
// Single boolean, validated by running THROUGH the engine.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/sysinfo.h>
#include <time.h>
#include <unistd.h>

static unsigned long long meminfo_kb(const char *key) {
    FILE *f = fopen("/proc/meminfo", "r");
    char line[256];
    unsigned long long kb = 0;
    if (!f) return 0;
    while (fgets(line, sizeof line, f)) {
        char name[64];
        unsigned long long v;
        if (sscanf(line, "%63[^:]: %llu kB", name, &v) == 2 && strcmp(name, key) == 0) {
            kb = v;
            break;
        }
    }
    fclose(f);
    return kb;
}

static double proc_uptime(void) {
    FILE *f = fopen("/proc/uptime", "r");
    double up = -1;
    if (f) {
        if (fscanf(f, "%lf", &up) != 1) up = -1;
        fclose(f);
    }
    return up;
}

int main(void) {
    int ok = 1;
    struct sysinfo si;
    if (sysinfo(&si) != 0) return 1;

    unsigned long long mem_unit = si.mem_unit ? si.mem_unit : 1;
    unsigned long long totalram = (unsigned long long)si.totalram * mem_unit;
    unsigned long long freeram = (unsigned long long)si.freeram * mem_unit;

    if (mem_unit < 1) ok = 0;
    if (si.totalram == 0) ok = 0;
    if (freeram > totalram) ok = 0;
    if (si.procs == 0) ok = 0;

    // THE KEY relationship: sysinfo totalram must equal /proc/meminfo MemTotal.
    unsigned long long memtotal = meminfo_kb("MemTotal") * 1024ULL;
    if (memtotal == 0) ok = 0;
    if (totalram != memtotal) ok = 0;

    unsigned long long memfree = meminfo_kb("MemFree") * 1024ULL;
    unsigned long long memavail = meminfo_kb("MemAvailable") * 1024ULL;
    if (memfree > memtotal) ok = 0;
    if (memavail > memtotal) ok = 0;

    // uptime: monotonic + agrees with /proc/uptime.
    if ((long)si.uptime <= 0) ok = 0;
    double pu = proc_uptime();
    if (pu < 0) ok = 0;
    double d = (double)si.uptime - pu;
    if (d < 0) d = -d;
    if (d > 5.0) ok = 0; // the old constant 3600 diverged from real /proc/uptime by far more than this

    struct sysinfo si2;
    struct timespec ts = {1, 100000000}; // 1.1s so a per-second uptime must advance
    nanosleep(&ts, NULL);
    if (sysinfo(&si2) == 0 && (long)si2.uptime < (long)si.uptime) ok = 0;

    printf("meminfo-consistency ok=%d\n", ok);
    return 0;
}
