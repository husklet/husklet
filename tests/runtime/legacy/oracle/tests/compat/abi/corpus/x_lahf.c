// LAHF flag-byte load after ADD (SF/ZF/AF/PF/CF into AH). aarch64 computes the identical flag byte.
#include <stdio.h>
#include <stdint.h>
static unsigned flagbyte_add(uint64_t a,uint64_t b){
#if defined(__x86_64__)
  unsigned char ah;
  __asm__ volatile("addq %2, %1\n\tlahf\n\tmovb %%ah, %0\n\t"
                   : "=r"(ah), "+r"(a) : "r"(b) : "cc","rax");
  return ah & 0xD5u; // CF(0) PF(2) AF(4) ZF(6) SF(7)
#else
  uint64_t s=a+b;
  unsigned cf = (s<a);
  unsigned pf = ((__builtin_popcount((unsigned)(s&0xff))&1)==0);
  unsigned af = (((a&0xf)+(b&0xf))>0xf);
  unsigned zf = (s==0);
  unsigned sf = (unsigned)(s>>63);
  return (cf<<0)|(pf<<2)|(af<<4)|(zf<<6)|(sf<<7);
#endif
}
int main(void){
  uint64_t h=0;
  for(int i=0;i<20000;i++){
    uint64_t a=(uint64_t)i*0x9E3779B97F4A7C15ULL, b=(uint64_t)(20000-i)*0xC2B2AE3D27D4EB4FULL;
    h=h*1000003ULL+flagbyte_add(a,b);
    h=h*1000003ULL+flagbyte_add(a, (uint64_t)0-a);   // exact zero -> ZF,AF-boundary
    h=h*1000003ULL+flagbyte_add(0x0f0f0f0f0f0f0f0fULL, 0x0101010101010101ULL);
  }
  printf("lahf=%016llx\n",(unsigned long long)h);
  return 0;
}
