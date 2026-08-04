// Rotate-through-carry (x86 RCL/RCR): 65-bit rotation modeled portably; stresses carry-in/out plumbing.
#include <stdio.h>
#include <stdint.h>
// rotate {carry:value} left by 1 (33-bit rotate for 32-bit value)
static uint32_t rcl1(uint32_t v,unsigned*c){ unsigned nc=v>>31; uint32_t r=(v<<1)|(*c&1u); *c=nc; return r; }
static uint32_t rcr1(uint32_t v,unsigned*c){ unsigned nc=v&1u; uint32_t r=(v>>1)|((*c&1u)<<31); *c=nc; return r; }
int main(void){
  uint64_t h=0; unsigned c=1; uint32_t v=0x8000000Fu;
  for(int i=0;i<3000;i++){ v=rcl1(v,&c); h=h*1000003ULL+v+c; }
  for(int i=0;i<3000;i++){ v=rcr1(v,&c); h=h*1000003ULL+v+c; }
  printf("rc=%016llx v=%08x c=%u\n",(unsigned long long)h,v,c);
  return 0;
}
