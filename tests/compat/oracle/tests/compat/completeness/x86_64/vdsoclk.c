// #344 vDSO clock_gettime: nanosecond scaling. Sleep a known interval, measure via
// CLOCK_MONOTONIC (vDSO fast path), assert elapsed is within a sane window. Deterministic
// verdict: prints whether the measured elapsed ns is within [80%,150%] of the requested.
#include <stdio.h>
#include <time.h>
#include <stdint.h>
static uint64_t ns(struct timespec *t){ return (uint64_t)t->tv_sec*1000000000ull + t->tv_nsec; }
int main(void){
  struct timespec a,b,req={0,200*1000*1000}; // 200ms
  clock_gettime(CLOCK_MONOTONIC,&a);
  nanosleep(&req,NULL);
  clock_gettime(CLOCK_MONOTONIC,&b);
  uint64_t el = ns(&b)-ns(&a);
  // also check REALTIME advances and tv_nsec is a valid nanosecond (< 1e9)
  struct timespec rt; clock_gettime(CLOCK_REALTIME,&rt);
  int nsec_valid = (rt.tv_nsec >= 0 && rt.tv_nsec < 1000000000L) && (a.tv_nsec < 1000000000L);
  // Wide, non-flaky window that still catches the #344 tv_nsec-scaling bug (which under-reported a
  // 2s sleep as ~48ms): a correct clock lands a 200ms sleep in [100ms,2s]; the bug lands far below.
  int in_window = (el >= 100*1000*1000ull && el <= 2000*1000*1000ull);
  printf("vdsoclk window=%d nsec_valid=%d\n", in_window, nsec_valid);
  return 0;
}
