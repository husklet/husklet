// A periodic timerfd accumulates expirations while unread; after a bounded sleep the read count
// is at least 1 and the fd disarms once value+interval are cleared. Count printed as booleans
// to stay arch-neutral and timing-tolerant.
#include <sys/timerfd.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>
int main(void){
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec its = { {0, 5*1000*1000}, {0, 5*1000*1000} };  // 5ms interval
    timerfd_settime(tfd, 0, &its, NULL);
    struct timespec s = {0, 60*1000*1000};  // 60ms
    nanosleep(&s, NULL);
    uint64_t exp = 0; read(tfd, &exp, 8);
    // disarm
    struct itimerspec off = { {0,0}, {0,0} };
    timerfd_settime(tfd, 0, &off, NULL);
    struct itimerspec cur; timerfd_gettime(tfd, &cur);
    int disarmed = cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec == 0;
    printf("periodic accumulated=%d disarmed=%d\n", exp >= 1, disarmed);  // 1 1
    return 0;
}
