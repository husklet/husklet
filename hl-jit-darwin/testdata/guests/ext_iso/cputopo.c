// CPU-topology sysfs conformance: lscpu / util-linux reconstruct sockets/cores/threads by reading
// /sys/devices/system/cpu/cpuN/topology/{core_id,physical_package_id,thread_siblings_list,core_cpus_list,
// core_siblings_list,...} for every online CPU. hl materialized the cpuN directories but served NONE of
// the topology attribute files (every open -> ENOENT), so lscpu mis-counts or errors -- a hl-only
// divergence from real docker (which always serves them). This probe drives lscpu's exact reads and
// prints a HOST-INDEPENDENT structural verdict (the attribute VALUES are host-variant -- core_id need not
// equal N on real SMT hardware -- so we assert STRUCTURE, not bytes): ok=1 iff, for every online CPU,
// every attribute opens and parses to a well-formed value AND the real invariants hold (physical_package_id
// >= 0, core_id >= 0, and cpu N is a member of its OWN thread_siblings_list). Byte-identical on real Linux
// and a correct hl; prints ok=0 (with the first failing path) on the pre-fix engine.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int read_line(const char *path, char *out, size_t n) {
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    if (!fgets(out, (int)n, f)) { fclose(f); return -1; }
    fclose(f);
    size_t L = strlen(out);
    while (L && (out[L - 1] == '\n' || out[L - 1] == ' ')) out[--L] = 0;
    return (int)L;
}

// Does the Linux cpu-list string `list` (e.g. "5", "0-17", "0,4-6") contain integer `v`?
static int list_has(const char *list, int v) {
    const char *p = list;
    while (*p) {
        char *e;
        long a = strtol(p, &e, 10);
        if (e == p) break;
        long b = a;
        if (*e == '-') { b = strtol(e + 1, &e, 10); }
        if (v >= a && v <= b) return 1;
        p = (*e == ',') ? e + 1 : e;
    }
    return 0;
}

int main(void) {
    long nc = sysconf(_SC_NPROCESSORS_ONLN);
    if (nc < 1) { printf("cputopo ok=0 nproc=%ld\n", nc); return 0; }
    for (long i = 0; i < nc; i++) {
        char path[128], buf[128];
        // core_id: a non-negative integer.
        snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%ld/topology/core_id", i);
        if (read_line(path, buf, sizeof buf) < 1) { printf("cputopo ok=0 miss=%s\n", path); return 0; }
        { char *e; long v = strtol(buf, &e, 10); if (e == buf || v < 0) { printf("cputopo ok=0 bad=%s val=%s\n", path, buf); return 0; } }
        // physical_package_id: a non-negative integer (the socket).
        snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%ld/topology/physical_package_id", i);
        if (read_line(path, buf, sizeof buf) < 1) { printf("cputopo ok=0 miss=%s\n", path); return 0; }
        { char *e; long v = strtol(buf, &e, 10); if (e == buf || v < 0) { printf("cputopo ok=0 bad=%s val=%s\n", path, buf); return 0; } }
        // thread_siblings_list: a well-formed cpu-list that INCLUDES this cpu (a real Linux invariant).
        snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%ld/topology/thread_siblings_list", i);
        if (read_line(path, buf, sizeof buf) < 1) { printf("cputopo ok=0 miss=%s\n", path); return 0; }
        if (!list_has(buf, (int)i)) { printf("cputopo ok=0 selfnotin=%s val=%s\n", path, buf); return 0; }
        // core_cpus_list + core_siblings_list: present, non-empty cpu-lists.
        snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%ld/topology/core_cpus_list", i);
        if (read_line(path, buf, sizeof buf) < 1) { printf("cputopo ok=0 miss=%s\n", path); return 0; }
        snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%ld/topology/core_siblings_list", i);
        if (read_line(path, buf, sizeof buf) < 1) { printf("cputopo ok=0 miss=%s\n", path); return 0; }
        if (!list_has(buf, (int)i)) { printf("cputopo ok=0 selfnotin=%s val=%s\n", path, buf); return 0; }
    }
    printf("cputopo ok=1\n");
    return 0;
}
