/* timerfd lifecycle: create a CLOCK_MONOTONIC timer fd, arm it with timerfd_settime, read it back
   with timerfd_gettime, and confirm the pending interval/value round-trips. A correct engine
   round-trips these or reports ENOSYS. Derived booleans (armed state, value agreement), never the
   raw nanosecond counters, so the output is arch-neutral and host-independent. */
#include "compat.h"
#include <stdio.h>
#include <string.h>
#include <sys/timerfd.h>

int main(void) {
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    if (fd < 0 && errno == ENOSYS) { printf("timerfd unsupported=1\n"); return 0; }
    int create_ok = fd >= 0;

    struct itimerspec want, got;
    memset(&want, 0, sizeof want);
    want.it_value.tv_sec = 100;              /* far future: still pending when we read back */
    want.it_interval.tv_sec = 5;
    int set_ok = create_ok && timerfd_settime(fd, 0, &want, NULL) == 0;

    memset(&got, 0, sizeof got);
    int get_ok = set_ok && timerfd_gettime(fd, &got) == 0;
    int interval_ok = get_ok && got.it_interval.tv_sec == 5;
    int pending = get_ok && (got.it_value.tv_sec > 0 && got.it_value.tv_sec <= 100);

    /* disarming zeroes it_value */
    struct itimerspec off;
    memset(&off, 0, sizeof off);
    int disarm_ok = create_ok && timerfd_settime(fd, 0, &off, NULL) == 0 &&
                    timerfd_gettime(fd, &got) == 0 && got.it_value.tv_sec == 0 &&
                    got.it_value.tv_nsec == 0;

    if (fd >= 0) close(fd);
    printf("timerfd create=%d set=%d interval=%d pending=%d disarm=%d\n",
           create_ok, set_ok, interval_ok, pending, disarm_ok);
    return 0;
}
