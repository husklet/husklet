#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t fires;

static void tick(int signal) {
    (void)signal;
    fires++;
}

static uint64_t monotonic_ns(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_handler = tick;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_RESTART;
    if (sigaction(SIGALRM, &action, NULL) != 0) return 1;

    struct itimerval timer;
    memset(&timer, 0, sizeof timer);
    timer.it_value.tv_usec = 200;
    timer.it_interval.tv_usec = 200;
    if (setitimer(ITIMER_REAL, &timer, NULL) != 0) return 2;

    uint64_t deadline = monotonic_ns() + UINT64_C(5000000000);
    while (fires < 10000 && monotonic_ns() < deadline)
        (void)syscall(SYS_getpid);

    memset(&timer, 0, sizeof timer);
    if (setitimer(ITIMER_REAL, &timer, NULL) != 0) return 3;
    if (fires < 10000) return 4;
    puts("nonpie-signal-fold ok");
    return 0;
}
