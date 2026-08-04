// timerfd_gettime reports the interval verbatim and a remaining it_value no larger than the
// armed value (and non-zero right after arming). Disarming clears both fields.
#include <sys/timerfd.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec its = { {0, 20*1000*1000}, {5, 0} };  // 5s value, 20ms interval
    timerfd_settime(tfd, 0, &its, NULL);
    struct itimerspec cur; timerfd_gettime(tfd, &cur);
    int interval_ok = cur.it_interval.tv_sec == 0 && cur.it_interval.tv_nsec == 20*1000*1000;
    int remaining_ok = cur.it_value.tv_sec <= 5 && (cur.it_value.tv_sec > 0 || cur.it_value.tv_nsec > 0);
    struct itimerspec off = { {0,0}, {0,0} };
    timerfd_settime(tfd, 0, &off, NULL);
    struct itimerspec cur2; timerfd_gettime(tfd, &cur2);
    int cleared = cur2.it_value.tv_sec == 0 && cur2.it_value.tv_nsec == 0 &&
                  cur2.it_interval.tv_sec == 0 && cur2.it_interval.tv_nsec == 0;
    printf("gettime interval_ok=%d remaining_ok=%d cleared=%d\n", interval_ok, remaining_ok, cleared);
    return 0;
}
