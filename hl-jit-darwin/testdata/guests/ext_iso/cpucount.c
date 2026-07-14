// Container CPU-count fidelity (docker --cpus). The guest must see its CPU ALLOTMENT, not the host's
// core count: glibc __get_nprocs / GOMAXPROCS / JVM availableProcessors all derive from these. Portable:
// the SAME source runs JIT-emulated on the two Linux engines.
// On Linux we cross-check sched_getaffinity (the engine caps its mask) against sysconf so a divergence
// between the two reporting paths is caught, not hidden.
#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>
#ifdef __linux__
#include <sched.h>
#endif

int main(void) {
    long n = sysconf(_SC_NPROCESSORS_ONLN);
#ifdef __linux__
    cpu_set_t set;
    CPU_ZERO(&set);
    if (sched_getaffinity(0, sizeof set, &set) == 0) {
        int aff = CPU_COUNT(&set);
        if (aff != (int)n) { // the two paths (sysconf vs affinity mask) must agree on the allotment
            printf("cpucount MISMATCH sysconf=%ld affinity=%d\n", n, aff);
            return 1;
        }
    }
#endif
    printf("cpucount=%ld\n", n);
    return 0;
}
