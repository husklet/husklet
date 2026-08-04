// BSF/BSR/TZCNT/LZCNT/POPCNT over the full input space incl. single-bit and sparse patterns.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t h=1469598103934665603ULL;
  for(int i=0;i<64;i++){
    uint64_t v=(1ULL<<i)| (0xA5A5A5A5A5A5A5A5ULL>>(i&7));
    h ^= (uint64_t)__builtin_ctzll(v); h*=1099511628211ULL;
    h ^= (uint64_t)__builtin_clzll(v); h*=1099511628211ULL;
    h ^= (uint64_t)__builtin_popcountll(v); h*=1099511628211ULL;
    unsigned w=(unsigned)(v^0x12345678u); if(!w) w=1;
    h ^= (uint64_t)__builtin_ctz(w); h*=1099511628211ULL;
    h ^= (uint64_t)__builtin_clz(w); h*=1099511628211ULL;
  }
  printf("scan=%016llx\n",(unsigned long long)h);
  return 0;
}
