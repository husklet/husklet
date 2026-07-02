// clock_nanosleep(CLOCK_MONOTONIC, relative): sleeps at least the requested interval and returns 0.
// Portable POSIX -> golden verdict on every engine.
#include <stdio.h>
#include <time.h>

int main(void) {
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    struct timespec req = { .tv_sec = 0, .tv_nsec = 40000000 };  // 40 ms
    int rc = clock_nanosleep(CLOCK_MONOTONIC, 0, &req, NULL);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long long dt = (t1.tv_sec - t0.tv_sec) * 1000000000LL + (t1.tv_nsec - t0.tv_nsec);
    int slept_ge = dt >= 40000000LL;
    int rc0 = rc == 0;
    printf("clocknanosleep rc=%d slept_ge=%d\n", rc0, slept_ge);
    return 0;
}
