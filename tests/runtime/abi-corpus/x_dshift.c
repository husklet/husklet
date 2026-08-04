// Double-precision funnel shifts (x86 SHLD/SHRD codegen): 128-bit funnel across a 64-bit boundary.
#include <stdio.h>
#include <stdint.h>
static uint64_t shld(uint64_t hi,uint64_t lo,unsigned n){ n&=63; if(!n) return hi; return (hi<<n)|(lo>>(64-n)); }
static uint64_t shrd(uint64_t lo,uint64_t hi,unsigned n){ n&=63; if(!n) return lo; return (lo>>n)|(hi<<(64-n)); }
int main(void){
  uint64_t a=0x0123456789ABCDEFULL,b=0xFEDCBA9876543210ULL,accL=1,accR=2;
  for(unsigned n=0;n<128;n++){ accL=accL*1000003ULL + shld(a^accL,b+n,n); accR=accR*1000003ULL + shrd(a+n,b^accR,n); }
  printf("shld=%016llx shrd=%016llx\n",(unsigned long long)accL,(unsigned long long)accR);
  return 0;
}
