// timerfd first-deadline: a PERIODIC timer whose initial it_value (20ms) is much earlier than its
// it_interval (10s) must fire FIRST at it_value, then every it_interval. A kqueue-backed timer that
// collapses the first deadline into the interval would not become readable for ~10s. We poll for
// readiness within a 200ms budget (well under the interval), then read the 8-byte expiration count.
// Native Linux: ready=1 n8=1 exp1=1. A first-deadline-collapsing bug yields ready=0 n8=0 exp1=0.
#define _GNU_SOURCE
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/timerfd.h>
#include <unistd.h>

int main(void) {
    int tfd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    struct itimerspec its = {
        .it_value = {0, 20 * 1000 * 1000},    // first expiry at 20ms
        .it_interval = {10, 0},               // then every 10s (never reached within the test budget)
    };
    int set = timerfd_settime(tfd, 0, &its, NULL) == 0;
    struct pollfd pfd = {.fd = tfd, .events = POLLIN};
    int ready = poll(&pfd, 1, 200) == 1 && (pfd.revents & POLLIN); // 200ms budget >> 20ms, << 10s
    uint64_t exp = 0;
    int n8 = ready && read(tfd, &exp, sizeof exp) == (ssize_t)sizeof exp;
    close(tfd);
    printf("timerfd_first set=%d ready=%d n8=%d exp1=%d\n", set, ready, n8, exp >= 1); // 1 1 1 1
    return 0;
}
