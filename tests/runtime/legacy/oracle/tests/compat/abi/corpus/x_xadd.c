// XADD, CMPXCHG (32-bit) — read/modify semantics + ZF result.
// (CMPXCHG8B is isolated in x_cmpxchg8b — it never updates memory on the x86 engine.)
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static uint32_t xadd32(uint32_t*m,uint32_t v){ __asm__ volatile("xadd %0,%1":"+r"(v),"+m"(*m)::"cc"); return v; }
static int cmpxchg32(uint32_t*m,uint32_t expect,uint32_t nv,uint32_t*prev){
  uint8_t z; uint32_t old=expect;
  __asm__ volatile("cmpxchg %3,%1\n\t sete %0":"=q"(z),"+m"(*m),"+a"(old):"r"(nv):"cc");
  *prev=old; return z;
}
#else
static uint32_t xadd32(uint32_t*m,uint32_t v){ uint32_t old=*m; *m=old+v; return old; }
static int cmpxchg32(uint32_t*m,uint32_t expect,uint32_t nv,uint32_t*prev){
  uint32_t old=*m; *prev=old; if(old==expect){ *m=nv; return 1; } return 0;
}
#endif

TGT int main(void){
  uint64_t h=0, seed=0xC0FFEE1234ABCDEFULL;
  uint32_t cell=0x1000;
  for(int it=0; it<5000; it++){
    seed = seed*6364136223846793005ULL + 1;
    h=h*1000003ULL + xadd32(&cell,(uint32_t)seed);
    h=h*1000003ULL + cell;
    uint32_t prev; int ok=cmpxchg32(&cell,(uint32_t)(seed&1?cell:seed),(uint32_t)(seed>>7),&prev);
    h=h*1000003ULL + (uint32_t)ok + prev + cell;
  }
  printf("xadd=%016llx cell=%08x\n",(unsigned long long)h,cell);
  return 0;
}
