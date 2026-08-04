// x87 compare-to-EFLAGS (FUCOMIP) incl. NaN/unordered + FNSTCW/FLDCW control-word round-trip.
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static unsigned fucomi_flags(long double a, long double b){
  uint64_t f;
  // Use memory operands + explicit fldt so GCC does not track the x87 stack (which drifts across
  // many iterations with "t"/"u" constraints). st0=a, st1=b; fucomip pops a, fstp pops b.
  __asm__ volatile("fldt %2\n\t"           // push b -> st0
                   "fldt %1\n\t"           // push a -> st0 (b now st1)
                   "fucomip %%st(1),%%st\n\t"
                   "fstp %%st(0)\n\t"
                   "pushfq\n\t pop %0"
                   : "=r"(f) : "m"(a), "m"(b) : "cc","st","st(1)");
  return (unsigned)(f & 0x45); // ZF(0x40)|PF(0x04)|CF(0x01)
}
static uint16_t cw_roundtrip(uint16_t rc){
  uint16_t cw, back;
  __asm__ volatile("fnstcw %0" : "=m"(cw) :: "memory");
  uint16_t mod=(uint16_t)((cw & ~0x0C00) | ((rc & 3)<<10));
  __asm__ volatile("fldcw %0" :: "m"(mod) : "memory");
  __asm__ volatile("fnstcw %0" : "=m"(back) :: "memory");
  __asm__ volatile("fldcw %0" :: "m"(cw) : "memory"); // restore
  return back;
}
#else
static unsigned fucomi_flags(long double a, long double b){
  unsigned r=0;
  if(a!=a || b!=b){ return 0x45; } // unordered: ZF|PF|CF
  if(a==b) r|=0x40; else if(a<b) r|=0x01;
  return r;
}
static uint16_t cw_roundtrip(uint16_t rc){
  // model default 0x037F control word with rounding bits (10:11) replaced
  uint16_t cw=0x037F;
  return (uint16_t)((cw & ~0x0C00) | ((rc & 3)<<10));
}
#endif

TGT int main(void){
  uint64_t h=0, seed=0x243F6A8885A308D3ULL;
  for(int it=0; it<4000; it++){
    seed = seed*6364136223846793005ULL + 1;
    long double a=(long double)((int)((seed>>3)&0x3FF)-512)/4.0L;
    long double b=(long double)((int)((seed>>17)&0x3FF)-512)/4.0L;
    if((seed&0x1F)==0) a=a/0.0L*0.0L;      // produce NaN occasionally (0/0)
    if((seed&0x3F)==7) b=a;                 // exact-equal case
    h=h*1000003ULL + fucomi_flags(a,b);
    h=h*1000003ULL + cw_roundtrip((uint16_t)(seed&3));
  }
  printf("x87cmp=%016llx\n",(unsigned long long)h);
  return 0;
}
