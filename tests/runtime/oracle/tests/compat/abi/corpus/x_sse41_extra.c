// SSE4.1 corners not covered elsewhere: MPSADBW (sum-of-abs-diff), PHMINPOSUW (horizontal min),
// DPPD (packed-double dot product). aarch64 uses scalar references; golden byte-identical.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("sse4.1")))
#else
#define TGT
#endif
static uint64_t mix(uint64_t h, uint64_t v){ return h*1000003ULL+v; }
static uint64_t b64(double d){ uint64_t u; memcpy(&u,&d,8); return u; }

#if !defined(__x86_64__)
static void mpsadbw(uint16_t*r,const uint8_t*a,const uint8_t*b,int imm){
  int boff=(imm&3)*4, aoff=((imm>>2)&1)*4;
  for(int i=0;i<8;i++){ int s=0; for(int k=0;k<4;k++){ int d=(int)a[aoff+i+k]-(int)b[boff+k]; s+=d<0?-d:d; } r[i]=(uint16_t)s; }
}
static void phminpos(uint16_t*r,const uint16_t*a){
  uint16_t m=a[0]; int idx=0; for(int i=1;i<8;i++) if(a[i]<m){ m=a[i]; idx=i; }
  memset(r,0,16); r[0]=m; r[1]=(uint16_t)idx;
}
static double dppd(const double*a,const double*b,int imm){
  double s=0; for(int i=0;i<2;i++) if(imm&(0x10<<i)) s+=a[i]*b[i];
  return s; // placed into lanes selected by low bits below
}
#endif

TGT int main(void){
  uint64_t h=0;
  uint8_t A[16], B[16];
  for(int i=0;i<16;i++){ A[i]=(uint8_t)(i*13+5); B[i]=(uint8_t)(i*7+2); }
  static const int mimm[]={0,1,2,4,5,7};
  for(int m=0;m<6;m++){
    uint16_t r[8];
#if defined(__x86_64__)
    __m128i va=_mm_loadu_si128((const __m128i*)A), vb=_mm_loadu_si128((const __m128i*)B);
    __m128i rr;
    switch(mimm[m]){case 0:rr=_mm_mpsadbw_epu8(va,vb,0);break;case 1:rr=_mm_mpsadbw_epu8(va,vb,1);break;
      case 2:rr=_mm_mpsadbw_epu8(va,vb,2);break;case 4:rr=_mm_mpsadbw_epu8(va,vb,4);break;
      case 5:rr=_mm_mpsadbw_epu8(va,vb,5);break;default:rr=_mm_mpsadbw_epu8(va,vb,7);}
    _mm_storeu_si128((__m128i*)r,rr);
#else
    mpsadbw(r,A,B,mimm[m]);
#endif
    for(int i=0;i<8;i++) h=mix(h,r[i]);
  }
  // PHMINPOSUW
  for(int t=0;t<8;t++){
    uint16_t W[8], r[8];
    for(int i=0;i<8;i++) W[i]=(uint16_t)((i*2027+t*911)%65521);
#if defined(__x86_64__)
    __m128i vw=_mm_loadu_si128((const __m128i*)W);
    __m128i rr=_mm_minpos_epu16(vw); _mm_storeu_si128((__m128i*)r,rr);
#else
    phminpos(r,W);
#endif
    for(int i=0;i<8;i++) h=mix(h,r[i]);
  }
  // DPPD
  double DA[2]={3.5,-2.25}, DB[2]={1.5,4.0};
  static const int dimm[]={0x31,0x33,0x11,0x21,0x13};
  for(int m=0;m<5;m++){
    double out[2];
#if defined(__x86_64__)
    __m128d va=_mm_loadu_pd(DA), vb=_mm_loadu_pd(DB), rr;
    switch(dimm[m]){case 0x31:rr=_mm_dp_pd(va,vb,0x31);break;case 0x33:rr=_mm_dp_pd(va,vb,0x33);break;
      case 0x11:rr=_mm_dp_pd(va,vb,0x11);break;case 0x21:rr=_mm_dp_pd(va,vb,0x21);break;default:rr=_mm_dp_pd(va,vb,0x13);}
    _mm_storeu_pd(out,rr);
#else
    double s=dppd(DA,DB,dimm[m]);
    out[0]=(dimm[m]&1)?s:0.0; out[1]=(dimm[m]&2)?s:0.0;
#endif
    h=mix(h,b64(out[0])); h=mix(h,b64(out[1]));
  }
  printf("sse41x=%016llx\n",(unsigned long long)h);
  return 0;
}
