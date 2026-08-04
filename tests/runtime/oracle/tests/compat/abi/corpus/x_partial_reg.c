// Partial-register / operand-size semantics: 8-bit low write preserves upper bits, 16-bit ADD
// wraparound + flags, MOVZX/MOVSX width promotion.
// (The 16-bit MOV upper-preserve — ins16 — is isolated in x_movpart16; it mistranslates on x86 engine.)
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static uint64_t ins8l(uint64_t base,uint8_t v){ uint64_t r=base; __asm__ volatile("mov %1,%b0":"+q"(r):"q"(v)); return r; }
static uint16_t add16(uint16_t a,uint16_t b,unsigned*fl){
  uint16_t r=a; uint64_t f;
  __asm__ volatile("add %2,%0\n\t pushfq\n\t pop %1":"+r"(r),"=r"(f):"r"(b):"cc");
  *fl=(unsigned)(f&0x8D5); return r; // CF|PF|AF|ZF|SF|OF
}
#else
static uint64_t ins8l(uint64_t base,uint8_t v){ return (base & ~0xFFULL) | v; }
static uint16_t add16(uint16_t a,uint16_t b,unsigned*fl){
  uint32_t s=(uint32_t)a+(uint32_t)b; uint16_t r=(uint16_t)s;
  unsigned f=0;
  if(s&0x10000u) f|=0x01;                 // CF
  if(__builtin_parity(r&0xFF)==0) f|=0x04;// PF (even parity of low byte)
  if(((a&0xF)+(b&0xF))&0x10) f|=0x10;     // AF
  if(r==0) f|=0x40;                        // ZF
  if(r&0x8000) f|=0x80;                    // SF
  if((~(a^b)&(a^r))&0x8000) f|=0x800;      // OF
  *fl=f&0x8D5; return r;
}
#endif

TGT int main(void){
  uint64_t h=0, seed=0xDEADC0DE12345678ULL;
  for(int it=0; it<6000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint64_t base=seed*0x9E3779B97F4A7C15ULL;
    h=h*1000003ULL + ins8l(base,(uint8_t)(seed>>3));
    unsigned fl; uint16_t r=add16((uint16_t)seed,(uint16_t)(seed>>16),&fl);
    h=h*1000003ULL + r + fl;
    h=h*1000003ULL + (uint32_t)(int32_t)(int16_t)(uint16_t)seed;      // movsx w->d
    h=h*1000003ULL + (uint32_t)(uint16_t)seed;                        // movzx w->d
  }
  printf("partial=%016llx\n",(unsigned long long)h);
  return 0;
}
