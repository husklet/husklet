// pselect() and ppoll() with no ready fds must block for approximately their timeout and then
// return 0 (timeout expired). We assert the return is 0 and the elapsed MONOTONIC time is at least
// most of the requested timeout (never returns early) and not absurdly long.
#define _GNU_SOURCE
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <sys/select.h>
#include <time.h>

static long long ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

int main(void) {
    // pselect with empty sets, 60ms timeout.
    struct timespec pto = {0, 60 * 1000 * 1000};
    long long t0 = ns();
    int pr = pselect(0, NULL, NULL, NULL, &pto, NULL);
    long long e1 = ns() - t0;
    int pselect_ok = pr == 0 && e1 >= 40LL * 1000 * 1000 && e1 < 2000LL * 1000 * 1000;

    // ppoll with no fds, 60ms timeout.
    struct timespec qto = {0, 60 * 1000 * 1000};
    long long t1 = ns();
    int qr = ppoll(NULL, 0, &qto, NULL);
    long long e2 = ns() - t1;
    int ppoll_ok = qr == 0 && e2 >= 40LL * 1000 * 1000 && e2 < 2000LL * 1000 * 1000;

    // poll() with a 0 timeout returns immediately with 0.
    int poll0 = poll(NULL, 0, 0) == 0;

    // select() with a 0 timeout returns 0 immediately too.
    struct timeval z = {0, 0};
    int select0 = select(0, NULL, NULL, NULL, &z) == 0;

    printf("pselectppoll pselect=%d ppoll=%d poll0=%d select0=%d\n", pselect_ok, ppoll_ok, poll0,
           select0);
    return 0;
}
