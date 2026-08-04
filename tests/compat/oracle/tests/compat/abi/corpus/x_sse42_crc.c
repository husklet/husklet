// SSE4.2 CRC32 (b/w/d/q accumulators) + POPCNT. Deterministic derived checksums.
#include <stdio.h>
#include <stdint.h>
#if defined(__x86_64__)
#include <nmmintrin.h>
#define TGT __attribute__((target("sse4.2,popcnt")))
#else
#define TGT
#endif

// portable CRC32C (Castagnoli, polynomial 0x11EDC6F41 reflected -> 0x82F63B78) — matches x86 CRC32 insn.
static uint32_t crc32c_byte(uint32_t crc, uint8_t v){
  crc ^= v;
  for(int i=0;i<8;i++) crc = (crc>>1) ^ (0x82F63B78u & (uint32_t)(-(int32_t)(crc&1)));
  return crc;
}
static uint32_t crc32c_u16(uint32_t c, uint16_t v){ c=crc32c_byte(c,(uint8_t)v); c=crc32c_byte(c,(uint8_t)(v>>8)); return c; }
static uint32_t crc32c_u32(uint32_t c, uint32_t v){ for(int i=0;i<4;i++) c=crc32c_byte(c,(uint8_t)(v>>(i*8))); return c; }
static uint64_t crc32c_u64(uint64_t c, uint64_t v){ for(int i=0;i<8;i++) c=crc32c_byte((uint32_t)c,(uint8_t)(v>>(i*8))); return c; }

TGT int main(void){
  uint64_t h=0, seed=0x0123456789ABCDEFULL;
  uint32_t cb=0xFFFFFFFF, cw=0x1234, cd=0xABCDEF01; uint64_t cq=0xDEADBEEFCAFE0000ULL;
  for(int it=0; it<5000; it++){
    seed = seed*6364136223846793005ULL + 1;
#if defined(__x86_64__)
    cb=_mm_crc32_u8(cb,(uint8_t)seed);
    cw=_mm_crc32_u16(cw,(uint16_t)(seed>>7));
    cd=_mm_crc32_u32(cd,(uint32_t)(seed>>13));
    cq=_mm_crc32_u64(cq,(uint64_t)(seed*2654435761ULL));
    h=h*1000003ULL + (uint32_t)__builtin_popcountll(seed) + (uint32_t)__builtin_popcount((uint32_t)seed);
#else
    cb=crc32c_byte(cb,(uint8_t)seed);
    cw=crc32c_u16(cw,(uint16_t)(seed>>7));
    cd=crc32c_u32(cd,(uint32_t)(seed>>13));
    cq=crc32c_u64(cq,(uint64_t)(seed*2654435761ULL));
    h=h*1000003ULL + (uint32_t)__builtin_popcountll(seed) + (uint32_t)__builtin_popcount((uint32_t)seed);
#endif
    h=h*1000003ULL + cb; h=h*1000003ULL + cw; h=h*1000003ULL + cd; h=h*1000003ULL + (uint32_t)cq + (uint32_t)(cq>>32);
  }
  printf("crc=%016llx cb=%08x cw=%08x cd=%08x cq=%016llx\n",(unsigned long long)h,cb,cw,cd,(unsigned long long)cq);
  return 0;
}
