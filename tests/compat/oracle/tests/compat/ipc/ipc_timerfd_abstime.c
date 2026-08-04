// TFD_TIMER_ABSTIME arms against an absolute clock value (now + delta) and fires once.
#include <sys/timerfd.h>
#include <poll.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>
int main(void){
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now);
    struct itimerspec its = { {0,0}, { now.tv_sec, now.tv_nsec } };
    its.it_value.tv_nsec += 10 * 1000 * 1000;
    if (its.it_value.tv_nsec >= 1000000000) { its.it_value.tv_sec++; its.it_value.tv_nsec -= 1000000000; }
    timerfd_settime(tfd, TFD_TIMER_ABSTIME, &its, NULL);
    struct pollfd pf = { .fd = tfd, .events = POLLIN };
    int pn = poll(&pf, 1, 1000);
    uint64_t exp = 0; read(tfd, &exp, 8);
    printf("abstime poll=%d expirations=%lu\n", pn, (unsigned long)exp);   // 1 1
    return 0;
}
