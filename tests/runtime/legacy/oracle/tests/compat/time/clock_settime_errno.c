// CANDIDATE ENGINE BUG: clock_settime on a garbage clock id, or on the read-only CLOCK_MONOTONIC,
// must fail with EINVAL (id/validity checked before privilege). The engine returns EPERM instead,
// i.e. it runs the CAP_SYS_TIME check ahead of clock-id validation.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <time.h>
int main(void){
    struct timespec v={1000000,0}; errno=0;
    int set_bad = clock_settime((clockid_t)0x7fff,&v)==-1 && errno==EINVAL;
    errno=0;
    int set_mono = clock_settime(CLOCK_MONOTONIC,&v)==-1 && errno==EINVAL;
    printf("settimeerrno set_bad=%d set_mono=%d\n", set_bad, set_mono);
    return 0;
}
