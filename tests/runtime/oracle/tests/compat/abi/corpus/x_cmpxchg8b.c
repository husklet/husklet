// ISOLATED CANDIDATE BUG: CMPXCHG8B (0F C7 /1) does not update the 64-bit memory operand on the
// x86->arm64 engine (the destination retains its initial value; the ZF/EDX:EAX readback path is also
// affected). aarch64 golden and qemu-x86_64 agree. 32-bit CMPXCHG and XADD translate correctly.
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static int cmpxchg8b(uint64_t*m,uint64_t expect,uint64_t nv,uint64_t*prev){
  uint32_t el=(uint32_t)expect, eh=(uint32_t)(expect>>32), nl=(uint32_t)nv, nh=(uint32_t)(nv>>32);
  uint8_t z;
  __asm__ volatile("cmpxchg8b %1\n\t setz %0":"=q"(z),"+m"(*m),"+a"(el),"+d"(eh):"b"(nl),"c"(nh):"cc");
  *prev=((uint64_t)eh<<32)|el; return z;
}
#else
static int cmpxchg8b(uint64_t*m,uint64_t expect,uint64_t nv,uint64_t*prev){
  uint64_t old=*m; *prev=old; if(old==expect){ *m=nv; return 1; } return 0;
}
#endif

TGT int main(void){
  uint64_t h=0, seed=0xC0FFEE1234ABCDEFULL, cell8=0xAABBCCDD00000000ULL;
  for(int it=0; it<5000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint64_t prev8; int ok8=cmpxchg8b(&cell8,(seed&2)?cell8:seed,seed*3+1,&prev8);
    h=h*1000003ULL + (uint32_t)ok8 + (uint32_t)prev8 + (uint32_t)(prev8>>32) + (uint32_t)cell8;
  }
  printf("cmpxchg8b=%016llx cell8=%016llx\n",(unsigned long long)h,(unsigned long long)cell8);
  return 0;
}
