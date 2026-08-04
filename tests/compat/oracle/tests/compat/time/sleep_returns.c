// Return-value contract of the classic sleep helpers. usleep() of a short interval returns 0. An
// uninterrupted sleep() returns 0 (no seconds remaining). sleep() interrupted by a signal returns
// the approximate seconds left. usleep with a huge (>=1e6) argument is still valid on Linux.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static void h(int s) { (void)s; }

int main(void) {
    int u_ok = usleep(50 * 1000) == 0;
    int u_big = usleep(1500 * 1000) == 0; // 1.5s, >= 1e6 us allowed on Linux

    // Uninterrupted sleep(1) returns 0.
    unsigned s_full = sleep(1);
    int full = s_full == 0;

    // Interrupt sleep(10) after ~1s -> returns remaining seconds (a positive, < 10 value).
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGALRM, &sa, NULL);
    alarm(1);
    unsigned left = sleep(10);
    int interrupted = left > 0 && left < 10;

    printf("sleepret u=%d ubig=%d full=%d interrupted=%d\n", u_ok, u_big, full, interrupted);
    return 0;
}
