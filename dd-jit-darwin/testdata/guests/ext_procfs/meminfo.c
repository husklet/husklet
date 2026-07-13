// /proc/meminfo — host-derived numbers, so assert the SHAPE exactly: the keys real software (free(1),
// glibc, the JVM, jemalloc) reads must all be present with the "kB" unit, MemTotal must be positive, and
// MemAvailable/MemFree must not exceed MemTotal. Catches an empty/stub meminfo.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pf.h"

static long kv(const char *b, const char *k) {
    char v[64];
    if (!pf_line_val(b, k, v, sizeof v)) return -1;
    return atol(v); // "NNN kB" -> atol stops at space
}

int main(void) {
    char b[8192];
    int n = pf_read("/proc/meminfo", b, sizeof b);
    long mt = kv(b, "MemTotal:"), mf = kv(b, "MemFree:"), ma = kv(b, "MemAvailable:");
    int has_buffers = pf_has(b, "Buffers:"), has_cached = pf_has(b, "Cached:"), has_swap = pf_has(b, "SwapTotal:");
    int units_kb = pf_has(b, " kB\n");
    int total_pos = mt > 0;
    int free_ok = mf >= 0 && mf <= mt;
    int avail_ok = ma >= 0 && ma <= mt;
    int ok = n > 0 && total_pos && free_ok && avail_ok && has_buffers && has_cached && has_swap && units_kb;
    printf("meminfo ok=%d\n", ok);
    return 0;
}
