#include <stdio.h>
#include <stdint.h>
static inline uint64_t rd(){
#if defined(__x86_64__)
 unsigned lo,hi; __asm__ volatile("rdtsc":"=a"(lo),"=d"(hi)); return ((uint64_t)hi<<32)|lo;
#elif defined(__aarch64__)
 uint64_t v; __asm__ volatile("mrs %0, cntvct_el0":"=r"(v)); return v;
#endif
}
int main(){uint64_t a=rd(); for(volatile long i=0;i<100000;i++); uint64_t b=rd();
 printf("TSC=%s\n", b>=a?"ok":"no"); return 0;}
