// Container CPU-count fidelity with NO --cpus (regression #412). Every path a guest uses to size its
// worker pools must report the TRUE host core count, not 1: an unconstrained container that self-sizes to
// a single CPU never scales. We cross-check the FOUR reporting paths the engine synthesizes and require
// them to agree, then print the agreed count:
//   - sysconf(_SC_NPROCESSORS_ONLN)                 (glibc __get_nprocs)
//   - sched_getaffinity(0) mask popcount            (Go / tcmalloc)
//   - /proc/cpuinfo "processor" block count         (JVM / feature probes)
//   - /sys/devices/system/cpu/online range          (glibc __get_nprocs fast path)
// Run with an Oracle check (no --cpus): the JIT output must byte-match the native oracle, i.e. the real
// host core count. Before the fix the mac-side engine's sysconf returned 1, so the JIT printed cpus=1
// while native printed the host count -> oracle mismatch. Also run with --cpus=2 -> must clamp to cpus=2.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sched.h>

// Read a whole small proc/sys file into buf; returns byte count (>=0) or -1.
static int slurp(const char *p, char *buf, int cap) {
    int fd = open(p, O_RDONLY);
    if (fd < 0) return -1;
    int n = 0, r;
    while (n < cap - 1 && (r = read(fd, buf + n, cap - 1 - n)) > 0) n += r;
    close(fd);
    buf[n < 0 ? 0 : n] = 0;
    return n;
}

// Count "processor" blocks in /proc/cpuinfo (lines starting with "processor").
static int cpuinfo_count(void) {
    char b[16384];
    if (slurp("/proc/cpuinfo", b, sizeof b) < 0) return -1;
    int c = 0;
    for (char *l = b; l && *l; ) {
        if (!strncmp(l, "processor", 9)) c++;
        char *nl = strchr(l, '\n');
        l = nl ? nl + 1 : 0;
    }
    return c;
}

// Parse the kernel CPU-range format ("0", "0-17", "0-3,8-11") -> count of listed CPUs.
static int online_count(void) {
    char b[256];
    if (slurp("/sys/devices/system/cpu/online", b, sizeof b) < 0) return -1;
    int total = 0;
    char *s = b;
    while (*s) {
        if (*s < '0' || *s > '9') { s++; continue; }
        int lo = 0; while (*s >= '0' && *s <= '9') lo = lo * 10 + (*s++ - '0');
        int hi = lo;
        if (*s == '-') { s++; hi = 0; while (*s >= '0' && *s <= '9') hi = hi * 10 + (*s++ - '0'); }
        total += hi - lo + 1;
    }
    return total;
}

int main(void) {
    long sc = sysconf(_SC_NPROCESSORS_ONLN);
    cpu_set_t set; CPU_ZERO(&set);
    int aff = (sched_getaffinity(0, sizeof set, &set) == 0) ? CPU_COUNT(&set) : -1;
    int ci = cpuinfo_count();
    int on = online_count();

    if (sc != aff || sc != ci || sc != on || sc < 1) {
        printf("cpucount MISMATCH sysconf=%ld affinity=%d cpuinfo=%d online=%d\n", sc, aff, ci, on);
        return 1;
    }
    printf("cpus=%ld\n", sc);
    return 0;
}
