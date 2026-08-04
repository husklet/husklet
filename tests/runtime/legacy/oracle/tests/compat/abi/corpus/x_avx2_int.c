// AVX2 integer 256-bit: VPADDD/VPSUBD/VPMULLD, VPBROADCASTD, VPERMD, VEXTRACTI128, VINSERTI128, VPUNPCKLDQ.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("avx2")))
#else
#define TGT
#endif

static uint64_t mix32(uint64_t h, const uint8_t *b){ for(int i=0;i<32;i++) h=h*1000003ULL+b[i]; return h; }

TGT int main(void){
  uint64_t h=0, seed=0x1122334455667788ULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint32_t a[8], b[8], idx[8];
    for(int i=0;i<8;i++){ a[i]=(uint32_t)(seed>>(i*4)); b[i]=(uint32_t)(seed>>(i*3+7)); idx[i]=(uint32_t)((seed>>(i*5))&7); }
    uint8_t out[32];
#if defined(__x86_64__)
    __m256i va=_mm256_loadu_si256((const __m256i*)a), vb=_mm256_loadu_si256((const __m256i*)b);
    __m256i vidx=_mm256_loadu_si256((const __m256i*)idx);
    _mm256_storeu_si256((__m256i*)out,_mm256_add_epi32(va,vb));   h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_sub_epi32(va,vb));   h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_mullo_epi32(va,vb)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_broadcastd_epi32(_mm256_castsi256_si128(va))); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_permutevar8x32_epi32(va,vidx)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_unpacklo_epi32(va,vb)); h=mix32(h,out);
    __m128i hi=_mm256_extracti128_si256(vb,1); uint8_t o16[16]; _mm_storeu_si128((__m128i*)o16,hi); for(int i=0;i<16;i++) h=h*1000003ULL+o16[i];
#else
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=a[i]+b[i]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=a[i]-b[i]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=a[i]*b[i]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=a[0]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); } // broadcastd lane0
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=a[idx[i]&7]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); } // permd: full 8-lane cross
    // unpacklo_epi32 per 128-bit lane: lane L uses a[L*4+0],b[L*4+0],a[L*4+1],b[L*4+1]
    { uint32_t r[8]; r[0]=a[0];r[1]=b[0];r[2]=a[1];r[3]=b[1]; r[4]=a[4];r[5]=b[4];r[6]=a[5];r[7]=b[5]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { uint32_t r[4]={b[4],b[5],b[6],b[7]}; uint8_t o16[16]; memcpy(o16,r,16); for(int i=0;i<16;i++) h=h*1000003ULL+o16[i]; } // extracti128 hi of b
#endif
  }
  printf("avx2int=%016llx\n",(unsigned long long)h);
  return 0;
}
