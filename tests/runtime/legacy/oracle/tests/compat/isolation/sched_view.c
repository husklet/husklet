// Container scheduler-view coherence. The online-CPU count from sysconf(_SC_NPROCESSORS_ONLN), the
// sched_getaffinity(2) mask population, and sched_getcpu()'s returned index must be mutually
// consistent (Go's GOMAXPROCS, glibc, and OpenMP all cross-use these). We emit a normalized verdict:
// affinity==sysconf, the running CPU is inside the mask, and both are positive — identical on a bare
// host and a correct engine regardless of the actual allotment; a divergence flags a CPU-view defect.
#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>
#include <sched.h>

int main(void) {
    long online = sysconf(_SC_NPROCESSORS_ONLN);
    long conf = sysconf(_SC_NPROCESSORS_CONF);

    cpu_set_t set;
    CPU_ZERO(&set);
    int aff_ok = sched_getaffinity(0, sizeof set, &set) == 0;
    int aff = aff_ok ? CPU_COUNT(&set) : -1;

    int cpu = sched_getcpu();
    int cpu_in_mask = (cpu >= 0 && aff_ok) ? CPU_ISSET(cpu, &set) : 0;

    printf("online_positive=%d conf_ge_online=%d affinity_matches_online=%d\n",
           online > 0, conf >= online, aff == (int)online);
    printf("getcpu_valid=%d getcpu_in_mask=%d getcpu_below_conf=%d\n",
           cpu >= 0, cpu_in_mask, cpu >= 0 && cpu < (int)conf);
    return 0;
}
