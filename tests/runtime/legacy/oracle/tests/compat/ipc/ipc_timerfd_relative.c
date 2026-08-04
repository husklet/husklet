// A relative single-shot timerfd delivers exactly one expiration; the 8-byte read yields count 1.
#include <sys/timerfd.h>
#include <poll.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
int main(void){
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec its = { {0,0}, {0, 10 * 1000 * 1000} };  // 10ms one-shot
    timerfd_settime(tfd, 0, &its, NULL);
    struct pollfd pf = { .fd = tfd, .events = POLLIN };
    int pn = poll(&pf, 1, 1000);
    uint64_t exp = 0; read(tfd, &exp, 8);
    printf("relative poll=%d expirations=%lu\n", pn, (unsigned long)exp);  // 1 1
    return 0;
}
