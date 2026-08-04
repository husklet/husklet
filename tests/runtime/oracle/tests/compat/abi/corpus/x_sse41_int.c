// SSE4.1 integer: PMULLD, PMOVSXBW, PMOVZXWD, PMINSD, PMAXUD, PACKUSDW, PBLENDW.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <smmintrin.h>
#define TGT __attribute__((target("sse4.1")))
#else
#define TGT
#endif

static uint64_t mix(uint64_t h, const uint8_t *b){ for(int i=0;i<16;i++) h=h*1000003ULL+b[i]; return h; }
static int32_t sat_u16(int32_t v){ if(v<0)return 0; if(v>65535)return 65535; return v; }

TGT int main(void){
  uint64_t h=0, seed=0x2468ACE013579BDULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    int32_t a[4], b[4];
    for(int i=0;i<4;i++){ a[i]=(int32_t)(seed>>(i*8)); b[i]=(int32_t)(seed>>(i*5+11)); }
    uint8_t ab[16], bb[16]; memcpy(ab,a,16); memcpy(bb,b,16);
    uint8_t out[16];
#if defined(__x86_64__)
    __m128i va=_mm_loadu_si128((const __m128i*)ab), vb=_mm_loadu_si128((const __m128i*)bb);
    _mm_storeu_si128((__m128i*)out,_mm_mullo_epi32(va,vb));  h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_cvtepi8_epi16(va));   h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_cvtepu16_epi32(va));  h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_min_epi32(va,vb));    h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_max_epu32(va,vb));    h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_packus_epi32(va,vb)); h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_blend_epi16(va,vb,0xB3)); h=mix(h,out);
#else
    { uint32_t r[4]; for(int i=0;i<4;i++) r[i]=(uint32_t)((int32_t)((uint32_t)a[i]*(uint32_t)b[i])); uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    // pmovsxbw: sign-extend low 8 bytes to 8 words
    { int16_t r[8]; for(int i=0;i<8;i++) r[i]=(int16_t)(int8_t)ab[i]; uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    // pmovzxwd: zero-extend low 4 words to 4 dwords
    { uint16_t w[8]; memcpy(w,ab,16); uint32_t r[4]; for(int i=0;i<4;i++) r[i]=w[i]; uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    { int32_t r[4]; for(int i=0;i<4;i++) r[i]=a[i]<b[i]?a[i]:b[i]; uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    { uint32_t r[4]; for(int i=0;i<4;i++){ uint32_t ua=(uint32_t)a[i],ub=(uint32_t)b[i]; r[i]=ua>ub?ua:ub; } uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    // packusdw: a[0..3],b[0..3] -> u16 saturated
    { uint16_t r[8]; for(int i=0;i<4;i++) r[i]=(uint16_t)sat_u16(a[i]); for(int i=0;i<4;i++) r[4+i]=(uint16_t)sat_u16(b[i]); uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    // blend_epi16 mask 0xB3: bit set -> take b word
    { uint16_t aw[8],bw[8],r[8]; memcpy(aw,ab,16); memcpy(bw,bb,16); int m=0xB3; for(int i=0;i<8;i++) r[i]=(m>>i&1)?bw[i]:aw[i]; uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
#endif
  }
  printf("sse41int=%016llx\n",(unsigned long long)h);
  return 0;
}
