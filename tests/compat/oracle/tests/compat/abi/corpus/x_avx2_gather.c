// AVX2 gather: VPGATHERDD, VGATHERDPS, VPGATHERDQ, plus VZEROUPPER interaction after gather.
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
  uint64_t h=0, seed=0x0F1E2D3C4B5A6978ULL;
  uint32_t tab[64]; for(int i=0;i<64;i++) tab[i]=(uint32_t)(i*2654435761u+0x1234);
  uint64_t tabq[64]; for(int i=0;i<64;i++) tabq[i]=(uint64_t)i*0x9E3779B97F4A7C15ULL+7;
  float ftab[64]; for(int i=0;i<64;i++) ftab[i]=(float)(i-32);
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    int32_t idx[8]; for(int i=0;i<8;i++) idx[i]=(int32_t)((seed>>(i*5))&63);
    uint8_t out[32];
#if defined(__x86_64__)
    __m256i vidx=_mm256_loadu_si256((const __m256i*)idx);
    _mm256_storeu_si256((__m256i*)out,_mm256_i32gather_epi32((const int*)tab,vidx,4)); h=mix32(h,out);
    __m256 fg=_mm256_i32gather_ps(ftab,vidx,4); float fo[8]; _mm256_storeu_ps(fo,fg); { uint32_t u[8]; memcpy(u,fo,32); for(int i=0;i<8;i++) h=h*1000003ULL+u[i]; }
    __m128i vidx4=_mm256_castsi256_si128(vidx);
    __m256i qg=_mm256_i32gather_epi64((const long long*)tabq,vidx4,8); _mm256_storeu_si256((__m256i*)out,qg); h=mix32(h,out);
    _mm256_zeroupper();
#else
    { uint32_t r[8]; for(int i=0;i<8;i++) r[i]=tab[idx[i]&63]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    { float r[8]; for(int i=0;i<8;i++) r[i]=ftab[idx[i]&63]; uint32_t u[8]; memcpy(u,r,32); for(int i=0;i<8;i++) h=h*1000003ULL+u[i]; }
    { uint64_t r[4]; for(int i=0;i<4;i++) r[i]=tabq[idx[i]&63]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
#endif
  }
  printf("gather=%016llx\n",(unsigned long long)h);
  return 0;
}
