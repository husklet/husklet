// High half of 64x64 (x86 MUL/MULX/IMUL RDX) and 32x32 widening across signed/unsigned.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t h=0; uint64_t a=0x9E3779B97F4A7C15ULL,b=0xC2B2AE3D27D4EB4FULL;
  for(int i=0;i<40000;i++){
    a+=i; b-=i;
    unsigned __int128 up=(unsigned __int128)a*b; uint64_t uhi=(uint64_t)(up>>64);
    __int128 sp=(__int128)(int64_t)a*(int64_t)b; uint64_t shi=(uint64_t)((unsigned __int128)sp>>64);
    uint64_t w32=(uint64_t)(uint32_t)a*(uint32_t)b;
    int64_t s32=(int64_t)(int32_t)a*(int32_t)b;
    h=h*1000003ULL+uhi; h=h*1000003ULL+shi; h=h*1000003ULL+w32; h=h*1000003ULL+(uint64_t)s32;
  }
  printf("mulhi=%016llx\n",(unsigned long long)h);
  return 0;
}
