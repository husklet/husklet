// AVX2 variable shifts + movemask: VPSLLVD/VPSRLVD/VPSRAVD/VPSLLVQ, VPMOVMSKB (256), VPSHUFD, VPSRLDQ.
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
  uint64_t h=0, seed=0xFEDCBA9876543210ULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint32_t a[8], sh[8]; uint64_t aq[4], shq[4];
    for(int i=0;i<8;i++){ a[i]=(uint32_t)(seed>>(i*3)); sh[i]=(uint32_t)((seed>>(i*4))&0x3F); }
    for(int i=0;i<4;i++){ aq[i]=seed*(i+1)+0x9E3779B9ULL; shq[i]=(seed>>(i*7))&0x7F; }
    uint8_t out[32];
#if defined(__x86_64__)
    __m256i va=_mm256_loadu_si256((const __m256i*)a), vsh=_mm256_loadu_si256((const __m256i*)sh);
    __m256i vaq=_mm256_loadu_si256((const __m256i*)aq), vshq=_mm256_loadu_si256((const __m256i*)shq);
    _mm256_storeu_si256((__m256i*)out,_mm256_sllv_epi32(va,vsh)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_srlv_epi32(va,vsh)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_srav_epi32(va,vsh)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_sllv_epi64(vaq,vshq)); h=mix32(h,out);
    h=h*1000003ULL + (uint32_t)_mm256_movemask_epi8(va);
    _mm256_storeu_si256((__m256i*)out,_mm256_shuffle_epi32(va,0x1B)); h=mix32(h,out);
#else
    // sllv_epi32: shift>=32 -> 0
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=(sh[i]>=32)?0:(a[i]<<sh[i]); uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=(sh[i]>=32)?0:(a[i]>>sh[i]); uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { uint32_t r[8]; for(int i=0;i<8;i++){ int32_t s=(int32_t)a[i]; uint32_t c=sh[i]; r[i]=(uint32_t)((c>=32)?(s>>31):(s>>c)); } uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { uint64_t r[4]; for(int i=0;i<4;i++) r[i]=(shq[i]>=64)?0:(aq[i]<<shq[i]); uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    // movemask_epi8: bit i = sign bit of byte i
    { uint8_t ab[32]; memcpy(ab,a,32); uint32_t m=0; for(int i=0;i<32;i++) if(ab[i]&0x80) m|=(1u<<i); h=h*1000003ULL+m; }
    // shuffle_epi32 imm 0x1B per-128 lane: sel per lane = imm bits; 0x1B=00011011 -> 3,2,1,0 (reverse)
    { uint32_t r[8]; for(int L=0;L<2;L++){ const uint32_t*al=a+L*4; uint32_t*rl=r+L*4; int imm=0x1B; for(int j=0;j<4;j++) rl[j]=al[(imm>>(2*j))&3]; } uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
#endif
  }
  printf("varshift=%016llx\n",(unsigned long long)h);
  return 0;
}
