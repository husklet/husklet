// F16C half-precision conversions: VCVTPS2PH / VCVTPH2PS (round-trip + rounding modes).
// aarch64 uses __fp16 hardware conversion; golden byte-identical.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("f16c,avx")))
#else
#define TGT
#endif
static uint64_t mix(uint64_t h, uint64_t v){ return h*1000003ULL+v; }
static uint32_t b32(float f){ uint32_t u; memcpy(&u,&f,4); return u; }

TGT int main(void){
  uint64_t h=0;
  float in[8];
  for(int t=0;t<32;t++){
    for(int i=0;i<8;i++) in[i]=(float)((t*8+i)*0.13750f - 3.0f) * ((i&1)?-1.0f:1.0f) + (float)t*17.0f;
    uint16_t half[8];
#if defined(__x86_64__)
    __m256 v=_mm256_loadu_ps(in);
    __m128i hh=_mm256_cvtps_ph(v, _MM_FROUND_TO_NEAREST_INT|_MM_FROUND_NO_EXC);
    _mm_storeu_si128((__m128i*)half, hh);
    __m256 back=_mm256_cvtph_ps(hh);
    float outf[8]; _mm256_storeu_ps(outf, back);
#else
    float outf[8];
    for(int i=0;i<8;i++){ __fp16 x=(__fp16)in[i]; uint16_t bits; memcpy(&bits,&x,2); half[i]=bits;
      __fp16 y; memcpy(&y,&bits,2); outf[i]=(float)y; }
#endif
    for(int i=0;i<8;i++){ h=mix(h,half[i]); h=mix(h,b32(outf[i])); }
  }
  printf("f16c=%016llx\n",(unsigned long long)h);
  return 0;
}
