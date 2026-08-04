// BSWAP / MOVBE-style byte reversal at 16/32/64 and mixed endian round-trips.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t h=0; uint64_t v=0x0102030405060708ULL;
  for(int i=0;i<20000;i++){
    v = v*6364136223846793005ULL + 1442695040888963407ULL;
    uint16_t s=__builtin_bswap16((uint16_t)v);
    uint32_t l=__builtin_bswap32((uint32_t)v);
    uint64_t q=__builtin_bswap64(v);
    h=h*1000003ULL + s; h=h*1000003ULL + l; h=h*1000003ULL + q;
  }
  printf("bswap=%016llx\n",(unsigned long long)h);
  return 0;
}
