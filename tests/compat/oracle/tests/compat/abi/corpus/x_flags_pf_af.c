// Exact PF (parity) and AF (aux-carry) plus CF/OF/SF/ZF after add/sub — the notorious x86 flag bits.
#include <stdio.h>
#include <stdint.h>
static int parity8(unsigned x){ x&=0xff; return (__builtin_popcount(x)&1)==0; } // PF: even parity=1
int main(void){
  uint64_t h=0;
  for(int i=0;i<4000;i++){
    uint32_t a=(uint32_t)(i*2654435761u), b=(uint32_t)(i*40503u+123);
    uint32_t s=a+b; uint32_t d=a-b;
    int cf_add = s<a;
    int cf_sub = a<b;
    int af_add = ((a&0xf)+(b&0xf))>0xf;
    int af_sub = ((a&0xf) - (b&0xf))<0;
    int pf_add = parity8(s);
    int of_add = (~(a^b)&(a^s))>>31;
    int of_sub = ((a^b)&(a^d))>>31;
    int sf=(int)(s>>31), zf=(s==0);
    int f=cf_add|(cf_sub<<1)|(af_add<<2)|(af_sub<<3)|(pf_add<<4)|(of_add<<5)|(of_sub<<6)|(sf<<7)|(zf<<8);
    h=h*1000003ULL+(unsigned)f;
  }
  printf("flags=%016llx\n",(unsigned long long)h);
  return 0;
}
