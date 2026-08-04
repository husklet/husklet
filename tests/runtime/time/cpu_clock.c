#define _GNU_SOURCE
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

enum { CPUCLOCK_SCHED = 2, CPUCLOCK_PERTHREAD_MASK = 4 };

static clockid_t caller_clock(int per_thread) {
    return (clockid_t)(-8 | CPUCLOCK_SCHED |
                       (per_thread ? CPUCLOCK_PERTHREAD_MASK : 0));
}

static int works(clockid_t clock) {
    struct timespec resolution = {0};
    struct timespec now = {0};
    return syscall(SYS_clock_getres, clock, &resolution) == 0 &&
           syscall(SYS_clock_gettime, clock, &now) == 0 &&
           resolution.tv_sec == 0 && resolution.tv_nsec > 0 &&
           now.tv_sec >= 0 && now.tv_nsec >= 0 && now.tv_nsec < 1000000000L;
}

int main(void) {
    printf("dynamic-cpu process=%d thread=%d\n", works(caller_clock(0)),
           works(caller_clock(1)));
    return 0;
}
