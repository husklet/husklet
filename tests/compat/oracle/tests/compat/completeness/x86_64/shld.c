#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <immintrin.h>
#include <x86intrin.h>
int main(void){
  unsigned long hi=0x1234567890ABCDEFUL, lo=0xFEDCBA0987654321UL, r1=hi, r2=lo;
  __asm__("shld $12,%1,%0":"+r"(r1):"r"(lo));
  __asm__("shrd $12,%1,%0":"+r"(r2):"r"(hi));
  unsigned long acc = (r1 ^ r2) & 0xffffff;
  // 16-bit SHLD/SHRD (immediate + by CL) -- the double-shift wraps within the 16-bit operand.
  unsigned short a=0x1234,b=0xABCD;
  __asm__("shldw $4,%1,%0":"+r"(a):"r"(b)); acc += (unsigned)a*3;
  unsigned short c=0x8001,d=0x4002;
  __asm__("shrdw $3,%1,%0":"+r"(c):"r"(d)); acc += (unsigned)c*7;
  unsigned short e=0xF0F0,f=0x0F0F; unsigned char cl=5;
  __asm__("shldw %%cl,%1,%0":"+r"(e):"r"(f),"c"(cl)); acc += (unsigned)e*13;
  // 32-bit SHLD/SHRD by CL
  unsigned int g=0x89ABCDEF,h=0x01234567; cl=9;
  __asm__("shldl %%cl,%1,%0":"+r"(g):"r"(h),"c"(cl)); acc += g;
  printf("shld r=%lu\n", acc); return 0; }
