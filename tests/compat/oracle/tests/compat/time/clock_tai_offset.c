// CANDIDATE ENGINE BUG: CLOCK_TAI must track the wall clock (== CLOCK_REALTIME plus the ~37s TAI-UTC
// leap-second offset). On the engine it instead returns the MONOTONIC-since-boot timeline, so
// |CLOCK_TAI - CLOCK_REALTIME| is astronomically large. Native: within a couple of minutes.
#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
static long long ns(clockid_t c){struct timespec t; if(clock_gettime(c,&t))return -1; return t.tv_sec*1000000000LL+t.tv_nsec;}
int main(void){
    long long tai=ns(CLOCK_TAI), real=ns(CLOCK_REALTIME);
    long long off = tai>real? tai-real : real-tai;
    int bounded = off < 120LL*1000*1000*1000; // TAI within 120s of REALTIME
    printf("taioffset bounded=%d\n", bounded);
    return 0;
}
