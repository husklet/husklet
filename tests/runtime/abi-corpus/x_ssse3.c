// SSSE3 (0F 38 map): PSHUFB, PHADDD, PMADDUBSW, PSIGNB, PABSD, PALIGNR.
// Arch-neutral: x86 uses tmmintrin.h; aarch64 mirrors exact byte/word semantics in portable C.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <tmmintrin.h>
#define TGT __attribute__((target("ssse3")))
#else
#define TGT
#endif

static uint64_t mix(uint64_t h, const uint8_t *b){ for(int i=0;i<16;i++) h=h*1000003ULL+b[i]; return h; }

TGT int main(void){
  uint64_t h=0, seed=0x123456789ABCDEFULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1442695040888963407ULL;
    uint8_t a[16], b[16];
    for(int i=0;i<16;i++){ a[i]=(uint8_t)(seed>>(i&7)); b[i]=(uint8_t)(seed>>((i*3)&15)); }
    uint8_t out[16];
#if defined(__x86_64__)
    __m128i va=_mm_loadu_si128((const __m128i*)a), vb=_mm_loadu_si128((const __m128i*)b);
    _mm_storeu_si128((__m128i*)out,_mm_shuffle_epi8(va,vb));                 h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_hadd_epi32(va,vb));                   h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_maddubs_epi16(va,vb));                h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_sign_epi8(va,vb));                    h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_abs_epi32(va));                       h=mix(h,out);
    _mm_storeu_si128((__m128i*)out,_mm_alignr_epi8(va,vb,5));                h=mix(h,out);
#else
    // PSHUFB: for each byte j: if b[j]&0x80 -> 0 else out[j]=a[b[j]&0x0F]
    { uint8_t r[16]; for(int j=0;j<16;j++) r[j]=(b[j]&0x80)?0:a[b[j]&0x0F]; h=mix(h,r); }
    // PHADDD: out = [a0+a1, a2+a3, b0+b1, b2+b3] as 32-bit words
    { int32_t ai[4],bi[4]; memcpy(ai,a,16); memcpy(bi,b,16);
      int32_t r[4]={ai[0]+ai[1],ai[2]+ai[3],bi[0]+bi[1],bi[2]+bi[3]}; uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    // PMADDUBSW: word k = sat16( a[2k]*(int8)b[2k] + a[2k+1]*(int8)b[2k+1] ), a unsigned, b signed
    { int16_t r[8]; for(int k=0;k<8;k++){ int p=(int)(uint8_t)a[2*k]*(int)(int8_t)b[2*k] + (int)(uint8_t)a[2*k+1]*(int)(int8_t)b[2*k+1];
        if(p>32767)p=32767; if(p<-32768)p=-32768; r[k]=(int16_t)p; } uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    // PSIGNB: out[j] = b[j]<0 ? -a[j] : (b[j]==0 ? 0 : a[j])  (signed)
    { uint8_t r[16]; for(int j=0;j<16;j++){ int8_t s=(int8_t)b[j]; int8_t av=(int8_t)a[j]; r[j]=(uint8_t)(s<0?-av:(s==0?0:av)); } h=mix(h,r); }
    // PABSD: absolute value of 32-bit lanes of a
    { int32_t ai[4]; memcpy(ai,a,16); uint32_t r[4]; for(int i=0;i<4;i++) r[i]=(uint32_t)(ai[i]<0?-(int64_t)ai[i]:ai[i]); uint8_t rb[16]; memcpy(rb,r,16); h=mix(h,rb); }
    // PALIGNR(va,vb,5): concat va:vb (va high), shift right by 5 bytes, take low 16
    { uint8_t cat[32]; for(int i=0;i<16;i++){ cat[i]=b[i]; cat[16+i]=a[i]; } uint8_t r[16]; for(int i=0;i<16;i++) r[i]=cat[i+5]; h=mix(h,r); }
#endif
  }
  printf("ssse3=%016llx\n",(unsigned long long)h);
  return 0;
}
