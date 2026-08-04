// Shift-count masking semantics (x86 masks shift count to width-1) and arithmetic vs logical shifts.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t h=0;
  for(unsigned n=0;n<70;n++){
    uint32_t u32=0x80000001u; int32_t  s32=(int32_t)0x80000001;
    uint64_t u64=0x8000000000000001ULL; int64_t s64=(int64_t)0x8000000000000001ULL;
    h=h*1000003ULL+ (u32<<n); h=h*1000003ULL+ (u32>>n);
    h=h*1000003ULL+ (uint32_t)(s32>>n);
    h=h*1000003ULL+ (u64<<n); h=h*1000003ULL+ (u64>>n);
    h=h*1000003ULL+ (uint64_t)(s64>>n);
  }
  printf("shift=%016llx\n",(unsigned long long)h);
  return 0;
}
