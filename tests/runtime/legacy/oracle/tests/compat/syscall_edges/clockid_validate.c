// clock_gettime/clock_getres/clock_nanosleep argument validation is a fixed Linux ABI error surface.
// An unknown clockid is EINVAL; a dynamic clockid encoding an invalid fd is EINVAL; the well-known
// clocks all succeed. Only booleans/errnos are printed (never the timespec VALUES, which are
// host/time dependent), so the golden is host-invariant. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <time.h>

int main(void) {
    struct timespec ts;
    // Valid clocks succeed for both gettime and getres.
    printf("realtime_ok=%d mono_ok=%d\n",
           clock_gettime(CLOCK_REALTIME, &ts) == 0, clock_gettime(CLOCK_MONOTONIC, &ts) == 0);
    printf("res_realtime_ok=%d res_mono_ok=%d\n",
           clock_getres(CLOCK_REALTIME, &ts) == 0, clock_getres(CLOCK_MONOTONIC, &ts) == 0);
    printf("boottime_ok=%d proccpu_ok=%d\n",
           clock_gettime(CLOCK_BOOTTIME, &ts) == 0, clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts) == 0);
    // Unknown clockid -> EINVAL(22).
    printf("gettime_bad_errno=%d getres_bad_errno=%d\n",
           clock_gettime((clockid_t)0x7fff, &ts) == -1 ? errno : 0,
           clock_getres((clockid_t)0x7fff, &ts) == -1 ? errno : 0);
    // clock_nanosleep with an unknown clockid -> EINVAL before it ever sleeps.
    struct timespec req = {0, 1};
    printf("nanosleep_bad_errno=%d\n", clock_nanosleep((clockid_t)0x7fff, 0, &req, NULL));
    // clock_nanosleep with a negative nanoseconds field -> EINVAL.
    struct timespec neg = {0, -1};
    printf("nanosleep_negns_errno=%d\n", clock_nanosleep(CLOCK_MONOTONIC, 0, &neg, NULL));
    return 0;
}
