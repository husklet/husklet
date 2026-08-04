// ISOLATED CANDIDATE BUG: CWD (0x66 0x99) 16-bit sign-extend of AX into DX mistranslates on the
// x86->arm64 engine (DX not filled with the sign of AX). CBW/CWDE/CDQE included for context.
// aarch64 golden and qemu-x86_64 agree; only the hl x86 engine diverges.
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static int16_t cwd_dx(int16_t ax){ int16_t dx; __asm__ volatile("cwd":"=d"(dx):"a"(ax):"cc"); return dx; }
static int16_t cbw_ax(int8_t al){ int16_t ax; __asm__ volatile("cbtw":"=a"(ax):"a"(al)); return ax; }
#else
static int16_t cwd_dx(int16_t ax){ return (int16_t)((ax<0)?-1:0); }
static int16_t cbw_ax(int8_t al){ return (int16_t)al; }
#endif

TGT int main(void){
  uint64_t h=0, seed=0x7F4A7C159E3779B9ULL;
  for(int it=0; it<6000; it++){
    seed = seed*6364136223846793005ULL + 1;
    h=h*1000003ULL + (uint16_t)cwd_dx((int16_t)seed);
    h=h*1000003ULL + (uint16_t)cbw_ax((int8_t)(seed>>7));
  }
  printf("cwd=%016llx\n",(unsigned long long)h);
  return 0;
}
