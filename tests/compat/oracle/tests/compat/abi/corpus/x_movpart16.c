// ISOLATED CANDIDATE BUG: a 16-bit register MOV (operand-size 0x66, "mov r16,r16") must PRESERVE the
// upper 48 bits of the 64-bit destination. The x86->arm64 engine clobbers/zero-mangles those bits.
// aarch64 golden and qemu-x86_64 agree; only the hl x86 engine diverges. (8/16-bit ALU ops are fine.)
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static uint64_t ins16(uint64_t base,uint16_t v){ uint64_t r=base; __asm__ volatile("mov %1,%w0":"+r"(r):"r"(v)); return r; }
static uint64_t addw(uint64_t base,uint16_t v){ uint64_t r=base; __asm__ volatile("add %1,%w0":"+r"(r):"r"(v):"cc"); return r; }
#else
static uint64_t ins16(uint64_t base,uint16_t v){ return (base & ~0xFFFFULL) | v; }
static uint64_t addw(uint64_t base,uint16_t v){ return (base & ~0xFFFFULL) | (uint16_t)((uint16_t)base + v); }
#endif

TGT int main(void){
  uint64_t h=0, seed=0xDEADC0DE12345678ULL;
  for(int it=0; it<6000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint64_t base=seed*0x9E3779B97F4A7C15ULL;
    h=h*1000003ULL + ins16(base,(uint16_t)seed);
    h=h*1000003ULL + addw(base,(uint16_t)(seed>>19));
  }
  printf("movpart16=%016llx\n",(unsigned long long)h);
  return 0;
}
