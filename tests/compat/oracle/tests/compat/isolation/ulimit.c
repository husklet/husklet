// Container rlimit fidelity (docker --ulimit). getrlimit(RLIMIT_NOFILE) inside the container must reflect
// the requested --ulimit, not hl's default -- memcached calloc()s off it, the JVM sizes thread pools off
// RLIMIT_NPROC. Portable: JIT-emulated on Linux (the engine's svc_fill_rlimit returns the override).
#include <stdio.h>
#include <sys/resource.h>

int main(void) {
    struct rlimit r;
    if (getrlimit(RLIMIT_NOFILE, &r) != 0) {
        printf("nofile getrlimit-failed\n");
        return 1;
    }
    printf("nofile soft=%llu hard=%llu\n", (unsigned long long)r.rlim_cur, (unsigned long long)r.rlim_max);
    return 0;
}
