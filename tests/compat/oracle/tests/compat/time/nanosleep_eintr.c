// nanosleep(): a full sleep of a short interval returns 0; an interrupted sleep (SIGALRM via
// setitimer mid-sleep) returns -1/EINTR with a remaining time that is > 0 and < the request.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>

static void noop(int s) { (void)s; }

int main(void) {
    // Basic full sleep of 50ms completes with return 0.
    struct timespec req = {0, 50 * 1000 * 1000}, rem = {0, 0};
    int full = nanosleep(&req, &rem) == 0;

    // Interrupt a 500ms sleep after ~50ms; expect EINTR with 0 < remaining < 500ms.
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = noop;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0; // no SA_RESTART -> nanosleep returns EINTR
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 50 * 1000; // fire once at 50ms
    setitimer(ITIMER_REAL, &it, NULL);

    struct timespec longreq = {0, 500 * 1000 * 1000}, left = {0, 0};
    errno = 0;
    int r = nanosleep(&longreq, &left);
    long long leftns = left.tv_sec * 1000000000LL + left.tv_nsec;
    int eintr = r == -1 && errno == EINTR;
    int rem_sane = leftns > 0 && leftns < 500LL * 1000 * 1000;

    // Negative nanosecond field -> EINVAL.
    struct timespec bad = {0, -1};
    errno = 0;
    int neg = nanosleep(&bad, NULL) == -1 && errno == EINVAL;

    printf("nanosleep full=%d eintr=%d rem_sane=%d neg=%d\n", full, eintr, rem_sane, neg);
    return 0;
}
