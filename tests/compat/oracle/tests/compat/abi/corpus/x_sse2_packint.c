// SSE2 packed-integer ops (PADDB/PSUBW/PMULLW/PADDUSB/PMINUB/PMAXSW/PCMPEQB/PCMPGTB + PMOVMSKB).
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <emmintrin.h>
#endif
static void ld(uint8_t*d,uint64_t*s,int n){ for(int i=0;i<n;i++) d[i]=(uint8_t)(*s>>((i&7)*8)^(i*61)); }
int main(void){
  uint64_t h=0,seed=0x123456789ABCDEFULL;
  for(int it=0;it<2000;it++){
    uint8_t a[16],b[16],o[16]; seed=seed*6364136223846793005ULL+1; ld(a,&seed,16); seed=seed*6364136223846793005ULL+1; ld(b,&seed,16);
    unsigned eqm,gtm;
#if defined(__x86_64__)
    __m128i va=_mm_loadu_si128((const __m128i*)a),vb=_mm_loadu_si128((const __m128i*)b);
    _mm_storeu_si128((__m128i*)o,_mm_add_epi8(va,vb));                for(int i=0;i<16;i++)h=h*131+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_sub_epi16(va,vb));               for(int i=0;i<16;i++)h=h*131+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_mullo_epi16(va,vb));             for(int i=0;i<16;i++)h=h*131+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_adds_epu8(va,vb));               for(int i=0;i<16;i++)h=h*131+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_min_epu8(va,vb));                for(int i=0;i<16;i++)h=h*131+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_max_epi16(va,vb));               for(int i=0;i<16;i++)h=h*131+o[i];
    eqm=(unsigned)_mm_movemask_epi8(_mm_cmpeq_epi8(va,vb));
    gtm=(unsigned)_mm_movemask_epi8(_mm_cmpgt_epi8(va,vb));
#else
    for(int i=0;i<16;i++)h=h*131+(uint8_t)(a[i]+b[i]);
    for(int i=0;i<16;i+=2){uint16_t x=(a[i]|(a[i+1]<<8))-(b[i]|(b[i+1]<<8)); h=h*131+(uint8_t)x; h=h*131+(uint8_t)(x>>8);}
    for(int i=0;i<16;i+=2){uint16_t x=(uint16_t)((a[i]|(a[i+1]<<8))*(b[i]|(b[i+1]<<8))); h=h*131+(uint8_t)x; h=h*131+(uint8_t)(x>>8);}
    for(int i=0;i<16;i++){unsigned s=a[i]+b[i]; h=h*131+(uint8_t)(s>255?255:s);}
    for(int i=0;i<16;i++)h=h*131+(a[i]<b[i]?a[i]:b[i]);
    for(int i=0;i<16;i+=2){int16_t x=(int16_t)(a[i]|(a[i+1]<<8)),y=(int16_t)(b[i]|(b[i+1]<<8)); int16_t m=x>y?x:y; h=h*131+(uint8_t)m; h=h*131+(uint8_t)((uint16_t)m>>8);}
    eqm=0;gtm=0; for(int i=0;i<16;i++){ if(a[i]==b[i])eqm|=1u<<i; if((int8_t)a[i]>(int8_t)b[i])gtm|=1u<<i; }
#endif
    h=h*1000003ULL+eqm; h=h*1000003ULL+gtm;
  }
  printf("packint=%016llx\n",(unsigned long long)h);
  return 0;
}
