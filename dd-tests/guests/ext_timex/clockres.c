// clock_getres for REALTIME/MONOTONIC/PROCESS_CPUTIME/THREAD_CPUTIME: each resolution is positive
// and finer than a second. Portable POSIX -> golden verdict on every engine.
#include <stdio.h>
#include <time.h>

static int res_ok(clockid_t c) {
    struct timespec ts;
    if (clock_getres(c, &ts) != 0) return 0;
    long long ns = ts.tv_sec * 1000000000LL + ts.tv_nsec;
    return ns > 0 && ns <= 1000000000LL;    // (0, 1s]
}

int main(void) {
    int real = res_ok(CLOCK_REALTIME);
    int mono = res_ok(CLOCK_MONOTONIC);
    int pcpu = res_ok(CLOCK_PROCESS_CPUTIME_ID);
    int tcpu = res_ok(CLOCK_THREAD_CPUTIME_ID);
    printf("clockres real=%d mono=%d pcpu=%d tcpu=%d\n", real, mono, pcpu, tcpu);
    return 0;
}
