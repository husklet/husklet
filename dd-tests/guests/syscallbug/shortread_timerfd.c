// syscall-compat regression: a too-short read() of a timerfd must return EINVAL and leave the pending
// expiration queued -- not consume it and report a bogus success.
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/timerfd.h>
#include <unistd.h>

int main(void) {
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec its = {{0, 0}, {0, 20 * 1000 * 1000}}; // one-shot 20ms
    timerfd_settime(tfd, 0, &its, NULL);
    struct pollfd pf = {tfd, POLLIN, 0};
    poll(&pf, 1, 1000);
    char small[4];
    ssize_t sr = read(tfd, small, sizeof small); // < 8 -> EINVAL, expiration preserved
    int se = (sr == -1) ? errno : 0;
    uint64_t v = 0;
    ssize_t fr = read(tfd, &v, 8);
    printf("timerfd short=%zd serr=%d full=%zd val=%llu\n", sr, se, fr, (unsigned long long)v);
    return 0;
}
