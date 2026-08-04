// MOVBE (0F 38 F0/F1): byte-swapping load/store, 16/32/64-bit. Plus BSWAP for cross-check.
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static uint32_t movbe32(const void*p){ uint32_t v; __asm__ volatile("movbe %1,%0":"=r"(v):"m"(*(const uint32_t*)p)); return v; }
static uint64_t movbe64(const void*p){ uint64_t v; __asm__ volatile("movbe %1,%0":"=r"(v):"m"(*(const uint64_t*)p)); return v; }
static uint16_t movbe16(const void*p){ uint16_t v; __asm__ volatile("movbe %1,%0":"=r"(v):"m"(*(const uint16_t*)p)); return v; }
static void movbe32s(void*p,uint32_t v){ __asm__ volatile("movbe %1,%0":"=m"(*(uint32_t*)p):"r"(v)); }
#else
static uint32_t movbe32(const void*p){ uint32_t v; __builtin_memcpy(&v,p,4); return __builtin_bswap32(v); }
static uint64_t movbe64(const void*p){ uint64_t v; __builtin_memcpy(&v,p,8); return __builtin_bswap64(v); }
static uint16_t movbe16(const void*p){ uint16_t v; __builtin_memcpy(&v,p,2); return __builtin_bswap16(v); }
static void movbe32s(void*p,uint32_t v){ uint32_t s=__builtin_bswap32(v); __builtin_memcpy(p,&s,4); }
#endif

TGT int main(void){
  uint64_t h=0, seed=0x0706050403020100ULL;
  for(int it=0; it<5000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint8_t buf[8]; __builtin_memcpy(buf,&seed,8);
    h=h*1000003ULL + movbe16(buf);
    h=h*1000003ULL + movbe32(buf);
    h=h*1000003ULL + movbe64(buf);
    uint8_t ob[4]; movbe32s(ob,(uint32_t)seed); uint32_t rr; __builtin_memcpy(&rr,ob,4); h=h*1000003ULL+rr;
  }
  printf("movbe=%016llx\n",(unsigned long long)h);
  return 0;
}
