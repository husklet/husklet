// prlimit(2): read RLIMIT_NOFILE, lower the soft limit, read it back, then restore. Linux-only
// (glibc prlimit) -> native oracle.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/resource.h>
#include <unistd.h>

int main(void) {
    struct rlimit old;
    int got = prlimit(0, RLIMIT_NOFILE, NULL, &old) == 0;

    struct rlimit lo = old;
    if (lo.rlim_max == RLIM_INFINITY || lo.rlim_max > 256) lo.rlim_cur = 256;
    else lo.rlim_cur = lo.rlim_max;
    int set = prlimit(0, RLIMIT_NOFILE, &lo, NULL) == 0;

    struct rlimit cur;
    prlimit(0, RLIMIT_NOFILE, NULL, &cur);
    int lowered = cur.rlim_cur == lo.rlim_cur;

    // restore the original soft limit
    int restored = prlimit(0, RLIMIT_NOFILE, &old, NULL) == 0;
    printf("prlimit got=%d set=%d lowered=%d restored=%d\n", got, set, lowered, restored);
    return 0;
}
