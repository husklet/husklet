// MOVSX/MOVZX and mixed-width integer promotion (8/16/32->64 signed & unsigned) around arithmetic.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t h=0; uint64_t seed=0x9E3779B97F4A7C15ULL;
  for(int i=0;i<30000;i++){
    seed=seed*6364136223846793005ULL+1;
    int8_t  s8=(int8_t)seed;   uint8_t  u8=(uint8_t)seed;
    int16_t s16=(int16_t)seed; uint16_t u16=(uint16_t)seed;
    int32_t s32=(int32_t)seed; uint32_t u32=(uint32_t)seed;
    int64_t a = (int64_t)s8 + (int64_t)s16 + (int64_t)s32;
    uint64_t b = (uint64_t)u8 + (uint64_t)u16 + (uint64_t)u32;
    int64_t c = (int64_t)s8 * (int64_t)s16;
    uint64_t d = (uint64_t)u16 * (uint64_t)u32;
    int64_t e = ((int64_t)s32 << 8) >> 4;   // arithmetic
    h=h*1000003ULL+(uint64_t)a; h=h*1000003ULL+b; h=h*1000003ULL+(uint64_t)c; h=h*1000003ULL+d; h=h*1000003ULL+(uint64_t)e;
  }
  printf("sext=%016llx\n",(unsigned long long)h);
  return 0;
}
