// timerfd (create/settime/gettime + read expiration count) — Linux-only (no macOS equivalent), diffed vs
// the native oracle. Normalized 0/1 verdicts, byte-identical to the oracle on both Linux engines:
//   * A relative one-shot: read() blocks until expiry and returns an 8-byte count of 1.
//   * A periodic timer: after several intervals a blocking read returns a count >= 1 (accumulated ticks).
//   * timerfd_gettime reports a plausible remaining time (0 < rem <= armed value) and it_interval.
//   * TFD_TIMER_ABSTIME: an absolute deadline in the near future fires.
//   * Disarm (it_value 0) -> gettime reports {0,0}.
//   * Error surface: bad clockid -> EINVAL; bad flags -> EINVAL; settime bad new ptr -> EFAULT.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static int read_count(int fd, uint64_t *out) {
    return read(fd, out, sizeof *out) == (ssize_t)sizeof *out;
}

int main(void) {
    // 1) relative one-shot -> read blocks, returns count 1.
    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 40 * 1000 * 1000; // 40ms
    timerfd_settime(fd, 0, &its, NULL);
    uint64_t n = 0;
    int oneshot = read_count(fd, &n) && n == 1;
    close(fd);

    // 2) gettime remaining on a long one-shot: 0 < rem <= 100s, interval reported.
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its, 0, sizeof its);
    its.it_value.tv_sec = 100;
    its.it_interval.tv_sec = 7;
    timerfd_settime(fd, 0, &its, NULL);
    struct itimerspec cur;
    memset(&cur, 0, sizeof cur);
    timerfd_gettime(fd, &cur);
    int rem_ok = cur.it_value.tv_sec > 0 && cur.it_value.tv_sec <= 100 && cur.it_interval.tv_sec == 7;

    // 3) disarm -> gettime {0,0}.
    memset(&its, 0, sizeof its);
    timerfd_settime(fd, 0, &its, NULL);
    memset(&cur, 0, sizeof cur);
    timerfd_gettime(fd, &cur);
    int disarmed = cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec == 0;
    close(fd);

    // 4) periodic -> accumulated ticks >= 1 after several intervals.
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 10 * 1000 * 1000;    // 10ms
    its.it_interval.tv_nsec = 10 * 1000 * 1000; // then every 10ms
    timerfd_settime(fd, 0, &its, NULL);
    usleep(55 * 1000); // ~5 ticks elapse
    n = 0;
    int periodic = read_count(fd, &n) && n >= 1;
    close(fd);

    // 5) TFD_TIMER_ABSTIME: absolute deadline ~40ms in the future fires.
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    memset(&its, 0, sizeof its);
    its.it_value = now;
    its.it_value.tv_nsec += 40 * 1000 * 1000;
    if (its.it_value.tv_nsec >= 1000000000L) { its.it_value.tv_sec++; its.it_value.tv_nsec -= 1000000000L; }
    timerfd_settime(fd, TFD_TIMER_ABSTIME, &its, NULL);
    n = 0;
    int abstime = read_count(fd, &n) && n == 1;
    close(fd);

    // 6) error surface.
    errno = 0;
    int badclock = timerfd_create(4242, 0) == -1 && errno == EINVAL;
    errno = 0;
    int badflags = timerfd_create(CLOCK_MONOTONIC, 0x40) == -1 && errno == EINVAL;
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    errno = 0;
    int efault = timerfd_settime(fd, 0, (const struct itimerspec *)0x1, NULL) == -1 && errno == EFAULT;
    close(fd);

    printf("timerfd oneshot=%d rem=%d disarmed=%d periodic=%d abstime=%d badclock=%d badflags=%d efault=%d\n",
           oneshot, rem_ok, disarmed, periodic, abstime, badclock, badflags, efault);
    return 0;
}
