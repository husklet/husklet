// SETcc across all 16 conditions derived from add/sub flag results, packed into a checksum.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t h=0;
  for(int i=0;i<2000;i++){
    int64_t a=(int64_t)(i*2654435761u) - 0x40000000; int64_t b=(int64_t)(i*40503u) - 0x20000000;
    uint64_t ua=(uint64_t)a, ub=(uint64_t)b;
    int f=0;
    f|=(a==b)<<0; f|=(a!=b)<<1; f|=(ua<ub)<<2; f|=(ua<=ub)<<3; f|=(ua>ub)<<4; f|=(ua>=ub)<<5;
    f|=(a<b)<<6; f|=(a<=b)<<7; f|=(a>b)<<8; f|=(a>=b)<<9;
    f|=(a<0)<<10; f|=(a>0)<<11; f|=(a==0)<<12;
    int64_t s; f|=__builtin_add_overflow(a,b,&s)<<13; f|=__builtin_sub_overflow(a,b,&s)<<14;
    f|=(__builtin_popcountll(ua)&1)<<15;
    h=h*1000003ULL + (unsigned)f;
  }
  printf("setcc=%016llx\n",(unsigned long long)h);
  return 0;
}
