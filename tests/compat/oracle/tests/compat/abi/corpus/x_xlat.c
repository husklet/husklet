// XLATB table-indexed byte load (AL = [RBX+AL]); aarch64 does the identical table lookup.
#include <stdio.h>
#include <stdint.h>
static uint8_t xlat(const uint8_t*tbl,uint8_t idx){
#if defined(__x86_64__)
  uint8_t r;
  __asm__ volatile("xlatb" : "=a"(r) : "0"(idx), "b"(tbl) : );
  return r;
#else
  return tbl[idx];
#endif
}
int main(void){
  uint8_t tbl[256]; for(int i=0;i<256;i++) tbl[i]=(uint8_t)(i*181+37);
  uint64_t h=0; uint8_t idx=0;
  for(int i=0;i<50000;i++){ idx=xlat(tbl,idx); h=h*1000003ULL+idx; }
  printf("xlat=%016llx idx=%u\n",(unsigned long long)h,idx);
  return 0;
}
